// Vim keys in every code editor, switched in Settings and applied to open
// editors straight away. The choice is kept in this window's storage.

import { Vim } from "@replit/codemirror-vim";
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
