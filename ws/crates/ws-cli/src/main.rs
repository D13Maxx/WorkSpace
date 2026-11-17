//! `ws`: give the coordinator a task and it dispatches workers on a folder.
//!
//! Tier one is the model you talk to, told to plan and review rather than edit.
//! Tier two is the `worker` agents it dispatches; they run side by side, lock
//! each file while editing it, and can message one another.
//!
//! `--yes` approves file edits only; commands and other tools still ask unless
//! `--yes-all` is given.
//!
//! The coordinator keeps its plan and progress in `.ws/plan.md` in the folder;
//! the next run is given that plan, and `ws` with no task carries it on.
//!
//! A worker whose model fails moves to the next model in the fallback list
//! set in the app's Settings (`worker-fallbacks.json` in the data folder).
//!
//! usage: ws [--yes | --yes-all] [--dir FOLDER] [--model BACKEND/MODEL] [--worker-model BACKEND/MODEL] [--context TOKENS] [--max-output TOKENS] [--data-dir FOLDER] [TASK...]

// A binary: its own output is the point.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context as _, Result, bail};
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ws_context::{BackendId, ConvId, HelperEntry, ModelCapabilities, ModelInfo, ModelRef, Part};
use ws_core::{
    Agents, Orchestrator, PLAN_FILE, Profile, ProfileTable, ToolSet, TurnEvent, coordinator_message,
};
use ws_providers::{AnthropicBackend, CompatBackend, CompatConfig, GeminiBackend, OpenAiBackend};
use ws_store::{BlobStore, Store};

/// The desktop app's data folder, so keys, providers and profiles are shared.
fn default_data_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".local/share")))
        .unwrap_or_default();
    base.join("dev.debargha.friday")
}

// shortcut: same loading as the app's state.rs, upgrade by moving both into one shared place.
fn load_profiles(store: &Store, profiles: &ProfileTable) {
    for row in store.list_model_profiles().unwrap_or_default() {
        let capabilities =
            serde_json::from_str::<ModelCapabilities>(&row.caps_json).unwrap_or_default();
        let thinking = row
            .thinking_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        profiles.set(
            row.model,
            Profile {
                capabilities,
                thinking,
                user_edited: row.user_edited,
            },
        );
    }
}

fn compat_configs(store: &Store) -> Vec<CompatConfig> {
    store
        .list_providers()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.enabled && p.kind == "openai_compat")
        .filter_map(|p| {
            Some(CompatConfig {
                base_url: p.base_url.clone()?,
                id: p.id,
                display_name: p.display_name,
                extra_headers: p.extra_headers,
                requires_key: false,
            })
        })
        .collect()
}

/// Replace the profile of `target` with the limits its provider reports, if it
/// reports any, then apply the command-line overrides on top.
fn apply_limits(
    profiles: &ProfileTable,
    target: &ModelRef,
    listed: &[ModelInfo],
    context: Option<u32>,
    max_output: Option<u32>,
) {
    let reported = listed
        .iter()
        .find(|m| &m.model == target)
        .and_then(|m| m.known_capabilities.clone());
    let mut profile = profiles.get(target);
    if let Some(capabilities) = reported {
        profile.capabilities = capabilities;
    }
    profile.capabilities.context_window = context.unwrap_or(profile.capabilities.context_window);
    profile.capabilities.max_output_tokens =
        max_output.unwrap_or(profile.capabilities.max_output_tokens);
    profiles.set(target.clone(), profile);
}

