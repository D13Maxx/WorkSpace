// Language servers: one per project and language, shared by every editor on
// that project's files, started the first time one is needed. The backend
// picks the program from the file type; this side only speaks LSP to it.
//
// Beyond what @codemirror/lsp-client does on its own, this workspace opens
// other files in tabs (go to definition, references), and renames and code
// actions edit every file they touch, open or not.

import {
  LSPClient,
  LSPPlugin,
  Workspace,
  WorkspaceMapping,
  findReferencesKeymap,
  formatKeymap,
  hoverTooltips,
  jumpToDefinitionKeymap,
  serverCompletion,
  serverDiagnostics,
  signatureHelp,
  type Transport,
  type WorkspaceFile,
} from "@codemirror/lsp-client";
import { Text, type TransactionSpec } from "@codemirror/state";
import { EditorView, keymap, showDialog, type Command } from "@codemirror/view";
import { forEachDiagnostic } from "@codemirror/lint";
import DOMPurify from "dompurify";
import { api } from "./api";
import { hasDraft } from "./components/FileViewer";
import { applyTextEdits, editsByUri, relativeTo, uriPath, type TextEdit } from "./lspEdits";

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

/** How the app shows a file; set once by the app. */
let openInTab: ((root: string, path: string) => void) | null = null;
export function setFileOpener(open: (root: string, path: string) => void): void {
  openInTab = open;
}

class OpenFile implements WorkspaceFile {
  constructor(
    public uri: string,
    public languageId: string,
    public version: number,
    public doc: Text,
    public view: EditorView,
  ) {}
  getView() {
    return this.view;
  }
}

/** Files read from disk for a server's answer, by the URI the server used. */
const diskDocs = new Map<string, Text>();

// The library's reference list converts every position through its record of
// open files, and throws for any other file. Files read from disk are
// converted from their own text instead.
// shortcut: patches @codemirror/lsp-client 6.3.0 (pinned); recheck on upgrade.
const libraryMapPosition = WorkspaceMapping.prototype.mapPosition;
WorkspaceMapping.prototype.mapPosition = function (
  this: WorkspaceMapping,
  uri: string,
  pos: { line: number; character: number },
  ...rest: unknown[]
) {
  const doc = diskDocs.get(uri);
  if (doc && !this.getMapping(uri)) {
    if (pos.line >= doc.lines) return doc.length;
    const line = doc.line(pos.line + 1);
    return Math.min(line.from + pos.character, line.to);
  }
  return (libraryMapPosition as (...a: unknown[]) => number | null).call(this, uri, pos, ...rest);
} as typeof libraryMapPosition;

/** A file read from disk for a server's answer, not open in any editor. */
class DiskFile implements WorkspaceFile {
  version = 0;
  constructor(
    public uri: string,
    public languageId: string,
    public doc: Text,
  ) {}
  getView() {
    return null;
  }
}

/** The library's default workspace, plus files that are not open: they are
 *  opened in a tab when shown, and read from disk when only listed. */
class AppWorkspace extends Workspace {
  files: OpenFile[] = [];
  private versions: Record<string, number> = {};
  private waiting = new Map<string, (view: EditorView | null) => void>();

  constructor(
    client: LSPClient,
    readonly root: string,
  ) {
    super(client);
  }

  /** Servers encode URIs their own way (`c%3A`, a lower-case drive). */
  private same(a: string, b: string) {
    return uriPath(a).toLowerCase() === uriPath(b).toLowerCase();
  }

  /** The project-relative path of `uri`, or null if it is outside the project. */
  pathOf(uri: string): string | null {
    return relativeTo(this.root, uriPath(uri));
  }

  getFile(uri: string): OpenFile | null {
    return this.files.find((f) => this.same(f.uri, uri)) ?? null;
  }

  private nextVersion(uri: string) {
    return (this.versions[uri] = (this.versions[uri] ?? -1) + 1);
  }

  syncFiles() {
    const result = [];
    for (const file of this.files) {
      const plugin = LSPPlugin.get(file.view);
      if (!plugin) continue;
      const changes = plugin.unsyncedChanges;
      if (!changes.empty) {
        result.push({ changes, file, prevDoc: file.doc });
        file.doc = file.view.state.doc;
        file.version = this.nextVersion(file.uri);
        plugin.clear();
      }
    }
    return result;
  }

  openFile(uri: string, languageId: string, view: EditorView) {
    if (this.getFile(uri)) return;
    const file = new OpenFile(uri, languageId, this.nextVersion(uri), view.state.doc, view);
    this.files.push(file);
    this.client.didOpen(file);
    for (const [want, resolve] of this.waiting) {
      if (this.same(want, uri)) {
        this.waiting.delete(want);
        // A moment later, and whichever editor holds it then: React may
        // rebuild a new editor straight away (it does twice in development).
        setTimeout(() => resolve(this.getFile(uri)?.view ?? null), 50);
      }
    }
  }

  closeFile(uri: string) {
    const file = this.getFile(uri);
    if (file) {
      this.files = this.files.filter((f) => f !== file);
      this.client.didClose(file.uri);
    }
  }

