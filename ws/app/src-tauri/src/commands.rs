//! Tauri commands: the whole surface the UI can reach.
//!
//! Every command is a thin translation into a `ws-core` call. Errors come back
//! as strings because that is all a webview can render; the typed errors stay
//! on the Rust side where they can be matched on.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tauri::State;
use tauri::ipc::Channel;
use ws_context::{
    ContextView, ConvId, ModelCapabilities, ModelRef, MsgId, MsgStatus, Part, Role, ThinkingMode,
};
use ws_core::{Profile, TurnEvent};
use ws_secrets::{Secret, SecretStore};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn conv_id(s: &str) -> CmdResult<ConvId> {
    ConvId::from_str(s).map_err(|_| format!("not a conversation id: {s}"))
}

fn msg_id(s: &str) -> CmdResult<MsgId> {
    MsgId::from_str(s).map_err(|_| format!("not a message id: {s}"))
}

// ---- wire shapes ------------------------------------------------------

/// A conversation as the sidebar needs it.
#[derive(Debug, Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub model_id: String,
}

/// A message as the thread renders it. Deliberately not the canonical
/// `Message`: the UI has no use for provider-native blocks, and shipping them
/// into a webview would be a way for signed payloads to leak into a log.
#[derive(Debug, Serialize)]
pub struct ThreadMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    /// Human-readable reasoning, when the model produced any.
    pub reasoning: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub model_id: Option<String>,
    /// How many siblings this message has, and which one it is. Drives the
    /// branch navigator; `total` is 1 when there is no branch here.
    pub branch_index: u32,
    pub branch_total: u32,
    /// The skill this message ran, shown collapsed rather than as text.
    pub skill: Option<SkillUseDto>,
    /// Tools an assistant message asked for.
    pub tool_calls: Vec<ToolCallDto>,
    /// What those tools returned, on a `tool` message.
    pub tool_results: Vec<ToolResultDto>,
}

#[derive(Debug, Serialize)]
pub struct SkillUseDto {
    pub name: String,
    pub instructions: String,
}

#[derive(Debug, Serialize)]
pub struct ToolCallDto {
    pub call_id: String,
    pub name: String,
    pub args: String,
}

#[derive(Debug, Serialize)]
pub struct ToolResultDto {
    pub call_id: String,
    pub text: String,
    pub is_error: bool,
    /// The file this call changed, if it changed one.
    pub edit: Option<EditDto>,
}

#[derive(Debug, Serialize)]
pub struct EditDto {
    pub path: String,
    pub diff: String,
    pub undone: bool,
}

#[derive(Debug, Serialize)]
pub struct Thread {
    pub conversation_id: String,
    pub title: String,
    pub backend: String,
    pub model_id: String,
    pub messages: Vec<ThreadMessage>,
}

#[derive(Debug, Serialize)]
pub struct ModelOption {
    pub backend: String,
    pub model_id: String,
    pub display_name: String,
    /// True while this model still has only guessed capabilities, so the UI
    /// can warn (master spec s7.1).
    pub unprofiled: bool,
}

#[derive(Debug, Serialize)]
pub struct BackendInfo {
    pub id: String,
    /// True for the three first-party adapters, which cannot be deleted.
    pub built_in: bool,
    pub has_key: bool,
    /// False for the local engine, which authenticates to nothing.
    pub needs_key: bool,
}

