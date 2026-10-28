// The typed surface of the Rust backend.
//
// Every call goes through a Tauri command; the webview has no filesystem or
// network access of its own (master spec s11.2).

import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import type { Sandbox } from "./blueprint/model";

/** Ask before something that cannot be taken back. The dialog plugin swaps
 *  `window.confirm` for an async one, so a plain `if (confirm(...))` is always
 *  true; this waits for the answer. */
export function ask(message: string): Promise<boolean> {
  return confirm(message, { title: "Workspace", kind: "warning" });
}

/** Windows' own folder picker. `null` if cancelled. */
export async function pickFolder(defaultPath?: string, title?: string): Promise<string | null> {
  const picked = await open({ directory: true, defaultPath, title });
  return typeof picked === "string" ? picked : null;
}

/** Windows' own file picker, limited to the given extensions. `null` if cancelled. */
/** A file chosen in the native picker. With no extensions, every file shows. */
export async function pickFile(label?: string, extensions?: string[]): Promise<string | null> {
  const picked = await open({
    filters: label && extensions ? [{ name: label, extensions }] : undefined,
  });
  return typeof picked === "string" ? picked : null;
}

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
  skill: { name: string; instructions: string } | null;
  tool_calls: { call_id: string; name: string; args: string }[];
  tool_results: {
    call_id: string;
    text: string;
    is_error: boolean;
    edit: { path: string; diff: string; undone: boolean } | null;
  }[];
}

export interface GraphBuilt {
  files: number;
  nodes: number;
  edges: number;
  layers: number;
  path: string;
}

/** The output panel's stream for one run. */
export type RunEvent =
  | { type: "output"; data: string }
  | { type: "exit"; code: number | null; stopped: boolean };

export interface FileEntry {
  name: string;
  /** Relative to the project root, with `/` separators. */
  path: string;
  is_dir: boolean;
}

export interface SkillList {
  dir: string;
  skills: { name: string; description: string }[];
}

export interface McpStatus {
  config_path: string;
  servers: { id: string; tools: number; error: string | null }[];
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

/** The model last picked in a chat. Work that starts without one (a new
 *  chat, Build with agent, the parts builder) uses it. Per viewer. */
const LAST_MODEL = "ws.lastModel";
export function rememberModel(backend: string, modelId: string) {
  try {
    localStorage.setItem(LAST_MODEL, JSON.stringify({ backend, modelId }));
  } catch {
    // Not remembered: the fallback picks instead.
  }
}

/** A Claude Sonnet if there is one, else any Claude, else the first model. */
export const claudeFirst = (models: ModelOption[]) =>
  models.find((m) => m.backend === "anthropic" && m.model_id.includes("sonnet")) ?? models.find((m) => m.backend === "anthropic") ?? models[0];

/** The model last picked, if it is still on offer; else `fallback`'s choice. */
export function preferredModel(models: ModelOption[], fallback: (models: ModelOption[]) => ModelOption | undefined) {
  try {
    const last = JSON.parse(localStorage.getItem(LAST_MODEL) ?? "null") as { backend?: string; modelId?: string } | null;
    const hit = last && models.find((m) => m.backend === last.backend && m.model_id === last.modelId);
    if (hit) return hit;
  } catch {
    // Unreadable: the fallback picks.
  }
  return fallback(models);
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
  | { type: "tool_call"; call_id: string; name: string; args: string }
  | { type: "approval_required"; call_id: string; name: string; args: string }
  | { type: "tool_result"; call_id: string; is_error: boolean; text: string }
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
  /** Set when the file needs RoPE scaling this build does not apply. */
  ignored_rope_scaling: string | null;
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
  ignored_rope_scaling: string | null;
}

/** One part of the built system, as the sandbox measured it. */
export interface SandboxMeasured {
  part: string;
  requests: number;
  failures: number;
  /** Typical and slow (95th percentile) time per request, ms. */
  p50: number;
  p95: number;
  /** Requests answered per second. */
  throughput: number;
  error: string | null;
}

export interface SandboxReport {
  parts: SandboxMeasured[];
  /** The end of what the system printed. */
  output: string;
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

  resolveApproval: (callId: string, approved: boolean) =>
    invoke<boolean>("resolve_approval", { callId, approved }),

  mcpStatus: () => invoke<McpStatus>("mcp_status"),