  async requestFile(uri: string): Promise<WorkspaceFile | null> {
    const open = this.getFile(uri);
    if (open) return open;
    const path = this.pathOf(uri);
    if (path === null) return null;
    try {
      const doc = Text.of((await api.readTextFile(this.root, path)).split(/\r\n|\r|\n/));
      // Named as the app names open files, so lists show one style of path.
      const own = fileUri(this.root, path);
      diskDocs.set(own, doc);
      return new DiskFile(own, "", doc);
    } catch {
      return null;
    }
  }

  /** Show `uri` in a tab, and hand back its editor once it is up. */
  displayFile(uri: string): Promise<EditorView | null> {
    const open = this.getFile(uri);
    if (open) return Promise.resolve(open.view);
    const path = this.pathOf(uri);
    if (path === null || !openInTab) return Promise.resolve(null);
    return new Promise((resolve) => {
      this.waiting.set(uri, resolve);
      openInTab!(this.root, path);
      setTimeout(() => {
        if (this.waiting.delete(uri)) resolve(null);
      }, 5000);
    });
  }

  updateFile(uri: string, update: TransactionSpec) {
    this.getFile(uri)?.view.dispatch(update);
  }

  /**
   * Apply a server's edits wherever they land: open files through their
   * editor (unsaved, so Undo works), other files on disk. Refuses before
   * changing anything if a file it would write has unsaved changes elsewhere.
   */
  async applyEdits(byUri: Record<string, TextEdit[]>): Promise<string | null> {
    const disk: [string, TextEdit[]][] = [];
    for (const [uri, edits] of Object.entries(byUri)) {
      if (!edits.length || this.getFile(uri)) continue;
      const path = this.pathOf(uri);
      if (path === null) return `the change reaches outside the project (${uriPath(uri)}); nothing was changed`;
      if (hasDraft(this.root, path)) return `${path} has unsaved changes in another tab; save or discard them, then try again`;
      disk.push([path, edits]);
    }
    for (const [uri, edits] of Object.entries(byUri)) {
      const file = this.getFile(uri);
      if (!file || !edits.length) continue;
      const doc = file.view.state.doc;
      const at = (p: TextEdit["range"]["start"]) =>
        p.line >= doc.lines ? doc.length : Math.min(doc.line(p.line + 1).from + p.character, doc.line(p.line + 1).to);
      const changes = edits
        .map((e) => ({ from: at(e.range.start), to: at(e.range.end), insert: e.newText }))
        .sort((a, b) => a.from - b.from);
      file.view.dispatch({ changes, userEvent: "rename" });
    }
    for (const [path, edits] of disk) {
      const text = await api.readTextFile(this.root, path);
      await api.writeTextFile(this.root, path, applyTextEdits(text, edits));
    }
    return null;
  }
}

/** Say something in the editor's own dialog, closed with Esc or OK. */
function tell(view: EditorView, message: string) {
  const { close, result } = showDialog(view, {
    content: (_v, done) => {
      const box = document.createElement("div");
      box.className = "cm-lsp-message";
      const text = document.createElement("span");
      text.textContent = message;
      const ok = document.createElement("button");
      ok.textContent = "OK";
      ok.onclick = done;
      box.append(text, ok);
      return box;
    },
  });
  void result.then(() => view.dispatch({ effects: close }));
}

/** F2: rename the symbol at the cursor in every file that uses it. */
export const renameEverywhere: Command = (view) => {
  const plugin = LSPPlugin.get(view);
  const word = view.state.wordAt(view.state.selection.main.head);
  if (!plugin || !word || !plugin.client.serverCapabilities?.renameProvider) return false;
  const workspace = plugin.client.workspace as AppWorkspace;
  const { close, result } = showDialog(view, {
    label: "New name",
    input: { name: "name", value: view.state.sliceDoc(word.from, word.to) },
    focus: true,
    submitLabel: "Rename everywhere",
    class: "cm-lsp-rename-panel",
  });
  void result.then(async (form) => {
    view.dispatch({ effects: close });
    const newName = (form?.elements.namedItem("name") as HTMLInputElement | null)?.value.trim();
    if (!newName) return;
    plugin.client.sync();
    try {
      const edit = await plugin.client.request<object, { changes?: Record<string, TextEdit[]>; documentChanges?: [] } | null>(
        "textDocument/rename",
        { textDocument: { uri: plugin.uri }, position: plugin.toPosition(word.from), newName },
      );
      if (!edit) return tell(view, "The language server cannot rename this.");
      const { byUri, skipped } = editsByUri(edit);
      const refused = await workspace.applyEdits(byUri);
      if (refused) return tell(view, `Not renamed: ${refused}.`);
      const n = Object.values(byUri).filter((e) => e.length).length;
      tell(view, `Renamed in ${n} file${n === 1 ? "" : "s"}${skipped.length ? `; not done: ${skipped.join(", ")} of whole files` : ""}. Open files have unsaved changes; others were saved.`);
    } catch (e) {
      tell(view, `Rename failed: ${String((e as { message?: string }).message ?? e)}`);
    }
  });
  return true;
};