#[derive(Debug, Serialize)]
pub struct CompatProviderDto {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileDto {
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub vision: bool,
    pub thinking: String,
    pub thinking_budget: Option<u32>,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn status_parts(status: &MsgStatus) -> (&'static str, Option<String>) {
    match status {
        MsgStatus::Complete => ("complete", None),
        MsgStatus::Cancelled => ("cancelled", None),
        MsgStatus::Failed { reason } => ("failed", Some(reason.clone())),
    }
}

// ---- conversations ----------------------------------------------------

#[tauri::command]
pub fn list_conversations(state: State<'_, AppState>) -> CmdResult<Vec<ConversationSummary>> {
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    let list = store.list_conversations().map_err(err)?;
    Ok(list
        .into_iter()
        .map(|c| ConversationSummary {
            id: c.id.to_string(),
            title: c.title,
            backend: c.current_model.backend.to_string(),
            model_id: c.current_model.model_id,
        })
        .collect())
}

#[tauri::command]
pub fn create_conversation(
    state: State<'_, AppState>,
    title: String,
    backend: String,
    model_id: String,
) -> CmdResult<String> {
    let model = ModelRef::new(backend, model_id);
    let id = state
        .orchestrator
        .create_conversation(&title, model)
        .map_err(err)?;
    Ok(id.to_string())
}

#[tauri::command]
pub fn rename_conversation(
    state: State<'_, AppState>,
    conversation: String,
    title: String,
) -> CmdResult<()> {
    let id = conv_id(&conversation)?;
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store.rename_conversation(id, &title).map_err(err)
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, AppState>, conversation: String) -> CmdResult<()> {
    let id = conv_id(&conversation)?;
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store.delete_conversation(id).map_err(err)
}

#[tauri::command]
pub fn load_thread(state: State<'_, AppState>, conversation: String) -> CmdResult<Thread> {
    let id = conv_id(&conversation)?;
    // Read before locking the store below: this takes the same lock.
    let mut edits: std::collections::HashMap<String, ws_core::EditView> = state
        .orchestrator
        .file_edits(id)
        .map_err(err)?
        .into_iter()
        .map(|e| (e.call_id.clone(), e))
        .collect();
    let store = state.store.lock().map_err(|_| "store unavailable")?;

    let conv = store
        .get_conversation(id)
        .map_err(err)?
        .ok_or("conversation not found")?;
    let path = store.load_active_path(id).map_err(err)?;

    let mut messages = Vec::with_capacity(path.len());
    for m in &path {
        // Sibling counts drive the "< 2/3 >" navigator.
        let siblings = store.siblings(m.id).map_err(err)?;
        let index = siblings
            .iter()
            .position(|s| s.id == m.id)
            .map_or(0, |i| u32::try_from(i).unwrap_or(0));
        let (status, failure_reason) = status_parts(&m.status);

        let reasoning = m.parts.iter().find_map(|p| match p {
            Part::Reasoning { visible_text } => visible_text.clone(),
            _ => None,
        });

        let skill = m
            .parts
            .first()
            .and_then(Part::as_text)
            .and_then(ws_core::skills::unwrap_envelope);

        messages.push(ThreadMessage {
            id: m.id.to_string(),
            role: role_str(m.role).to_owned(),
            skill: skill.map(|(name, instructions)| SkillUseDto {
                name: name.to_owned(),
                instructions: instructions.to_owned(),
            }),
            text: if skill.is_some() {
                m.parts.iter().skip(1).filter_map(Part::as_text).collect()
            } else {
                m.text_content()
            },
            reasoning,
            status: status.to_owned(),
            failure_reason,
            model_id: m.author_model.as_ref().map(|a| a.model_id.clone()),
            branch_index: index + 1,
            branch_total: u32::try_from(siblings.len()).unwrap_or(1).max(1),
            tool_calls: m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::ToolCall {
                        call_id,
                        name,
                        args_json,
                    } => Some(ToolCallDto {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args: args_json.clone(),
                    }),
                    _ => None,
                })
                .collect(),
            tool_results: m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } => Some(ToolResultDto {
                        call_id: call_id.clone(),
                        text: content.iter().filter_map(Part::as_text).collect(),
                        is_error: *is_error,
                        edit: edits.remove(call_id).map(|e| EditDto {
                            path: e.path,
                            diff: e.diff,
                            undone: e.undone,
                        }),
                    }),
                    _ => None,
                })
                .collect(),
        });
    }

    Ok(Thread {
        conversation_id: id.to_string(),
        title: conv.title,
        backend: conv.current_model.backend.to_string(),
        model_id: conv.current_model.model_id,
        messages,
    })
}

#[tauri::command]
pub fn siblings_of(state: State<'_, AppState>, message: String) -> CmdResult<Vec<String>> {
    let id = msg_id(&message)?;
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    Ok(store
        .siblings(id)
        .map_err(err)?
        .into_iter()
        .map(|m| m.id.to_string())
        .collect())
}

/// Move the visible thread to a different branch.
#[tauri::command]
pub fn select_branch(
    state: State<'_, AppState>,
    conversation: String,
    message: String,
) -> CmdResult<()> {
    let conv = conv_id(&conversation)?;
    let leaf = msg_id(&message)?;
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store.set_active_leaf(conv, Some(leaf)).map_err(err)
}

// ---- turns ------------------------------------------------------------

