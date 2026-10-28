//! Application state: the store, the orchestrator, and the in-flight turns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use ws_context::{BackendId, ConvId, ModelCapabilities};
use ws_core::{Orchestrator, Profile, ProfileTable};
use ws_engine::{LocalBackend, backend::LOCAL_BACKEND_ID};
use ws_mcp::{McpConfig, McpTools, ServerStatus};
use ws_providers::{AnthropicBackend, CompatBackend, CompatConfig, GeminiBackend, OpenAiBackend};
use ws_store::{BlobStore, Store};

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
    /// `SKILL.md` packs found under `skills/` in the app data folder.
    pub skills: Arc<ws_core::Skills>,
    pub skills_dir: std::path::PathBuf,
    /// The Understand-Anything viewer process, while its window is open.
    pub viewer: crate::graph::Viewer,
    /// The open folder and file, for the agent's built-in tools.
    pub workspace: Arc<crate::workspace::Workspace>,
    /// The program running in the output panel, if any.
    pub runner: crate::run::Runner,
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

        let store = Store::open(dir.join("ws.db"))?;

        // Master spec s10: a reply interrupted by a crash becomes a Failed
        // message keeping its partial text. Run before anything can write.
        match store.recover_drafts() {
            Ok(ids) if !ids.is_empty() => {
                tracing::info!(count = ids.len(), "recovered interrupted replies");
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "draft recovery failed"),
        }

        let store = Arc::new(Mutex::new(store));
        let blobs = Arc::new(BlobStore::new(dir.join("blobs"))?);
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
        let pipeline = ua_pipeline_dir();
        if pipeline.is_none() {
            tracing::warn!("Understand-Anything pipeline not found; /understand is unavailable");
        }
        let skills = ws_core::Skills::load(&skills_dir);
        let skills = match &pipeline {
            Some(dir) => skills.and_load(&dir.join("skills")),
            None => skills,
        };
        let skills = Arc::new(skills.with_builtin(BUILTIN_SKILLS));
        let agents = Arc::new(
            pipeline
                .as_ref()
                .map(|dir| ws_core::Agents::load(&dir.join("agents")))
                .unwrap_or_default(),
        );
        let workspace = Arc::new(crate::workspace::Workspace::new(pipeline));

        let mut orchestrator = Orchestrator::new(store.clone(), blobs, profiles.clone())
            .with_agents(agents)
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
        // its own id and its own key (master spec s7.5).
        for cfg in load_compat_configs(&store) {
            orchestrator = orchestrator
                .with_backend(
                    Arc::new(CompatBackend::new(cfg, profiles.clone() as Arc<_>)) as Arc<_>,
                );
        }

        Ok(Self {
            orchestrator: Arc::new(orchestrator),
            store,
            profiles,
            local,
            skills,
            skills_dir,
            viewer: std::sync::Mutex::new(None),
            workspace,
            runner: crate::run::Runner::default(),
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
/// scripts), built by `scripts/build-ua-pipeline.mjs`. Installed beside the
/// exe; in a dev build, read from the source tree.
fn ua_pipeline_dir() -> Option<std::path::PathBuf> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("ua-pipeline")));
    let source_tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ua-pipeline");
    beside_exe
        .into_iter()
        .chain([source_tree])
        .find(|dir| dir.join("agents").is_dir())
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

/// MCP tools that arrive after launch.
///
/// Serena starts language servers before it answers, which can take seconds.
/// Connecting in the background keeps the window responsive; until it is
/// done, no tools are offered.
#[derive(Debug)]
pub struct LateTools {
    pub config_path: std::path::PathBuf,
    tools: std::sync::OnceLock<McpTools>,
    status: Mutex<Vec<ServerStatus>>,
}

impl LateTools {
    fn new(config_path: std::path::PathBuf) -> Self {
        Self {
            config_path,
            tools: std::sync::OnceLock::new(),
            status: Mutex::new(Vec::new()),
        }
    }

    async fn connect(&self) {
        let config = match std::fs::read_to_string(&self.config_path) {
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
            Err(_) => return, // no file: no tool servers configured
        };
        let (host, status) = McpTools::connect(&config).await;
        let _ = self.tools.set(host);
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
        self.tools
            .get()
            .map(ws_core::ToolHost::specs)
            .unwrap_or_default()
    }

    fn needs_approval(&self, name: &str) -> bool {
        self.tools.get().is_none_or(|t| t.needs_approval(name))
    }

    fn file_for(&self, name: &str, args: &serde_json::Value) -> Option<std::path::PathBuf> {
        self.tools.get().and_then(|t| t.file_for(name, args))
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> ws_core::ToolOutput {
        match self.tools.get() {
            Some(t) => t.call(name, args).await,
            None => ws_core::ToolOutput::error("tool servers are still starting"),
        }
    }
}