interface CodeAction {
  title: string;
  kind?: string;
  edit?: { changes?: Record<string, TextEdit[]>; documentChanges?: [] };
  command?: { command: string; arguments?: unknown[] } | string;
  arguments?: unknown[];
  data?: unknown;
}

/** Ctrl+.: quick fixes and refactors the server offers at the cursor. */
export const codeActions: Command = (view) => {
  const plugin = LSPPlugin.get(view);
  if (!plugin || !plugin.client.serverCapabilities?.codeActionProvider) return false;
  const workspace = plugin.client.workspace as AppWorkspace;
  const { from, to } = view.state.selection.main;
  // The problems under the cursor, which the server's quick fixes key on.
  const diagnostics: object[] = [];
  forEachDiagnostic(view.state, (d, dFrom, dTo) => {
    if (dFrom <= to && dTo >= from) {
      diagnostics.push({
        range: { start: plugin.toPosition(dFrom), end: plugin.toPosition(dTo) },
        message: d.message,
        severity: { error: 1, warning: 2, info: 3, hint: 4 }[d.severity],
      });
    }
  });
  plugin.client.sync();
  void plugin.client
    .request<object, CodeAction[] | null>("textDocument/codeAction", {
      textDocument: { uri: plugin.uri },
      range: { start: plugin.toPosition(from), end: plugin.toPosition(to) },
      context: { diagnostics },
    })
    .then((actions) => {
      if (!actions?.length) return tell(view, "No code actions here.");
      const run = async (action: CodeAction) => {
        let a = action;
        // Some servers (rust-analyzer) send the edit only when asked for it.
        if (!a.edit && !a.command && a.data !== undefined) {
          a = await plugin.client.request<CodeAction, CodeAction>("codeAction/resolve", a);
        }
        if (a.edit) {
          const refused = await workspace.applyEdits(editsByUri(a.edit).byUri);
          if (refused) return tell(view, `Not applied: ${refused}.`);
        }
        const cmd = typeof a.command === "string" ? { command: a.command, arguments: a.arguments } : a.command;
        if (cmd) await plugin.client.request("workspace/executeCommand", cmd);
      };
      const { close, result } = showDialog(view, {
        content: (_v, done) => {
          const list = document.createElement("div");
          list.className = "cm-lsp-actions";
          for (const action of actions) {
            const b = document.createElement("button");
            b.type = "button";
            b.textContent = action.title;
            b.onclick = () => {
              done();
              run(action).catch((e) => tell(view, `That action failed: ${String((e as { message?: string }).message ?? e)}`));
            };
            list.append(b);
          }
          queueMicrotask(() => list.querySelector("button")?.focus());
          return list;
        },
      });
      void result.then(() => view.dispatch({ effects: close }));
    }, (e) => tell(view, `Code actions failed: ${String((e as { message?: string }).message ?? e)}`));
  return true;
};

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
  let workspace: AppWorkspace | null = null;
  let id: number;
  const reply = (msg: object) => void api.bridgeSend(id, JSON.stringify({ jsonrpc: "2.0", ...msg })).catch(() => {});
  try {
    id = await api.lspStart(root, ext, (e) => {
      if (e.type !== "message") {
        // Later files say why, instead of waiting on a server that is gone.
        const why = e.stderr.trim().split("\n").slice(-3).join(" ") || `exit code ${e.code ?? "unknown"}`;
        started.set(key, Promise.resolve({ error: `the language server stopped: ${why}` }));
        return;
      }
      // The client answers every server request with "not implemented";
      // applying an edit (after a code action's command) is handled here.
      if (e.text.includes('"workspace/applyEdit"')) {
        const msg = JSON.parse(e.text) as { id?: number; method?: string; params?: { edit: object } };
        if (msg.method === "workspace/applyEdit" && msg.id !== undefined && workspace && msg.params) {
          void workspace.applyEdits(editsByUri(msg.params.edit).byUri).then((refused) =>
            reply({ id: msg.id, result: refused ? { applied: false, failureReason: refused } : { applied: true } }),
          );
          return;
        }
      }
      for (const h of handlers) h(e.text);
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
    workspace: (c) => (workspace = new AppWorkspace(c, root)),
    extensions: [
      serverCompletion(),
      hoverTooltips(),
      // The library's F2 rename skips files that are not open, so it is replaced.
      keymap.of([
        ...formatKeymap,
        ...jumpToDefinitionKeymap,
        ...findReferencesKeymap,
        { key: "F2", run: renameEverywhere, preventDefault: true },
        { key: "Mod-.", run: codeActions, preventDefault: true },
      ]),
      signatureHelp(),
      serverDiagnostics(),
    ],
    // Hover text is Markdown from the server, rendered as HTML.
    sanitizeHTML: (html) => DOMPurify.sanitize(html),
  }).connect(transport);
  return { client, id };
}
