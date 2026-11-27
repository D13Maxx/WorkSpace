//! Application state: the store, the orchestrator, and the in-flight turns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use ws_context::{BackendId, ConvId, ModelCapabilities};
use ws_core::{Orchestrator, Profile, ProfileTable};
use ws_engine::{LocalBackend, backend::LOCAL_BACKEND_ID};
use ws_mcp::{CodeMode, McpConfig, McpTools, ServerStatus};
use ws_providers::{AnthropicBackend, CompatBackend, CompatConfig, GeminiBackend, OpenAiBackend};
use ws_store::Store;

/// Ids for the three built-in adapters. Each is also the account name its API
/// key is stored under in the OS credential store.
pub const ANTHROPIC: &str = "anthropic";
pub const OPENAI: &str = "openai";
pub const GEMINI: &str = "gemini";

/// The local engine's backend id. Not a provider: it has no key and no
/// endpoint, and it is listed separately in the picker.
pub const LOCAL: &str = LOCAL_BACKEND_ID;

/// The built-in backends, in the order the model picker should group them.
pub const BUILT_IN: [&str; 3] = [ANTHROPIC, OPENAI, GEMINI];

/// Skills shipped inside Workspace, so they work with nothing installed: all of
/// Ponytail, and the Understand-Anything skills that read an existing graph.
/// Both MIT; their licences sit beside them in `src-tauri/skills`.
/// shortcut: /understand itself (building a graph) is left out; it needs subagents and UA's Node packages.
/// The agent's `choose_location`: the system folder picker, titled with why.
fn folder_chooser(app: tauri::AppHandle) -> crate::workspace::Chooser {
    use tauri_plugin_dialog::DialogExt as _;
    crate::workspace::Chooser(Arc::new(move |reason| {
        let app = app.clone();
        Box::pin(async move {
            tauri::async_runtime::spawn_blocking(move || {
                app.dialog()
                    .file()
                    .set_title(format!("Choose a folder: {reason}"))
                    .blocking_pick_folder()
            })
            .await
            .ok()
            .flatten()
            .and_then(|f| f.into_path().ok())
        })
    }))
}

const BUILTIN_SKILLS: &[(&str, &str)] = &[
    (
        "built-in/ponytail/ponytail",
        include_str!("../skills/ponytail/ponytail/SKILL.md"),
    ),
    (
        "built-in/ponytail/ponytail-audit",
        include_str!("../skills/ponytail/ponytail-audit/SKILL.md"),
    ),
    (
        "built-in/ponytail/ponytail-debt",
        include_str!("../skills/ponytail/ponytail-debt/SKILL.md"),
    ),
    (
        "built-in/ponytail/ponytail-gain",
        include_str!("../skills/ponytail/ponytail-gain/SKILL.md"),
    ),
    (
        "built-in/ponytail/ponytail-help",
        include_str!("../skills/ponytail/ponytail-help/SKILL.md"),
    ),
    (
        "built-in/ponytail/ponytail-review",
        include_str!("../skills/ponytail/ponytail-review/SKILL.md"),
    ),
    (
        "built-in/understand-anything/understand-chat",
        include_str!("../skills/understand-anything/understand-chat/SKILL.md"),
    ),
    (
        "built-in/understand-anything/understand-diff",
        include_str!("../skills/understand-anything/understand-diff/SKILL.md"),
    ),
    (
        "built-in/understand-anything/understand-explain",
        include_str!("../skills/understand-anything/understand-explain/SKILL.md"),
    ),
    (
        "built-in/understand-anything/understand-onboard",
        include_str!("../skills/understand-anything/understand-onboard/SKILL.md"),
    ),
];