/// Send a user message and stream the reply over `on_event`.
///
/// A channel per turn rather than a global event name, so two conversations
/// streaming at once cannot interleave.
#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    conversation: String,
    text: String,
    on_event: Channel<TurnEvent>,
) -> CmdResult<()> {
    let conv = conv_id(&conversation)?;
    // `/name request` runs a skill: its instructions travel with the message.
    let parts = state
        .skills
        .expand(&text)
        .unwrap_or_else(|| vec![Part::text(text)]);
    state
        .orchestrator
        .append_user_message(conv, parts)
        .map_err(err)?;
    stream_turn(&state, conv, on_event).await
}

/// Regenerate an assistant message as a new sibling (master spec s5.1).
#[tauri::command]
pub async fn regenerate(
    state: State<'_, AppState>,
    conversation: String,
    message: String,
    on_event: Channel<TurnEvent>,
) -> CmdResult<()> {
    let conv = conv_id(&conversation)?;
    let msg = msg_id(&message)?;
    state
        .orchestrator
        .prepare_regenerate(conv, msg)
        .map_err(err)?;
    stream_turn(&state, conv, on_event).await
}

async fn stream_turn(
    state: &State<'_, AppState>,
    conv: ConvId,
    on_event: Channel<TurnEvent>,
) -> CmdResult<()> {
    let cancel = state.begin_turn(conv);
    let orchestrator = state.orchestrator.clone();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(256);
    let forward = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            // A send failure means the window is gone; the turn still finishes
            // and persists, so nothing is lost.
            if on_event.send(event).is_err() {
                break;
            }
        }
    });

    let result = orchestrator.run_turn(conv, tx, cancel).await;
    let _ = forward.await;
    state.end_turn(conv);

    result.map(|_| ()).map_err(err)
}

/// Stop the turn streaming in this conversation. Partial text is kept.
#[tauri::command]
pub fn cancel_turn(state: State<'_, AppState>, conversation: String) -> CmdResult<()> {
    let conv = conv_id(&conversation)?;
    state.cancel(conv);
    Ok(())
}

#[tauri::command]
pub fn switch_model(
    state: State<'_, AppState>,
    conversation: String,
    backend: String,
    model_id: String,
) -> CmdResult<()> {
    let conv = conv_id(&conversation)?;
    state
        .orchestrator
        .switch_model(conv, ModelRef::new(backend, model_id))
        .map_err(err)
}

#[tauri::command]
pub fn get_context_view(
    state: State<'_, AppState>,
    message: String,
) -> CmdResult<Option<ContextView>> {
    let id = msg_id(&message)?;
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store.get_context_view(id).map_err(err)
}

// ---- models and profiles ----------------------------------------------

/// Model ids come from the providers, merged with anything already profiled.
/// Never a hardcoded list (master spec s7.1).
///
/// One provider being unreachable or unconfigured must not empty the picker,
/// so failures are logged per backend and the rest still render.
#[tauri::command]
pub async fn list_models(state: State<'_, AppState>) -> CmdResult<Vec<ModelOption>> {
    let profiles = state.profiles.clone();
    let mut options: Vec<ModelOption> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Profiled models are offered even when every provider is unreachable, so
    // an offline user still sees their own configuration.
    // Not from a provider with no key, though: those can only fail.
    let secrets = SecretStore::new();
    let usable = |backend: &ws_context::BackendId| {
        backend.as_str() == crate::state::LOCAL
            || secrets.has_key(backend.as_str()).unwrap_or(false)
    };
    if let Ok(guard) = state.store.lock() {
        if let Ok(rows) = guard.list_model_profiles() {
            for row in rows.into_iter().filter(|r| usable(&r.model.backend)) {
                if seen.insert(row.model.clone()) {
                    options.push(ModelOption {
                        backend: row.model.backend.to_string(),
                        display_name: row.model.model_id.clone(),
                        model_id: row.model.model_id,
                        unprofiled: !row.user_edited,
                    });
                }
            }
        }
    }

    for (backend, result) in state.orchestrator.list_all_models().await {
        match result {
            Ok(models) => {
                for info in models {
                    // A provider that reports its own limits pre-fills the
                    // profile, so the user is not asked to guess (Gemini does).
                    if let Some(caps) = &info.known_capabilities {
                        if profiles.is_unprofiled(&info.model) {
                            profiles.set(
                                info.model.clone(),
                                Profile {
                                    capabilities: caps.clone(),
                                    thinking: ThinkingMode::Off,
                                    // Still not user-edited: reported limits
                                    // are better than defaults, not confirmed.
                                    user_edited: false,
                                },
                            );
                        }
                    }
                    if seen.insert(info.model.clone()) {
                        // "Unprofiled" means the numbers are conservative
                        // guesses the user should correct. The local engine's
                        // are measured from the file, so the warning would be
                        // false there.
                        let local = info.model.backend.as_str() == crate::state::LOCAL;
                        options.push(ModelOption {
                            backend: info.model.backend.to_string(),
                            model_id: info.model.model_id.clone(),
                            display_name: info.display_name,
                            unprofiled: !local && profiles.is_unprofiled(&info.model),
                        });
                    }
                }
            }
            Err(e) => tracing::debug!(%backend, error = %e, "no model list from this backend"),
        }
    }

    options.sort_by(|a, b| (&a.backend, &a.model_id).cmp(&(&b.backend, &b.model_id)));
    Ok(options)
}

