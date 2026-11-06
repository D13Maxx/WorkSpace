// Language servers: one per project and language, shared by every editor on
// that project's files, started the first time one is needed. The backend
// picks the program from the file type; this side only speaks LSP to it.

import { LSPClient, languageServerExtensions, type Transport } from "@codemirror/lsp-client";
import DOMPurify from "dompurify";
import { api } from "./api";

/** File type → [which server handles it, the LSP language id]. */
const LANGS: Record<string, [string, string]> = {
  rs: ["rust", "rust"],
  c: ["c", "c"],
  h: ["c", "c"],
  cpp: ["c", "cpp"],
  cc: ["c", "cpp"],
  cxx: ["c", "cpp"],
  hpp: ["c", "cpp"],
  hh: ["c", "cpp"],
  hxx: ["c", "cpp"],
  py: ["py", "python"],
  ts: ["ts", "typescript"],
  mts: ["ts", "typescript"],
  cts: ["ts", "typescript"],
  tsx: ["ts", "typescriptreact"],
  js: ["ts", "javascript"],
  mjs: ["ts", "javascript"],
  cjs: ["ts", "javascript"],
  jsx: ["ts", "javascriptreact"],
  go: ["go", "go"],
};

/** `file:///C:/proj/src/a.rs` for root `C:\proj` and path `src/a.rs`. */
export function fileUri(root: string, path: string): string {
  const abs = [root.replace(/[\\/]+$/, ""), path].filter(Boolean).join("/").replace(/\\/g, "/");
  return "file:///" + encodeURI(abs.replace(/^\/+/, "")).replace(/#/g, "%23").replace(/\?/g, "%3F");
}

type Started = { client: LSPClient; languageId: string } | { error: string };
type Running = { client: LSPClient; id: number } | { error: string };
const started = new Map<string, Promise<Running>>();

/** Stop the servers for a folder that was closed; reopening it starts them again. */
export function stopLanguageServers(root: string): void {
  for (const [key, p] of started) {
    if (!key.startsWith(`${root}\n`)) continue;
    started.delete(key);
    void p.then((r) => {
      if ("client" in r) {
        r.client.disconnect();
        void api.bridgeStop(r.id);
      }
    });
  }
}

/** The language server for `ext` files in `root`, started if need be; null
 *  for a type no server is known for. An error says what to install. */
export async function languageServer(root: string, ext: string): Promise<Started | null> {
  const lang = LANGS[ext];
  if (!lang) return null;
  const key = `${root}\n${lang[0]}`;
  let p = started.get(key);
  if (!p) {
    p = start(root, ext, key);
    started.set(key, p);
  }
  const r = await p;
  return "client" in r ? { client: r.client, languageId: lang[1] } : r;
}

async function start(root: string, ext: string, key: string): Promise<Running> {
  const handlers = new Set<(message: string) => void>();
  let id: number;
  try {
    id = await api.lspStart(root, ext, (e) => {
      if (e.type === "message") {
        for (const h of handlers) h(e.text);
        return;
      }
      // Later files say why, instead of waiting on a server that is gone.
      const why = e.stderr.trim().split("\n").slice(-3).join(" ") || `exit code ${e.code ?? "unknown"}`;
      started.set(key, Promise.resolve({ error: `the language server stopped: ${why}` }));
    });
  } catch (e) {
    // Kept, so every file of this type does not try again; a restart retries.
    return { error: String(e) };
  }
  const transport: Transport = {
    send: (message) => {
      api.bridgeSend(id, message).catch(() => {});
    },
    subscribe: (h) => handlers.add(h),
    unsubscribe: (h) => handlers.delete(h),
  };
  const client = new LSPClient({
    rootUri: fileUri(root, ""),
    extensions: languageServerExtensions(),
    // Hover text is Markdown from the server, rendered as HTML.
    sanitizeHTML: (html) => DOMPurify.sanitize(html),
    // shortcut: jumps stay within open files (the default workspace), upgrade to open other files in tabs.
  }).connect(transport);
  return { client, id };
}