pub struct AppState {
    pub orchestrator: Arc<Orchestrator>,
    pub store: Arc<Mutex<Store>>,
    pub profiles: Arc<ProfileTable>,
    /// The local engine, reachable directly for load and unload. It is also
    /// registered as a backend, which is how a turn reaches it.
    pub local: Arc<LocalBackend>,
    /// MCP tool servers (Serena and friends), from `mcp.json`.
    pub mcp: Arc<LateTools>,
    /// `SKILL.md` packs found under `skills/` in the app data folder, the
    /// pipeline's and the built-in ones; rebuilt when Settings changes them.
    pub skills: Arc<LiveSkills>,
    /// The app data folder, where the worker fallback list lives.
    pub data_dir: std::path::PathBuf,
    /// The Understand-Anything viewer process, while its window is open.
    pub viewer: crate::graph::Viewer,
    /// The open folder and file, for the agent's built-in tools.
    pub workspace: Arc<crate::workspace::Workspace>,
    /// The program running in the output panel, if any.
    pub runner: crate::run::Runner,
    /// The shells in the terminal panel.
    pub terminals: crate::run::Terminals,
    /// Cancellation handles for turns currently streaming, by conversation.
    /// A conversation can have at most one turn in flight.
    running: Mutex<HashMap<ConvId, CancellationToken>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// Open the database, recover any interrupted reply, and wire up backends.
    ///
    /// # Errors
    /// Fails if the app data directory is unavailable or the database cannot
    /// be opened or migrated.
    pub fn initialise(app: &tauri::App) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&dir)?;

        // Encrypted unless the user turned it off; converted here if that changed.
        let (store, blobs) = ws_store::open_data(&dir)?;

        // A reply interrupted by a crash becomes a Failed
        // message keeping its partial text. Run before anything can write.
        match store.recover_drafts() {
            Ok(ids) if !ids.is_empty() => {
                tracing::info!(count = ids.len(), "recovered interrupted replies");
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "draft recovery failed"),
        }

        let store = Arc::new(Mutex::new(store));
        let blobs = Arc::new(blobs);
        let profiles = Arc::new(ProfileTable::new());
        load_profiles(&store, &profiles);

        // The engine worker starts with the app, but loads nothing until the
        // user picks a file: a worker thread costs nothing, a model costs
        // gigabytes.
        let local = Arc::new(LocalBackend::start(None)?);
        let mcp = Arc::new(LateTools::new(dir.join("mcp.json")));
        // Created up front so there is somewhere obvious to clone skills into.
        let skills_dir = dir.join("skills");
        std::fs::create_dir_all(&skills_dir)?;
        let pipeline = ua_pipeline_dir(app.path().resource_dir().ok());
        let rea = bundled(app.path().resource_dir().ok(), "rea", "scripts/rea.mjs");
        if rea.is_none() {
            tracing::warn!("Reverse Engineer Anything is not bundled; teams cannot use it");
        }
        if pipeline.is_none() {
            tracing::warn!("Understand-Anything pipeline not found; /understand is unavailable");
        }
        let skills = Arc::new(LiveSkills::new(skills_dir, pipeline.clone()));
        let agents = Arc::new(
            pipeline
                .as_ref()
                .map(|dir| ws_core::Agents::load(&dir.join("agents")))
                .unwrap_or_default()
                .with_worker(),
        );
        let workspace = Arc::new(
            crate::workspace::Workspace::new(pipeline)
                .with_chooser(folder_chooser(app.handle().clone())),
        );

        let mut orchestrator = Orchestrator::new(store.clone(), blobs, profiles.clone())
            .with_agents(agents)
            .with_team_tools(Arc::new(crate::rea::Rea::new(rea)))
            .with_reconnect_waits(ws_core::RECONNECT_WAITS.to_vec())
            .with_backend(local.clone() as Arc<_>)
            .with_tools(Arc::new(ws_core::ToolSet::new(vec![
                skills.clone() as Arc<_>,
                workspace.clone() as Arc<_>,
                mcp.clone() as Arc<_>,
            ])))
            .with_backend(Arc::new(AnthropicBackend::new(
                BackendId::new(ANTHROPIC),
                profiles.clone() as Arc<_>,
            )) as Arc<_>)
            .with_backend(Arc::new(OpenAiBackend::new(
                BackendId::new(OPENAI),
                profiles.clone() as Arc<_>,
            )) as Arc<_>)
            .with_backend(Arc::new(GeminiBackend::new(
                BackendId::new(GEMINI),
                profiles.clone() as Arc<_>,
            )) as Arc<_>);

        // Any number of OpenAI-compatible endpoints, each its own backend with
        // its own id and its own key.
        for cfg in load_compat_configs(&store) {
            orchestrator = orchestrator
                .with_backend(
                    Arc::new(CompatBackend::new(cfg, profiles.clone() as Arc<_>)) as Arc<_>,
                );
        }

        orchestrator.set_worker_fallbacks(ws_core::load_worker_fallbacks(&dir));
        orchestrator.set_role_fallbacks(ws_core::load_role_fallbacks(&dir));
        orchestrator.set_worker_model(ws_core::load_worker_model(&dir));
        orchestrator.set_role_models(ws_core::load_role_models(&dir));
        orchestrator.set_rules(ws_core::rules::load_rules(&dir));

        Ok(Self {
            orchestrator: Arc::new(orchestrator),
            data_dir: dir,
            store,
            profiles,
            local,
            skills,
            viewer: std::sync::Mutex::new(None),
            workspace,
            runner: crate::run::Runner::default(),
            terminals: crate::run::Terminals::default(),
            mcp: {
                let late = mcp.clone();
                tauri::async_runtime::spawn(async move { late.connect().await });
                mcp
            },
            running: Mutex::new(HashMap::new()),
        })
    }

    /// Register a turn as running, returning its cancellation token.
    /// Any turn already running for this conversation is cancelled first.
    pub fn begin_turn(&self, conv: ConvId) -> CancellationToken {
        let token = CancellationToken::new();
        if let Ok(mut map) = self.running.lock() {
            if let Some(previous) = map.insert(conv, token.clone()) {
                previous.cancel();
            }
        }
        token
    }

    /// Whether any turn is running, in any conversation.
    pub fn working(&self) -> bool {
        self.running.lock().is_ok_and(|m| !m.is_empty())
    }

    pub fn end_turn(&self, conv: ConvId) {
        if let Ok(mut map) = self.running.lock() {
            map.remove(&conv);
        }
    }

    /// Stop the turn streaming in `conv`, if any. Partial text is preserved by
    /// the orchestrator, not discarded here.
    pub fn cancel(&self, conv: ConvId) {
        if let Ok(map) = self.running.lock() {
            if let Some(token) = map.get(&conv) {
                token.cancel();
            }
        }
    }
}