/// The backends the app knows about, for grouping the picker and for the
/// settings panel to show which have keys.
#[tauri::command]
pub fn list_backends(state: State<'_, AppState>) -> CmdResult<Vec<BackendInfo>> {
    let secrets = SecretStore::new();
    let mut out = Vec::new();

    for id in state.orchestrator.backend_ids() {
        let built_in = crate::state::BUILT_IN.contains(&id.as_str());
        // The local engine has no account and no endpoint, so offering it a
        // key field would just be a box that can never be right.
        let needs_key = id.as_str() != crate::state::LOCAL;
        out.push(BackendInfo {
            has_key: needs_key && secrets.has_key(id.as_str()).unwrap_or(false),
            id: id.to_string(),
            built_in,
            needs_key,
        });
    }
    Ok(out)
}

/// Add or update an OpenAI-compatible endpoint (master spec s7.5).
///
/// Takes effect on the next launch: backends are wired up at startup.
#[tauri::command]
pub fn save_compat_provider(
    state: State<'_, AppState>,
    id: String,
    display_name: String,
    base_url: String,
    extra_headers: std::collections::BTreeMap<String, String>,
) -> CmdResult<()> {
    if id.trim().is_empty() || base_url.trim().is_empty() {
        return Err("an endpoint needs an id and a base URL".to_owned());
    }
    // A built-in id would shadow one of the first-party adapters.
    if crate::state::BUILT_IN.contains(&id.as_str()) {
        return Err(format!("{id} is a built-in backend; choose another id"));
    }

    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store
        .save_provider(&ws_store::StoredProvider {
            id: ws_context::BackendId::new(id),
            kind: "openai_compat".to_owned(),
            display_name,
            base_url: Some(base_url),
            extra_headers,
            enabled: true,
        })
        .map_err(err)
}

#[tauri::command]
pub fn list_compat_providers(state: State<'_, AppState>) -> CmdResult<Vec<CompatProviderDto>> {
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    Ok(store
        .list_providers()
        .map_err(err)?
        .into_iter()
        .map(|p| CompatProviderDto {
            id: p.id.to_string(),
            display_name: p.display_name,
            base_url: p.base_url.unwrap_or_default(),
            enabled: p.enabled,
        })
        .collect())
}

#[tauri::command]
pub fn delete_compat_provider(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let store = state.store.lock().map_err(|_| "store unavailable")?;
    store
        .delete_provider(&ws_context::BackendId::new(id))
        .map_err(err)
}

#[tauri::command]
pub fn get_model_profile(
    state: State<'_, AppState>,
    backend: String,
    model_id: String,
) -> CmdResult<ProfileDto> {
    let model = ModelRef::new(backend, model_id);
    let p = state.profiles.get(&model);
    let (thinking, budget) = match p.thinking {
        ThinkingMode::Off => ("off", None),
        ThinkingMode::Adaptive => ("adaptive", None),
        ThinkingMode::Enabled { budget_tokens } => ("enabled", Some(budget_tokens)),
    };
    Ok(ProfileDto {
        context_window: p.capabilities.context_window,
        max_output_tokens: p.capabilities.max_output_tokens,
        vision: p.capabilities.vision,
        thinking: thinking.to_owned(),
        thinking_budget: budget,
    })
}