  listSkills: () => invoke<SkillList>("list_skills"),

  undoEdit: (callId: string) => invoke<void>("undo_edit", { callId }),

  listDir: (root: string, path: string) => invoke<FileEntry[]>("list_dir", { root, path }),

  readTextFile: (root: string, path: string) =>
    invoke<string>("read_text_file", { root, path }),


  writeTextFile: (root: string, path: string, text: string) =>
    invoke<void>("write_text_file", { root, path, text }),


  createFile: (root: string, path: string) => invoke<void>("create_file", { root, path }),
  createDir: (root: string, path: string) => invoke<void>("create_dir", { root, path }),
  /** Returns the new relative path. */
  renamePath: (root: string, path: string, name: string) =>
    invoke<string>("rename_path", { root, path, name }),
  /** To the Recycle Bin. */
  deletePath: (root: string, path: string) => invoke<void>("delete_path", { root, path }),
  /** A blueprint opens in its own window; a second open brings it forward.
   *  With no file it opens untitled, and its save dialog starts in `dir`. */
  openBlueprintWindow: (root: string | null, path: string | null, dir: string | null = null) =>
    invoke<void>("open_blueprint_window", { root, path, dir }),
  blueprintSavedAs: (root: string, path: string) => invoke<void>("blueprint_saved_as", { root, path }),
  /** This blueprint window has (or no longer has) unsaved changes. */
  blueprintDirty: (dirty: boolean) => invoke<void>("blueprint_dirty", { dirty }),
  unsavedBlueprints: () => invoke<number>("unsaved_blueprints"),
  /** One question to a model, outside any chat; nothing is stored. */
  askModel: (backend: string, modelId: string, system: string, prompt: string) =>
    invoke<string>("ask_model", { backend, modelId, system, prompt }),
  /** Start what the agent built, measure each part through its probe, stop it.
   *  `dir` is the blueprint's folder inside `root`. */
  sandboxRun: (root: string, dir: string, spec: Sandbox, seconds: number, concurrency: number, onProgress: (status: string) => void) => {
    const channel = new Channel<string>();
    channel.onmessage = onProgress;
    return invoke<SandboxReport>("sandbox_run", { root, dir, spec, seconds, concurrency, onProgress: channel });
  },
  sandboxStop: () => invoke<void>("sandbox_stop"),
  /** The inventory of parts and connectors every blueprint shares, as JSON. */
  readPartsLibrary: () => invoke<string>("read_parts_library"),
  writePartsLibrary: (text: string) => invoke<void>("write_parts_library", { text }),
  openTerminal: (root: string, onEvent: (e: RunEvent) => void) => {
    const channel = new Channel<RunEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("open_terminal", { root, onEvent: channel });
  },
  pipInstall: (root: string, pkg: string, onEvent: (e: RunEvent) => void) => {
    const channel = new Channel<RunEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("pip_install", { root, package: pkg, onEvent: channel });
  },

  /** Tell the agent's tools which folder and file are open. */
  setWorkspace: (root: string | null, file: string | null) =>
    invoke<void>("set_workspace", { root, file }),

  runFile: (root: string, path: string, onEvent: (e: RunEvent) => void) => {
    const channel = new Channel<RunEvent>();
    channel.onmessage = onEvent;
    return invoke<void>("run_file", { root, path, onEvent: channel });
  },
  runInput: (data: string) => invoke<void>("run_input", { data }),
  runResize: (cols: number, rows: number) => invoke<void>("run_resize", { cols, rows }),
  stopRun: () => invoke<void>("stop_run"),

  /** Open the Understand-Anything dashboard for a project. Returns its URL. */
  openGraph: (project: string) => invoke<string>("open_graph", { project }),
  hasGraph: (project: string) => invoke<boolean>("has_graph", { project }),
  /** Workspace's free graph. Fails with "EXISTS: ..." if another graph is there. */
  buildGraph: (project: string, replace: boolean) =>
    invoke<GraphBuilt>("build_graph", { project, replace }),

  setApiKey: (backend: string, key: string) => invoke<void>("set_api_key", { backend, key }),
  hasApiKey: (backend: string) => invoke<boolean>("has_api_key", { backend }),
  deleteApiKey: (backend: string) => invoke<void>("delete_api_key", { backend }),
};