struct Args {
    yes: bool,
    yes_all: bool,
    dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    model: Option<String>,
    /// What workers start on; else the app's worker model setting, else `model`.
    worker_model: Option<String>,
    context: Option<u32>,
    max_output: Option<u32>,
    task: String,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args> {
    let mut out = Args {
        yes: false,
        yes_all: false,
        dir: std::env::current_dir()?,
        data_dir: default_data_dir(),
        model: None,
        worker_model: None,
        context: None,
        max_output: None,
        task: String::new(),
    };
    let mut task = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--yes" => out.yes = true,
            "--yes-all" => out.yes_all = true,
            "--dir" => out.dir = args.next().context("--dir needs a folder")?.into(),
            "--data-dir" => out.data_dir = args.next().context("--data-dir needs a folder")?.into(),
            "--context" => {
                out.context = Some(
                    args.next()
                        .and_then(|v| v.parse().ok())
                        .context("--context needs a number of tokens")?,
                )
            }
            "--max-output" => {
                out.max_output = Some(
                    args.next()
                        .and_then(|v| v.parse().ok())
                        .context("--max-output needs a number of tokens")?,
                )
            }
            "--model" => out.model = Some(args.next().context("--model needs BACKEND/MODEL")?),
            "--worker-model" => {
                out.worker_model = Some(args.next().context("--worker-model needs BACKEND/MODEL")?)
            }
            _ => task.push(a),
        }
    }
    out.task = task.join(" ");
    Ok(out)
}

/// Whether a tool call goes ahead without asking: `--yes` covers the agent's
/// own file edits, which can be undone; anything that runs code needs `--yes-all`.
fn auto_approved(tool: &str, yes: bool, yes_all: bool) -> bool {
    yes_all || (yes && matches!(tool, "write_file" | "edit_file" | "create_folder"))
}

/// The coordinator and its workers act only through tools, so a model that
/// cannot carry them would run and quietly do nothing.
fn require_tools(orch: &Orchestrator, target: &ModelRef) -> Result<()> {
    if !orch.tools_enabled(target) {
        bail!(
            "{} cannot use tools here (only anthropic and OpenAI-compatible endpoints can), so the coordinator could not dispatch anything; pick another model",
            target.backend
        );
    }
    Ok(())
}

/// Ask on the terminal; anything but "y" declines.
async fn ask(name: &str, args: &str) -> bool {
    let shown: String = args.chars().take(300).collect();
    eprint!("\nallow {name} {shown} ? [y/N] ");
    let _ = std::io::stderr().flush();
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
    })
    .await
    .unwrap_or(false)
}

/// The agent's `choose_location` in a terminal: type a folder, or nothing to refuse.
async fn choose_folder(reason: String) -> Option<std::path::PathBuf> {
    eprint!(
        "
the agent wants a folder outside the project: {reason}
folder (empty to refuse): "
    );
    let _ = std::io::stderr().flush();
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        let line = line.trim().trim_matches('"');
        // The agent writes there by absolute path, so "out" means ./out.
        (!line.is_empty())
            .then(|| std::path::absolute(line).ok())
            .flatten()
    })
    .await
    .ok()
    .flatten()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ws_core=warn".into()),
        )
        .init();
    let cli = parse_args(std::env::args().skip(1))?;
    // shortcut: a plan file that cannot be read counts as none.
    let plan = std::fs::read_to_string(cli.dir.join(PLAN_FILE)).ok();
    if cli.task.trim().is_empty() && plan.is_none() {
        bail!(
            "usage: ws [--yes | --yes-all] [--dir FOLDER] [--model BACKEND/MODEL] [--worker-model BACKEND/MODEL] [--context TOKENS] [--max-output TOKENS] [--data-dir FOLDER] [TASK...]
(no task given, and no {PLAN_FILE} to continue)"
        );
    }

    std::fs::create_dir_all(&cli.data_dir)?;
    let store = Store::open(cli.data_dir.join("ws.db"))?;
    let blobs = Arc::new(BlobStore::new(cli.data_dir.join("blobs"))?);
    let profiles = Arc::new(ProfileTable::new());
    load_profiles(&store, &profiles);
    let configs = compat_configs(&store);

    // The model is `backend/model`, e.g. `anthropic/claude-sonnet-5-5`.
    let (backend, model) = cli
        .model
        .as_deref()
        .context("--model BACKEND/MODEL is needed, e.g. anthropic/claude-sonnet-5-5")?
        .split_once('/')
        .context("--model must look like BACKEND/MODEL")?;

    let workspace = Arc::new(ws_tools::workspace::Workspace::new(None).with_chooser(
        ws_tools::workspace::Chooser(Arc::new(|reason| Box::pin(choose_folder(reason)))),
    ));
    workspace.set(Some(cli.dir.to_string_lossy().into_owned()), None);

    let mut orch = Orchestrator::new(Arc::new(Mutex::new(store)), blobs, profiles.clone())
        .with_agents(Arc::new(Agents::default().with_worker()))
        .with_reconnect_waits(ws_core::RECONNECT_WAITS.to_vec())
        .with_tools(Arc::new(ToolSet::new(vec![workspace as Arc<_>])))
        .with_backend(Arc::new(AnthropicBackend::new(
            BackendId::new("anthropic"),
            profiles.clone() as Arc<_>,
        )) as Arc<_>)
        .with_backend(Arc::new(OpenAiBackend::new(
            BackendId::new("openai"),
            profiles.clone() as Arc<_>,
        )) as Arc<_>)
        .with_backend(Arc::new(GeminiBackend::new(
            BackendId::new("gemini"),
            profiles.clone() as Arc<_>,
        )) as Arc<_>);
    for cfg in configs {
        orch = orch
            .with_backend(Arc::new(CompatBackend::new(cfg, profiles.clone() as Arc<_>)) as Arc<_>);
    }
    // The same list the app's Settings edit; only workers use it.
    orch.set_worker_fallbacks(ws_core::load_worker_fallbacks(&cli.data_dir));
    let worker_model = match cli.worker_model.as_deref() {
        Some(m) => {
            let (b, m) = m
                .split_once('/')
                .context("--worker-model must look like BACKEND/MODEL")?;
            Some(ModelRef::new(b, m))
        }
        None => ws_core::load_worker_model(&cli.data_dir),
    };
    orch.set_worker_model(worker_model);
    orch.set_rules(ws_core::rules::load_rules(&cli.data_dir));
    let orch = Arc::new(orch);

    let target = ModelRef::new(backend, model);
    require_tools(&orch, &target)?;
    let mut listed = Vec::new();
    for (_, models) in orch.list_all_models().await {
        listed.extend(models.unwrap_or_default());
    }
    apply_limits(&profiles, &target, &listed, cli.context, cli.max_output);
    let conv = orch.create_conversation("ws", target)?;
    orch.append_user_message(
        conv,
        vec![Part::text(coordinator_message(&cli.task, plan.as_deref()))],
    )?;

    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    drive(&orch, conv, cli.yes, cli.yes_all, cancel).await
}