#[tauri::command]
pub fn set_model_profile(
    state: State<'_, AppState>,
    backend: String,
    model_id: String,
    profile: ProfileDto,
) -> CmdResult<()> {
    let model = ModelRef::new(backend, model_id);
    let thinking = match profile.thinking.as_str() {
        "adaptive" => ThinkingMode::Adaptive,
        "enabled" => ThinkingMode::Enabled {
            budget_tokens: profile.thinking_budget.unwrap_or(4096),
        },
        _ => ThinkingMode::Off,
    };
    let capabilities = ModelCapabilities {
        context_window: profile.context_window,
        max_output_tokens: profile.max_output_tokens,
        vision: profile.vision,
        ..ModelCapabilities::default()
    };

    let caps_json = serde_json::to_string(&capabilities).map_err(err)?;
    let thinking_json = serde_json::to_string(&thinking).map_err(err)?;
    {
        let store = state.store.lock().map_err(|_| "store unavailable")?;
        store
            .save_model_profile(&model, &caps_json, Some(&thinking_json), true)
            .map_err(err)?;
    }
    state.profiles.set(
        model,
        Profile {
            capabilities,
            thinking,
            user_edited: true,
        },
    );
    Ok(())
}

// ---- secrets ----------------------------------------------------------
//
// Master spec s12: the UI can set, replace, delete and check for a key, but
// there is deliberately no command to read one back.

#[tauri::command]
pub fn set_api_key(backend: String, key: String) -> CmdResult<()> {
    SecretStore::new()
        .set_key(&backend, &Secret::new(key))
        .map_err(err)
}

#[tauri::command]
pub fn has_api_key(backend: String) -> CmdResult<bool> {
    SecretStore::new().has_key(&backend).map_err(err)
}

#[tauri::command]
pub fn delete_api_key(backend: String) -> CmdResult<()> {
    SecretStore::new().delete_key(&backend).map_err(err)
}

// ---- the local engine (master spec s8, s11.1) -------------------------

/// What a GGUF says about itself, before anything is loaded.
#[derive(Debug, Serialize)]
pub struct LocalModelInfo {
    pub path: String,
    pub file_name: String,
    pub architecture: String,
    pub supported: bool,
    /// Why not, when `supported` is false.
    pub unsupported_reason: Option<String>,
    pub quant_summary: String,
    pub context_length: Option<u32>,
    pub has_chat_template: bool,
    /// Set when the file needs RoPE scaling this build does not apply.
    pub ignored_rope_scaling: Option<String>,
    pub file_size_bytes: u64,
    /// Estimated memory at the default context cap, and whether it fits now.
    pub estimated_bytes: u64,
    pub available_bytes: u64,
    pub fits: bool,
}

/// What is loaded right now.
#[derive(Debug, Serialize)]
pub struct LoadedModelDto {
    pub path: String,
    pub file_name: String,
    pub context_cap: u32,
    pub model_context_length: Option<u32>,
    pub vocab_size: usize,
    pub quant_summary: String,
    pub threads: usize,
    pub weights_bytes: u64,
    pub kv_cache_bytes: u64,
    /// True when a shared prefix can be partially reused (master spec s8.5).
    /// False means editing an earlier turn re-reads the prompt.
    pub truncatable_cache: bool,
    /// The largest context cap this build can address.
    pub max_context_cap: u32,
    /// Set when the loaded file needs RoPE scaling this build does not apply.
    pub ignored_rope_scaling: Option<String>,
}

fn to_dto(info: &ws_engine::LoadedInfo) -> LoadedModelDto {
    LoadedModelDto {
        path: info.path.display().to_string(),
        file_name: file_name_of(&info.path),
        context_cap: info.context_cap,
        model_context_length: info.model_context_length,
        vocab_size: info.vocab_size,
        quant_summary: info.quant_summary.clone(),
        threads: info.threads,
        weights_bytes: info.memory.weights_bytes,
        kv_cache_bytes: info.memory.kv_cache_bytes,
        truncatable_cache: info.truncatable_cache,
        max_context_cap: ws_engine::REFERENCE_MAX_POSITIONS,
        ignored_rope_scaling: info.ignored_rope_scaling.clone(),
    }
}

fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Read a GGUF header without loading it.
///
/// Separate from `load_local_model` on purpose: the user should be able to see
/// what a file is, and whether it will fit, before committing to a load that
/// takes seconds and gigabytes.
#[tauri::command]
pub async fn inspect_local_model(
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<LocalModelInfo> {
    let path = std::path::PathBuf::from(path);
    let info = state.local.engine().inspect(path).await.map_err(err)?;

    let supported = ws_engine::gguf::require_supported(&info);
    let estimate = ws_engine::memory::estimate(
        &info,
        ws_engine::DEFAULT_CONTEXT_CAP,
        ws_engine::KvDType::default(),
    );
    let available = ws_engine::memory::available_memory();

    Ok(LocalModelInfo {
        file_name: file_name_of(&info.path),
        path: info.path.display().to_string(),
        architecture: info.architecture.clone(),
        supported: supported.is_ok(),
        unsupported_reason: supported.err().map(|e| e.to_string()),
        quant_summary: info.quant_summary.clone(),
        context_length: info.context_length,
        has_chat_template: info.has_chat_template,
        ignored_rope_scaling: info.rope_scaling.as_ref().map(ToString::to_string),
        file_size_bytes: info.file_size,
        estimated_bytes: estimate.total_bytes,
        available_bytes: available,
        fits: estimate.total_bytes <= available,
    })
}

/// Load a GGUF. Replaces whatever was loaded before.
#[tauri::command]
pub async fn load_local_model(
    state: State<'_, AppState>,
    path: String,
    context_cap: Option<u32>,
) -> CmdResult<LoadedModelDto> {
    let opts = ws_engine::LoadOptions {
        context_cap: context_cap.unwrap_or(ws_engine::DEFAULT_CONTEXT_CAP),
        ..ws_engine::LoadOptions::default()
    };
    let info = state
        .local
        .load(std::path::PathBuf::from(path), opts)
        .await
        .map_err(err)?;

    // The engine knows its own window exactly, so record it as a profile
    // rather than leaving the model "unprofiled" with conservative defaults
    // (master spec s7.1).
    let model = ModelRef::new(crate::state::LOCAL, file_name_of(&info.path));
    state.profiles.set(
        model,
        Profile {
            capabilities: ModelCapabilities {
                context_window: info.context_cap,
                token_counting: ws_context::TokenCounting::Exact,
                ..ModelCapabilities::for_window(info.context_cap)
            },
            thinking: ThinkingMode::Off,
            user_edited: false,
        },
    );

    Ok(to_dto(&info))
}

#[tauri::command]
pub async fn unload_local_model(state: State<'_, AppState>) -> CmdResult<()> {
    state.local.unload().await.map_err(err)
}

#[tauri::command]
pub async fn local_model_status(state: State<'_, AppState>) -> CmdResult<Option<LoadedModelDto>> {
    Ok(state.local.loaded_info().as_ref().map(to_dto))
}

// ---- tools (MCP) ------------------------------------------------------

/// Answer a tool call waiting for approval. False if it is no longer waiting.
#[tauri::command]
pub fn resolve_approval(state: State<'_, AppState>, call_id: String, approved: bool) -> bool {
    state.orchestrator.resolve_approval(&call_id, approved)
}

#[derive(Debug, Serialize)]
pub struct McpStatusDto {
    /// Where to put the `mcpServers` config.
    pub config_path: String,
    pub servers: Vec<ws_mcp::ServerStatus>,
}

#[tauri::command]
pub fn mcp_status(state: State<'_, AppState>) -> McpStatusDto {
    McpStatusDto {
        config_path: state.mcp.config_path.display().to_string(),
        servers: state.mcp.status(),
    }
}

// ---- skills -------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SkillDto {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct SkillsDto {
    /// Where to put skill folders, e.g. a clone of the Ponytail repo.
    pub dir: String,
    pub skills: Vec<SkillDto>,
}

#[tauri::command]
pub fn list_skills(state: State<'_, AppState>) -> SkillsDto {
    SkillsDto {
        dir: state.skills_dir.display().to_string(),
        skills: state
            .skills
            .list()
            .iter()
            .map(|s| SkillDto {
                name: s.name.clone(),
                description: s.description.clone(),
            })
            .collect(),
    }
}

// ---- knowledge graph (Understand-Anything) ----------------------------------

/// Open the knowledge-graph dashboard for a project folder. Returns its URL.
#[tauri::command]
pub async fn open_graph(app: tauri::AppHandle, project: String) -> CmdResult<String> {
    crate::graph::open(&app, &project).await
}

#[tauri::command]
pub fn has_graph(project: String) -> bool {
    crate::graph::exists(&project)
}

