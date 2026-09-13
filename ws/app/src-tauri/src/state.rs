//! Application state: the store, the orchestrator, and the in-flight turns.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio_util::sync::CancellationToken;
use ws_context::{BackendId, ConvId, ModelCapabilities};
use ws_core::{Orchestrator, Profile, ProfileTable};
use ws_providers::AnthropicBackend;
use ws_store::{BlobStore, Store};

/// The backend id for the built-in Anthropic adapter. Also the account name
/// its API key is stored under in the OS credential store.
pub const ANTHROPIC: &str = "anthropic";

pub struct AppState {
    pub orchestrator: Arc<Orchestrator>,
    pub store: Arc<Mutex<Store>>,
    pub profiles: Arc<ProfileTable>,
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

        let anthropic =
            AnthropicBackend::new(BackendId::new(ANTHROPIC), profiles.clone() as Arc<_>);
        let orchestrator = Orchestrator::new(store.clone(), blobs, profiles.clone())
            .with_backend(Arc::new(anthropic) as Arc<_>);

        Ok(Self {
            orchestrator: Arc::new(orchestrator),
            store,
            profiles,
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