/// Run the turn, printing progress and answering approvals.
async fn drive(
    orch: &Arc<Orchestrator>,
    conv: ConvId,
    yes: bool,
    yes_all: bool,
    cancel: CancellationToken,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(256);
    let turn = {
        let orch = orch.clone();
        tokio::spawn(async move { orch.run_team_turn(conv, tx, cancel).await })
    };
    while let Some(event) = rx.recv().await {
        match event {
            TurnEvent::TextDelta { text } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            TurnEvent::ToolCall { name, args, .. } => {
                let short: String = args.chars().take(120).collect();
                eprintln!("\n> {name} {short}");
            }
            TurnEvent::ToolResult {
                is_error: true,
                text,
                ..
            } => {
                let short: String = text.chars().take(200).collect();
                eprintln!("  error: {short}");
            }
            TurnEvent::ApprovalRequired {
                call_id,
                name,
                args,
            } => {
                let approved = auto_approved(&name, yes, yes_all) || ask(&name, &args).await;
                orch.resolve_approval(&call_id, approved);
            }
            // Its steps already print as tool calls; this is what it says.
            TurnEvent::HelperStep {
                dispatch,
                agent,
                entry: HelperEntry::Text { text },
            } => eprintln!("\n[{agent} ({dispatch})] {}", text.trim()),
            // Briefs and reports already show as the calls and texts above.
            TurnEvent::Team { post, .. } if post.what == ws_context::TeamWhat::Note => eprintln!(
                "\n[team] {} ({}) to {}: {}",
                post.from,
                post.dispatch,
                post.to.as_deref().unwrap_or("everyone"),
                post.text.trim()
            ),
            TurnEvent::Question { call_id, text } => {
                eprint!("\nthe lead asks: {}\n> ", text.trim());
                let _ = std::io::stderr().flush();
                let answer = tokio::task::spawn_blocking(|| {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).map(|_| line)
                })
                .await;
                let answer = match answer {
                    Ok(Ok(line)) if !line.trim().is_empty() => line.trim().to_owned(),
                    // No one at the keyboard: the lead decides itself.
                    _ => "No answer; decide yourself.".to_owned(),
                };
                orch.answer_question(&call_id, answer);
            }
            TurnEvent::Failed { error, .. } => eprintln!("\nfailed: {error}"),
            _ => {}
        }
    }
    println!();
    turn.await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use ws_core::testing::{FakeBackend, Scripted};

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn flags_and_the_task_are_told_apart() {
        let a = parse_args(args(&[
            "--yes",
            "--model",
            "groq/x",
            "--worker-model",
            "groq/small",
            "--context",
            "9000",
            "add",
            "a",
            "readme",
        ]))
        .unwrap();
        assert!(a.yes);
        assert_eq!(a.model.as_deref(), Some("groq/x"));
        assert_eq!(a.worker_model.as_deref(), Some("groq/small"));
        assert_eq!(a.context, Some(9000));
        assert_eq!(a.task, "add a readme");
    }

    #[test]
    fn yes_covers_file_edits_but_not_commands() {
        assert!(auto_approved("write_file", true, false));
        assert!(auto_approved("edit_file", true, false));
        assert!(auto_approved("create_folder", true, false));
        assert!(
            !auto_approved("run_command", true, false),
            "commands still ask"
        );
        assert!(
            !auto_approved("write_file", false, false),
            "nothing without a flag"
        );
        assert!(
            auto_approved("run_command", false, true),
            "--yes-all covers everything"
        );
    }

    #[test]
    fn a_model_that_cannot_carry_tools_is_refused_before_anything_runs() {
        let profiles = Arc::new(ProfileTable::new());
        let (db, orch) = (tempfile::tempdir().unwrap(), |tools: bool| {
            let db = tempfile::tempdir().unwrap();
            let store = Arc::new(Mutex::new(Store::open(db.path().join("ws.db")).unwrap()));
            let blobs = Arc::new(BlobStore::new(db.path().join("blobs")).unwrap());
            let mut fake = FakeBackend::new("fake", profiles.clone() as Arc<_>);
            if !tools {
                fake = fake.without_tools();
            }
            let workspace = Arc::new(ws_tools::workspace::Workspace::new(None));
            (
                db,
                Orchestrator::new(store, blobs, profiles.clone())
                    .with_tools(Arc::new(ToolSet::new(vec![workspace as Arc<_>])))
                    .with_backend(Arc::new(fake) as Arc<_>),
            )
        });
        let _keep = db;
        let target = ModelRef::new("fake", "m");
        assert!(require_tools(&orch(true).1, &target).is_ok());
        let err = require_tools(&orch(false).1, &target)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot use tools"), "{err}");
        assert!(
            require_tools(&orch(true).1, &ModelRef::new("openai", "m")).is_err(),
            "unregistered backend"
        );
    }

    #[test]
    fn a_missing_task_or_bad_number_is_refused() {
        // No task is allowed here: it means continue the plan, which main checks for.
        assert!(parse_args(args(&["--yes"])).unwrap().task.is_empty());
        assert!(
            parse_args(args(&["--context", "lots", "task"])).is_err(),
            "not a number"
        );
        assert!(
            parse_args(args(&["task", "--model"])).is_err(),
            "flag without value"
        );
    }

    fn listed(model: &ModelRef, window: u32, out: u32) -> Vec<ModelInfo> {
        vec![ModelInfo {
            model: model.clone(),
            display_name: String::new(),
            known_capabilities: Some(ModelCapabilities {
                context_window: window,
                max_output_tokens: out,
                ..ModelCapabilities::default()
            }),
        }]
    }

    #[test]
    fn the_providers_numbers_replace_even_a_saved_profile() {
        let (profiles, target) = (ProfileTable::new(), ModelRef::new("or", "m"));
        let mut saved = profiles.get(&target);
        saved.capabilities.context_window = 1000;
        saved.user_edited = true;
        profiles.set(target.clone(), saved);

        apply_limits(
            &profiles,
            &target,
            &listed(&target, 200_000, 16_000),
            None,
            None,
        );
        let c = profiles.get(&target).capabilities;
        assert_eq!((c.context_window, c.max_output_tokens), (200_000, 16_000));
    }

    #[test]
    fn flags_beat_the_provider_and_an_unreported_model_keeps_its_profile() {
        let (profiles, target) = (ProfileTable::new(), ModelRef::new("or", "m"));
        apply_limits(
            &profiles,
            &target,
            &listed(&target, 200_000, 16_000),
            Some(50_000),
            None,
        );
        let c = profiles.get(&target).capabilities;
        assert_eq!((c.context_window, c.max_output_tokens), (50_000, 16_000));

        let other = ModelRef::new("or", "other");
        apply_limits(&profiles, &other, &listed(&target, 1, 1), None, None);
        assert_eq!(
            profiles.get(&other).capabilities,
            ModelCapabilities::default()
        );
    }

    /// A lead (fake-1) that may write first, starts a worker to write hi.txt,
    /// waits for it and finishes; the worker (fake-2) writes and reports. Each
    /// has its own script, since they run at the same time.
    fn team_script(first: Option<Scripted>, profiles: &Arc<ProfileTable>) -> FakeBackend {
        let backend = FakeBackend::new("fake", profiles.clone() as Arc<_>);
        let lead = |s| backend.script_for("fake-1", s);
        if let Some(s) = first {
            lead(s);
        }
        lead(Scripted::calls(
            "d1",
            "dispatch_agent",
            r#"{"agent":"worker","prompt":"write hi.txt"}"#,
        ));
        lead(Scripted::calls("t1", "wait_for_team", "{}"));
        lead(Scripted::Text(vec!["all done".to_owned()]));
        backend.script_for(
            "fake-2",
            Scripted::calls("w1", "write_file", r#"{"path":"hi.txt","content":"hi\n"}"#),
        );
        backend.script_for("fake-2", Scripted::Text(vec!["wrote hi.txt".to_owned()]));
        backend
    }

    #[tokio::test]
    async fn a_coordinator_run_writes_its_plan_and_has_a_worker_write_a_file() {
        let (db, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let store = Arc::new(Mutex::new(Store::open(db.path().join("ws.db")).unwrap()));
        let blobs = Arc::new(BlobStore::new(db.path().join("blobs")).unwrap());
        let profiles = Arc::new(ProfileTable::new());
        let backend = team_script(
            Some(Scripted::calls(
                "p1",
                "write_file",
                r#"{"path":".ws/plan.md","content":"- [ ] hi.txt\n"}"#,
            )),
            &profiles,
        );
        let workspace = Arc::new(ws_tools::workspace::Workspace::new(None));
        workspace.set(Some(folder.path().to_string_lossy().into_owned()), None);
        let orch = Arc::new(
            Orchestrator::new(store, blobs, profiles)
                .with_agents(Arc::new(Agents::default().with_worker()))
                .with_tools(Arc::new(ToolSet::new(vec![workspace as Arc<_>])))
                .with_backend(Arc::new(backend) as Arc<_>),
        );
        orch.set_worker_model(Some(ModelRef::new("fake", "fake-2")));
        let conv = orch
            .create_conversation("t", ModelRef::new("fake", "fake-1"))
            .unwrap();
        orch.append_user_message(
            conv,
            vec![Part::text(coordinator_message("make hi.txt", None))],
        )
        .unwrap();

        drive(&orch, conv, true, false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.path().join("hi.txt")).unwrap(),
            "hi\n"
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join(PLAN_FILE)).unwrap(),
            "- [ ] hi.txt\n",
            "the next run reads it from here"
        );
    }

    /// In a git project the worker works in its own copy: nothing reaches the
    /// project until its branch is merged.
    #[tokio::test]
    async fn in_a_git_project_a_workers_file_waits_on_its_branch_until_merged() {
        let git = |dir: &std::path::Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?}");
        };
        if ws_tools::find(&["git"]).is_none() {
            return;
        }
        let (db, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(folder.path().join("README.md"), "hello\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "commit.gpgsign", "false"],
            &["add", "."],
            &["commit", "-q", "-m", "first"],
        ] {
            git(folder.path(), args);
        }
        let store = Arc::new(Mutex::new(Store::open(db.path().join("ws.db")).unwrap()));
        let blobs = Arc::new(BlobStore::new(db.path().join("blobs")).unwrap());
        let profiles = Arc::new(ProfileTable::new());
        let backend = team_script(None, &profiles);
        let workspace = Arc::new(ws_tools::workspace::Workspace::new(None));
        let root = folder.path().to_string_lossy().into_owned();
        workspace.set(Some(root.clone()), None);
        let orch = Arc::new(
            Orchestrator::new(store.clone(), blobs, profiles)
                .with_agents(Arc::new(Agents::default().with_worker()))
                .with_tools(Arc::new(ToolSet::new(vec![workspace as Arc<_>])))
                .with_backend(Arc::new(backend) as Arc<_>),
        );
        orch.set_worker_model(Some(ModelRef::new("fake", "fake-2")));
        let conv = orch
            .create_conversation("t", ModelRef::new("fake", "fake-1"))
            .unwrap();
        orch.append_user_message(conv, vec![Part::text("make hi.txt")])
            .unwrap();
        // Approvals refused: the worker's file write must not need one.
        drive(&orch, conv, false, false, CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !folder.path().join("hi.txt").exists(),
            "not in the project yet"
        );
        let copy = store
            .lock()
            .unwrap()
            .list_events(conv)
            .unwrap()
            .into_iter()
            .flat_map(|e| match e.kind {
                ws_context::EventKind::HelperLog { entries, .. } => entries,
                _ => Vec::new(),
            })
            .find_map(|e| match e {
                HelperEntry::Isolated { branch, base, .. } => Some((branch, base)),
                _ => None,
            })
            .expect("the worker had its own copy");
        let changed = ws_tools::worktree::changes(&root, &copy.1, &copy.0)
            .unwrap()
            .unwrap();
        assert_eq!(changed[0].path, "hi.txt");

        ws_tools::worktree::merge(&root, &copy.1, &copy.0).unwrap();
        // Line endings follow the user's git settings (core.autocrlf), as a checkout's do.
        assert_eq!(
            std::fs::read_to_string(folder.path().join("hi.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "hi\n"
        );
    }

    fn provider(
        id: &str,
        kind: &str,
        url: Option<&str>,
        enabled: bool,
    ) -> ws_store::StoredProvider {
        ws_store::StoredProvider {
            id: BackendId::new(id),
            kind: kind.to_owned(),
            display_name: id.to_owned(),
            base_url: url.map(str::to_owned),
            extra_headers: Default::default(),
            enabled,
        }
    }

    #[test]
    fn only_enabled_compatible_endpoints_with_a_url_become_backends() {
        let store = Store::open_in_memory().unwrap();
        for p in [
            provider(
                "openrouter",
                "openai_compat",
                Some("https://openrouter.ai/api/v1"),
                true,
            ),
            provider("off", "openai_compat", Some("https://x/v1"), false),
            provider("nourl", "openai_compat", None, true),
            provider("other", "something_else", Some("https://y/v1"), true),
        ] {
            store.save_provider(&p).unwrap();
        }
        let got: Vec<_> = compat_configs(&store)
            .into_iter()
            .map(|c| c.id.to_string())
            .collect();
        assert_eq!(got, ["openrouter"]);
    }

    #[test]
    fn saved_profiles_are_loaded_and_a_malformed_one_falls_back_to_defaults() {
        let store = Store::open_in_memory().unwrap();
        let good = ModelRef::new("groq", "good");
        let caps = ModelCapabilities {
            context_window: 123_456,
            ..ModelCapabilities::default()
        };
        store
            .save_model_profile(&good, &serde_json::to_string(&caps).unwrap(), None, true)
            .unwrap();
        let bad = ModelRef::new("groq", "bad");
        store
            .save_model_profile(&bad, "{ not json", None, true)
            .unwrap();

        let profiles = ProfileTable::new();
        load_profiles(&store, &profiles);
        assert_eq!(profiles.get(&good).capabilities.context_window, 123_456);
        assert_eq!(
            profiles.get(&bad).capabilities,
            ModelCapabilities::default()
        );
    }
}