/// Build Workspace's free, model-free graph of a project. `replace` overwrites a
/// graph built some other way (an error starting `EXISTS:` asks for it).
#[tauri::command]
pub async fn build_graph(project: String, replace: bool) -> CmdResult<crate::graph_builder::Built> {
    tauri::async_runtime::spawn_blocking(move || crate::graph_builder::write(&project, replace))
        .await
        .map_err(err)?
}

// ---- project files ---------------------------------------------------------

#[tauri::command]
pub fn list_dir(root: String, path: String) -> CmdResult<Vec<crate::files::Entry>> {
    crate::files::list_dir(&root, &path)
}

/// One question to a model outside any chat; nothing is stored. The blueprint
/// parts builder uses it to turn a description into a part.
#[tauri::command]
pub async fn ask_model(
    state: State<'_, AppState>,
    backend: String,
    model_id: String,
    system: String,
    prompt: String,
) -> CmdResult<String> {
    let model = ModelRef::new(backend, model_id);
    state
        .orchestrator
        .complete(
            &model,
            system,
            prompt,
            2048,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
}

#[tauri::command]
pub fn read_text_file(root: String, path: String) -> CmdResult<String> {
    crate::files::read_text(&root, &path)
}

#[tauri::command]
pub fn write_text_file(root: String, path: String, text: String) -> CmdResult<()> {
    crate::files::write_text(&root, &path, &text)
}

#[tauri::command]
pub fn rename_path(root: String, path: String, name: String) -> CmdResult<String> {
    crate::files::rename(&root, &path, &name)
}

/// To the Recycle Bin, not gone.
#[tauri::command]
pub fn delete_path(root: String, path: String) -> CmdResult<()> {
    crate::files::delete(&root, &path)
}

/// A shell in the output panel, in `root`, for pip, npm, cargo and the rest.
#[tauri::command]
pub fn open_terminal(
    state: State<'_, AppState>,
    root: String,
    on_event: Channel<crate::run::RunEvent>,
) -> CmdResult<()> {
    let root = std::path::Path::new(root.trim())
        .canonicalize()
        .map_err(err)?;
    state.runner.start(crate::run::terminal(&root)?, on_event)
}

/// `pip install` one package for the Python that runs `.py` files.
#[tauri::command]
pub fn pip_install(
    state: State<'_, AppState>,
    root: String,
    package: String,
    on_event: Channel<crate::run::RunEvent>,
) -> CmdResult<()> {
    let root = std::path::Path::new(root.trim())
        .canonicalize()
        .map_err(err)?;
    state
        .runner
        .start(crate::run::pip_install(&root, &package)?, on_event)
}

#[tauri::command]
pub fn create_dir(root: String, path: String) -> CmdResult<()> {
    crate::files::create_dir(&root, &path)
}

#[tauri::command]
pub fn create_file(root: String, path: String) -> CmdResult<()> {
    crate::files::create_file(&root, &path)
}

/// What the agent's tools act on: the open folder and the file in view.
#[tauri::command]
pub fn set_workspace(state: State<'_, AppState>, root: Option<String>, file: Option<String>) {
    state.workspace.set(root, file);
}

// ---- running code -------------------------------------------------------------

/// Compile and run one file in the output panel, stopping any earlier run.
#[tauri::command]
pub fn run_file(
    state: State<'_, AppState>,
    root: String,
    path: String,
    on_event: Channel<crate::run::RunEvent>,
) -> CmdResult<()> {
    let file = crate::files::resolve(&root, &path)?;
    let root = std::path::Path::new(root.trim())
        .canonicalize()
        .map_err(err)?;
    let plan = crate::run::plan(&root, &file)?;
    state.runner.start(plan, on_event)
}

#[tauri::command]
pub fn run_input(state: State<'_, AppState>, data: String) {
    state.runner.input(&data);
}

#[tauri::command]
pub fn run_resize(state: State<'_, AppState>, cols: u16, rows: u16) {
    state.runner.resize(cols, rows);
}

#[tauri::command]
pub fn stop_run(state: State<'_, AppState>) {
    state.runner.stop();
}

/// Put a file back as it was before an agent's edit.
#[tauri::command]
pub fn undo_edit(state: State<'_, AppState>, call_id: String) -> CmdResult<()> {
    state.orchestrator.undo_edit(&call_id).map_err(err)
}
