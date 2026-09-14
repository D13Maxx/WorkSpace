// The typed surface of the Rust backend.
//
// Every call goes through a Tauri command; the webview has no filesystem or
// network access of its own (master spec s11.2).

import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";

export interface ConversationSummary {
  id: string;
  title: string;
  backend: string;
  model_id: string;
}

export interface ThreadMessage {
  id: string;
  role: "system" | "user" | "assistant" | "tool";
  text: string;
  reasoning: string | null;
  status: "complete" | "cancelled" | "failed";
  failure_reason: string | null;
  model_id: string | null;
  branch_index: number;
  branch_total: number;
}

export interface Thread {
  conversation_id: string;
  title: string;
  backend: string;
  model_id: string;
  messages: ThreadMessage[];
}

export interface ModelOption {
  backend: string;
  model_id: string;
  display_name: string;
  unprofiled: boolean;
}

export interface BackendInfo {
  id: string;
  built_in: boolean;
  has_key: boolean;
  /** False for the local engine, which authenticates to nothing. */
  needs_key: boolean;
}

export interface CompatProvider {
  id: string;
  display_name: string;
  base_url: string;
  enabled: boolean;
}

export interface ModelProfile {
  context_window: number;
  max_output_tokens: number;
  vision: boolean;
  thinking: "off" | "adaptive" | "enabled";
  thinking_budget: number | null;
}

export interface ContextView {
  target: { backend: string; model_id: string };
  mode: "full" | "summary_plus_recent";
  summary: string | null;
  verbatim_messages: string[];
  transformations: unknown[];
  opaque_replayed: boolean;
  estimated_input_tokens: number;
}

/** Mirrors `ws_core::TurnEvent`. */
export type TurnEvent =
  | { type: "preparing_context"; target: { backend: string; model_id: string } }
  | { type: "summarising"; writer: { backend: string; model_id: string }; covering: number }
  | { type: "context_ready"; view: ContextView }
  | {
      type: "prefill_progress";
      /** Tokens the KV cache already held, which cost nothing to "read". */
      reused_tokens: number;
      done_tokens: number;
      total_tokens: number;
    }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
  | { type: "usage"; input_tokens: number; output_tokens: number }
  | { type: "completed"; message: string; stop_reason: unknown }
  | { type: "failed"; message: string | null; error: string };

/** A GGUF file, as read from its header without loading it. */
export interface LocalModelInfo {
  path: string;
  file_name: string;
  architecture: string;
  supported: boolean;
  unsupported_reason: string | null;
  quant_summary: string;
  context_length: number | null;
  has_chat_template: boolean;
  file_size_bytes: number;
  estimated_bytes: number;
  available_bytes: number;
  fits: boolean;
}

/** The model the local engine currently holds. */
export interface LoadedModel {
  path: string;
  file_name: string;
  context_cap: number;
  model_context_length: number | null;
  vocab_size: number;
  quant_summary: string;
  threads: number;
  weights_bytes: number;
  kv_cache_bytes: number;
  truncatable_cache: boolean;
  max_context_cap: number;
}

export const api = {
  listConversations: () => invoke<ConversationSummary[]>("list_conversations"),

  createConversation: (title: string, backend: string, modelId: string) =>
    invoke<string>("create_conversation", { title, backend, modelId }),

  renameConversation: (conversation: string, title: string) =>
    invoke<void>("rename_conversation", { conversation, title }),

  deleteConversation: (conversation: string) =>
    invoke<void>("delete_conversation", { conversation }),

  loadThread: (conversation: string) => invoke<Thread>("load_thread", { conversation }),

  siblingsOf: (message: string) => invoke<string[]>("siblings_of", { message }),

  selectBranch: (conversation: string, message: string) =>
    invoke<void>("select_branch", { conversation, message }),

  sendMessage: (conversation: string, text: string, onEvent: (e: TurnEvent) => void) => {
    const channel = new Channel<TurnEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("send_message", { conversation, text, onEvent: channel });
  },

  regenerate: (conversation: string, message: string, onEvent: (e: TurnEvent) => void) => {
    const channel = new Channel<TurnEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("regenerate", { conversation, message, onEvent: channel });
  },

  cancelTurn: (conversation: string) => invoke<void>("cancel_turn", { conversation }),

  switchModel: (conversation: string, backend: string, modelId: string) =>
    invoke<void>("switch_model", { conversation, backend, modelId }),

  listModels: () => invoke<ModelOption[]>("list_models"),

  listBackends: () => invoke<BackendInfo[]>("list_backends"),

  listCompatProviders: () => invoke<CompatProvider[]>("list_compat_providers"),

  saveCompatProvider: (
    id: string,
    displayName: string,
    baseUrl: string,
    extraHeaders: Record<string, string> = {},
  ) =>
    invoke<void>("save_compat_provider", {
      id,
      displayName,
      baseUrl,
      extraHeaders,
    }),

  deleteCompatProvider: (id: string) => invoke<void>("delete_compat_provider", { id }),

  getContextView: (message: string) => invoke<ContextView | null>("get_context_view", { message }),

  getModelProfile: (backend: string, modelId: string) =>
    invoke<ModelProfile>("get_model_profile", { backend, modelId }),

  setModelProfile: (backend: string, modelId: string, profile: ModelProfile) =>
    invoke<void>("set_model_profile", { backend, modelId, profile }),

  inspectLocalModel: (path: string) => invoke<LocalModelInfo>("inspect_local_model", { path }),

  loadLocalModel: (path: string, contextCap?: number) =>
    invoke<LoadedModel>("load_local_model", { path, contextCap: contextCap ?? null }),

  unloadLocalModel: () => invoke<void>("unload_local_model"),

  localModelStatus: () => invoke<LoadedModel | null>("local_model_status"),

  setApiKey: (backend: string, key: string) => invoke<void>("set_api_key", { backend, key }),
  hasApiKey: (backend: string) => invoke<boolean>("has_api_key", { backend }),
  deleteApiKey: (backend: string) => invoke<void>("delete_api_key", { backend }),
};
