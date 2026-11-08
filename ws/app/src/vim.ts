// Vim keys in every code editor, switched in Settings and applied to open
// editors straight away. The choice is kept in this window's storage.

import { Vim } from "@replit/codemirror-vim";
import { findReferences, jumpToDefinition } from "@codemirror/lsp-client";
import type { EditorView } from "@codemirror/view";

const KEY = "ws.vim";
const listeners = new Set<(on: boolean) => void>();

export function vimEnabled(): boolean {
  try {
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

export function setVimEnabled(on: boolean): void {
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    // Storage blocked: it still applies until the app closes.
  }
  for (const l of listeners) l(on);
}

export function onVimChange(listener: (on: boolean) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** What `:w` does in each open editor: its own save. */
const savers = new WeakMap<EditorView, () => void>();

export function setSaver(view: EditorView, save: () => void): void {
  savers.set(view, save);
}

const write = (cm: { cm6: EditorView }) => savers.get(cm.cm6)?.();
Vim.defineEx("write", "w", write);
Vim.defineEx("update", "up", write);

// Neovim's language-server keys: gd for the definition, gr for references.
Vim.defineAction("lspDefinition", (cm: { cm6: EditorView }) => void jumpToDefinition(cm.cm6));
Vim.defineAction("lspReferences", (cm: { cm6: EditorView }) => void findReferences(cm.cm6));
Vim.mapCommand("gd", "action", "lspDefinition", {}, { context: "normal" });
Vim.mapCommand("gr", "action", "lspReferences", {}, { context: "normal" });

/** What Vim's window keys do: the app's editor splits. */
export interface WindowCommands {
  split: (dir: "row" | "column") => void;
  close: () => void;
  focus: (by: number) => void;
  only: () => void;
}
let windows: WindowCommands | null = null;
export function setWindowCommands(commands: WindowCommands): void {
  windows = commands;
}

// Ctrl+W v/s split, w/l/j next, h/k previous, c/q close, o only; and the
// matching : commands.
const windowActions: [string, string[], () => void][] = [
  ["winSplitRight", ["v"], () => windows?.split("row")],
  ["winSplitDown", ["s"], () => windows?.split("column")],
  ["winNext", ["w", "l", "j", "<C-w>"], () => windows?.focus(1)],
  ["winPrevious", ["h", "k", "W"], () => windows?.focus(-1)],
  ["winClose", ["c", "q"], () => windows?.close()],
  ["winOnly", ["o"], () => windows?.only()],
];
// The library ignores Ctrl+W on its own in normal mode, which ends the key
// sequence before the second key; insert mode's Ctrl+W (delete word) stays.
Vim.unmap("<C-w>", "normal");
for (const [name, keys, run] of windowActions) {
  Vim.defineAction(name, run);
  for (const k of keys) Vim.mapCommand(`<C-w>${k}`, "action", name, {}, { context: "normal" });
}
Vim.defineEx("vsplit", "vs", () => windows?.split("row"));
Vim.defineEx("split", "sp", () => windows?.split("column"));
Vim.defineEx("close", "clo", () => windows?.close());
Vim.defineEx("only", "on", () => windows?.only());