/// Understand-Anything's /understand pipeline (skill, agents, bundled
/// scripts), built by `scripts/build-ua-pipeline.mjs`. An installer ships it
/// as a resource (beside the exe on Windows, under /usr/lib on Linux);
/// install.ps1 puts it beside the exe; a dev build reads the source tree.
fn ua_pipeline_dir(resources: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    bundled(resources, "ua-pipeline", "agents")
}

/// A bundled folder: among the installed resources, beside the exe (install.ps1
/// copies it there), or in the source tree for `tauri dev`. `marker` must be in it.
fn bundled(
    resources: Option<std::path::PathBuf>,
    name: &str,
    marker: &str,
) -> Option<std::path::PathBuf> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)));
    let source_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    resources
        .map(|r| r.join(name))
        .into_iter()
        .chain(beside_exe)
        .chain([source_tree])
        .find(|dir| dir.join(marker).exists())
}

/// Load saved model profiles into memory. Missing or malformed rows fall back
/// to conservative defaults rather than failing startup.
fn load_profiles(store: &Arc<Mutex<Store>>, profiles: &Arc<ProfileTable>) {
    let Ok(guard) = store.lock() else {
        return;
    };
    match guard.list_model_profiles() {
        Ok(rows) => {
            for row in rows {
                let capabilities = serde_json::from_str::<ModelCapabilities>(&row.caps_json)
                    .unwrap_or_else(|e| {
                        tracing::warn!(model = %row.model, error = %e, "ignoring malformed profile");
                        ModelCapabilities::default()
                    });
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
        Err(e) => tracing::error!(error = %e, "could not load model profiles"),
    }
}

/// Read the user-configured OpenAI-compatible endpoints. A row without a base
/// URL is unusable and is skipped rather than failing startup.
fn load_compat_configs(store: &Arc<Mutex<Store>>) -> Vec<CompatConfig> {
    let Ok(guard) = store.lock() else {
        return Vec::new();
    };
    match guard.list_providers() {
        Ok(rows) => rows
            .into_iter()
            .filter(|p| p.enabled && p.kind == "openai_compat")
            .filter_map(|p| {
                let base_url = p.base_url.clone().or_else(|| {
                    tracing::warn!(id = %p.id, "compatible endpoint has no base URL; skipping");
                    None
                })?;
                Some(CompatConfig {
                    id: p.id,
                    display_name: p.display_name,
                    base_url,
                    extra_headers: p.extra_headers,
                    // A local server usually needs none; the adapter treats a
                    // missing key as ordinary rather than as an error.
                    requires_key: false,
                })
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "could not load configured providers");
            Vec::new()
        }
    }
}

/// MCP tools that arrive after launch, offered in Code Mode: models search
/// and call every server's tools with scripts (`mcp_search`, `mcp_execute`)
/// rather than seeing each tool's schema.
///
/// Serena starts language servers before it answers, which can take seconds.
/// Connecting in the background keeps the window responsive; until it is
/// done, no tools are offered.
#[derive(Debug)]
pub struct LateTools {
    pub config_path: std::path::PathBuf,
    tools: std::sync::RwLock<Option<CodeMode>>,
    status: Mutex<Vec<ServerStatus>>,
}

impl LateTools {
    fn new(config_path: std::path::PathBuf) -> Self {
        Self {
            config_path,
            tools: std::sync::RwLock::new(None),
            status: Mutex::new(Vec::new()),
        }
    }

    /// Stop the running servers and start what `mcp.json` (and Settings) now
    /// list. A call already running finishes on the old ones.
    pub async fn reconnect(&self) {
        self.set_tools(None);
        self.set_status(Vec::new());
        self.connect().await;
    }

    fn set_tools(&self, tools: Option<CodeMode>) {
        if let Ok(mut t) = self.tools.write() {
            *t = tools;
        }
    }

    fn current(&self) -> Option<CodeMode> {
        self.tools.read().ok().and_then(|t| t.clone())
    }

    async fn connect(&self) {
        let mut config = match std::fs::read_to_string(&self.config_path) {
            Ok(text) => match McpConfig::parse(&text) {
                Ok(c) => c,
                Err(e) => {
                    // A broken file is shown in Settings, not swallowed.
                    self.set_status(vec![ServerStatus {
                        id: "mcp.json".to_owned(),
                        tools: 0,
                        error: Some(e.to_string()),
                    }]);
                    return;
                }
            },
            Err(_) => McpConfig::default(), // no file: only what Settings turned on
        };
        if let Some(dir) = self.config_path.parent() {
            if let Some(server) = crate::github::agent_server(dir) {
                config
                    .mcp_servers
                    .entry("github".to_owned())
                    .or_insert(server);
            }
        }
        if config.mcp_servers.is_empty() {
            return;
        }
        let (host, status) = McpTools::connect(&config).await;
        self.set_tools(Some(CodeMode::new(host)));
        self.set_status(status);
    }

    fn set_status(&self, status: Vec<ServerStatus>) {
        if let Ok(mut s) = self.status.lock() {
            *s = status;
        }
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl ws_core::ToolHost for LateTools {
    fn specs(&self) -> Vec<ws_context::ToolSpec> {
        self.current()
            .map(|t| ws_core::ToolHost::specs(&t))
            .unwrap_or_default()
    }

    fn needs_approval(&self, name: &str) -> bool {
        self.current().is_none_or(|t| t.needs_approval(name))
    }

    fn needs_approval_for(&self, name: &str, args: &serde_json::Value) -> bool {
        self.current()
            .is_none_or(|t| t.needs_approval_for(name, args))
    }

    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        self.current().and_then(|t| t.file_for(name, args))
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ws_core::ToolOutput {
        match self.current() {
            Some(t) => t.call(name, args).await,
            None => ws_core::ToolOutput::error("tool servers are still starting"),
        }
    }
}

/// The skills the agent sees: the user's folder, the pipeline's and the
/// built-in ones. Settings rebuilds them when a skill is added or removed.
#[derive(Debug)]
pub struct LiveSkills {
    pub dir: std::path::PathBuf,
    pipeline: Option<std::path::PathBuf>,
    now: std::sync::RwLock<Arc<ws_core::Skills>>,
}

impl LiveSkills {
    fn new(dir: std::path::PathBuf, pipeline: Option<std::path::PathBuf>) -> Self {
        let live = Self {
            dir,
            pipeline,
            now: std::sync::RwLock::default(),
        };
        live.reload();
        live
    }

    /// Look in the folders again.
    pub fn reload(&self) {
        let skills = ws_core::Skills::load(&self.dir);
        let skills = match &self.pipeline {
            Some(p) => skills.and_load(&p.join("skills")),
            None => skills,
        };
        if let Ok(mut now) = self.now.write() {
            *now = Arc::new(skills.with_builtin(BUILTIN_SKILLS));
        }
    }

    pub fn current(&self) -> Arc<ws_core::Skills> {
        self.now.read().map(|s| s.clone()).unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl ws_core::ToolHost for LiveSkills {
    fn specs(&self) -> Vec<ws_context::ToolSpec> {
        ws_core::ToolHost::specs(&*self.current())
    }

    fn needs_approval(&self, name: &str) -> bool {
        ws_core::ToolHost::needs_approval(&*self.current(), name)
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ws_core::ToolOutput {
        let skills = self.current();
        ws_core::ToolHost::call(&*skills, name, args).await
    }
}
