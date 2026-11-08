// The middle pane: one code or Markdown file, in the editor. Other files
// (PDFs, Word, plain text…) stay in the tree but do not open here.

import { Component, useEffect, useMemo, useRef, useState } from "react";
import type { Extension } from "@codemirror/state";
import type { EditorView } from "codemirror";
import { emit } from "@tauri-apps/api/event";
import { api, ask } from "../api";
import { CodeEditor } from "./CodeEditor";
import { MarkdownPreview } from "./MarkdownPreview";
import { fileUri, languageServer } from "../lsp";
import { breakpointGutter } from "../breakpoints";
import { gitChanged, gitExtensions } from "../git";

/** Files the debugger can run. */
export const DEBUGGABLE = new Set(["c", "cpp", "cc", "cxx", "rs", "py"]);

const MARKDOWN = new Set(["md", "markdown", "mdx"]);

/** Files the output panel can build and run. */
export const RUNNABLE = new Set([
  ...["c", "cpp", "cc", "cxx", "py", "rs", "tex"],
  ...["js", "mjs", "cjs", "ts", "mts", "cts"],
]);

/** What opens here: code (and config) and Markdown. */
const EDITABLE = new Set([
  ...RUNNABLE,
  ...["h", "hh", "hpp", "hxx", "java", "kt", "kts", "go", "rb", "php", "cs", "swift", "lua", "r", "dart", "scala"],
  ...["jsx", "tsx", "vue", "svelte", "html", "htm", "css", "scss", "sass", "less"],
  ...["json", "jsonc", "toml", "yaml", "yml", "xml", "ini", "cfg", "conf", "env", "lock", "gradle", "cmake"],
  ...["sh", "bash", "zsh", "ps1", "psm1", "bat", "cmd", "sql", "graphql", "proto"],
  ...["md", "markdown", "mdx"],
  // A blueprint's text, opened from its window's Source button.
  "blueprint",
  ...["gitignore", "gitattributes", "editorconfig", "dockerignore", "npmrc"],
]);
const EDITABLE_NAMES = new Set(["makefile", "dockerfile", "cmakelists.txt", "procfile"]);

export function opensHere(path: string): boolean {
  const name = (path.split(/[\\/]/).pop() ?? "").toLowerCase();
  return EDITABLE_NAMES.has(name) || EDITABLE.has(extOf(path));
}

export function extOf(path: string): string {
  const m = /\.([^./\\]+)$/.exec(path);
  return m ? m[1].toLowerCase() : "";
}

/** Unsaved text by file, kept when the user opens another file and comes back. */
// shortcut: lost when the app closes, upgrade to a warn-on-close if that bites.
// `base` is the file as it was on disk under the draft, so saving it later can
// tell whether someone else changed the file meanwhile.
const drafts = new Map<string, { doc: string; base: string | null }>();

/** After `from` (a file or folder) in `root` became `to`, keep its drafts. */
export function moveDrafts(root: string, from: string, to: string | null) {
  for (const [key, draft] of [...drafts]) {
    const [r, path] = key.split("\n");
    if (r !== root || (path !== from && !path.startsWith(`${from}/`))) continue;
    drafts.delete(key);
    if (to !== null) drafts.set(`${root}\n${to}${path.slice(from.length)}`, draft);
  }
  for (const key of [...edited]) {
    const [r, path] = key.split("\n");
    if (r === root && (path === from || path.startsWith(`${from}/`))) edited.delete(key);
  }
}

/** Files being edited with unsaved changes right now; `drafts` only fills when an editor closes. */
const edited = new Set<string>();

/** Whether `path` has unsaved text, shown or kept aside, for a tab's dot. */
export function hasDraft(root: string, path: string): boolean {
  const key = `${root}\n${path}`;
  return edited.has(key) || drafts.has(key);
}

interface Props {
  project: string;
  path: string;
  /** Show this file's changes since the last commit rather than the file. */
  diff?: boolean;
  /** Scroll to this line when the file shows; `lineKey` repeats a jump. */
  line?: number;
  lineKey?: unknown;
  /** Called when this file gains or loses unsaved changes, so a tab's dot can follow. */
  onDirtyChange?: () => void;
  /** Called after the file is saved. */
  onSaved?: () => void;
  /** Start debugging `path`; absent where the debugger is not offered. */
  onDebug?: (path: string) => void;
  /** Bumped when the agent or a run may have changed files on disk. */
  version: number;
  onClose: () => void;
  onRun: (path: string) => void;
}

function Header({ path, children, onClose }: { path: string; children?: React.ReactNode; onClose: () => void }) {
  return (
    <header>
      <code title={path}>{path}</code>
      <span className="actions">
        {children}
        <button onClick={onClose} aria-label="Close file">
          ✕
        </button>
      </span>
    </header>
  );
}

/** A viewer that throws shows why, in its pane, instead of blanking the app. */
class ViewerBoundary extends Component<{ path: string; children: React.ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  render() {
    return this.state.error ? (
      <section className="file-viewer">
        <p className="failure">
          Could not show {this.props.path}: {this.state.error.message}
        </p>
        <button onClick={() => this.setState({ error: null })}>Try again</button>
      </section>
    ) : (
      this.props.children
    );
  }
}

export function FileViewer(props: Props) {
  return (
    <ViewerBoundary path={props.path}>
      <FileView {...props} />
    </ViewerBoundary>
  );
}

function FileView(props: Props) {
  if (props.diff) return <DiffFile {...props} />;
  if (!opensHere(props.path)) {
    return (
      <section className="file-viewer" aria-label={props.path}>
        <Header path={props.path} onClose={props.onClose} />
        <p className="muted file-note">Workspace opens code and Markdown files. Open this one in another app.</p>
      </section>
    );
  }
  return <TextFile {...props} ext={extOf(props.path)} />;
}

function TextFile({ project, path, version, onClose, onRun, onDebug, ext, line, lineKey, onDirtyChange, onSaved }: Props & { ext: string }) {
  const key = `${project}\n${path}`;
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [nonce, setNonce] = useState(0);
  const viewRef = useRef<EditorView | null>(null);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  useEffect(() => {
    if (dirty) edited.add(key);
    else edited.delete(key);
    onDirtyChange?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, key]);
  // The file as it was on disk when last read or saved here.
  const base = useRef<string | null>(null);
  // Markdown: the preview beside the editor, from the text as typed.
  const md = MARKDOWN.has(ext);
  const [preview, setPreview] = useState(true);
  const [live, setLive] = useState("");
  // Code intelligence from the language server, once it is up.
  const [server, setServer] = useState<Extension | null>(null);
  const [serverNote, setServerNote] = useState<string | null>(null);
  useEffect(() => {
    let current = true;
    setServer(null);
    setServerNote(null);
    void languageServer(project, ext).then((r) => {
      if (!current || !r) return;
      if ("client" in r) setServer(r.client.plugin(fileUri(project, path), r.languageId));
      else setServerNote(r.error);
    });
    return () => {
      current = false;
    };
  }, [project, path, ext]);
  const debuggable = DEBUGGABLE.has(ext) && !!onDebug;
  const gutter = useMemo(() => (debuggable ? breakpointGutter(project, path) : []), [debuggable, project, path]);
  const git = useMemo(() => gitExtensions(project, path), [project, path]);
  const extensions = useMemo(() => [gutter, git, server ?? []], [gutter, git, server]);
  useEffect(() => setLive(text ?? ""), [text]);

  useEffect(() => {
    setError(null);
    const draft = drafts.get(key);
    if (draft !== undefined && nonce === 0) {
      setText(draft.doc);
      base.current = draft.base;
      setDirty(true);
      return;
    }
    setText(null);
    setDirty(false);
    api
      .readTextFile(project, path)
      .then((t) => {
        base.current = t;
        setText(t);
      })
      .catch((e) => setError(String(e)));
  }, [key, project, path, nonce]);

  // Pick up the agent's edits, but never over unsaved typing, and only when
  // the file really changed: most agent steps only read, and a reload costs
  // the cursor, the scroll position and undo.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current) return;
    seen.current = version;
    if (dirtyRef.current) return;
    let live = true;
    api
      .readTextFile(project, path)
      .then((t) => {
        if (!live || dirtyRef.current || t === base.current) return;
        base.current = t;
        setText(t);
      })
      // Gone or unreadable: keep what is shown; Reload says why.
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [version, project, path]);

  const save = async (): Promise<boolean> => {
    const view = viewRef.current;
    if (!view) return false;
    setSaving(true);
    try {
      // The agent (or another window) may have written the file while this
      // edit was open; replacing that needs a yes.
      const disk = await api.readTextFile(project, path).catch(() => null);
      if (disk !== null && base.current !== null && disk !== base.current) {
        if (!(await ask(`${path} changed on disk since you opened it (perhaps the agent edited it). Replace it with your version?`))) return false;
      }
      const doc = view.state.sliceDoc();
      await api.writeTextFile(project, path, doc);
      base.current = doc;
      // An open blueprint window redraws from the text just saved.
      if (ext === "blueprint") void emit("files-changed");
      drafts.delete(key);
      setDirty(false);
      setError(null);
      // Blame follows the saved file.
      gitChanged();
      onSaved?.();
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="file-viewer" aria-label={path}>
      <Header path={path} onClose={onClose}>
        {dirty && <span className="muted unsaved">unsaved</span>}
        {serverNote && (
          <span className="muted server-note" title={serverNote}>
            no code intelligence
          </span>
        )}
        {md && (
          <button onClick={() => setPreview(!preview)} className={preview ? "selected" : ""} aria-pressed={preview}>
            Preview
          </button>
        )}
        {RUNNABLE.has(ext) && (
          <button
            className="primary"
            title={ext === "tex" ? "Save and build the PDF" : "Save, compile and run"}
            onClick={async () => {
              if (!dirtyRef.current || (await save())) onRun(path);
            }}
          >
            {ext === "tex" ? "Build PDF" : "Run"}
          </button>
        )}
        {debuggable && (
          <button
            title="Save, build with debug information and debug; click left of a line number to set a breakpoint"
            onClick={async () => {
              if (!dirtyRef.current || (await save())) onDebug?.(path);
            }}
          >
            Debug
          </button>
        )}
        <button onClick={() => void save()} disabled={!dirty || saving} title="Ctrl+S">
          {saving ? "Saving…" : "Save"}
        </button>
        <button
          onClick={() => {
            drafts.delete(key);
            setDirty(false);
            setNonce((n) => n + 1);
          }}
          title={dirty ? "Throw away your changes and reload from disk" : "Reload from disk"}
        >
          {dirty ? "Discard changes" : "Reload"}
        </button>
      </Header>
      {error && <p className="failure">{error}</p>}
      {text === null && !error && <p className="muted">Loading…</p>}
      {text !== null && (
        <div className={md && preview ? "md-split" : "md-solo"}>
          <CodeEditor
            text={text}
            ext={ext}
            viewRef={viewRef}
            onChange={() => {
              setDirty(true);
              if (md) setLive(viewRef.current?.state.sliceDoc() ?? "");
            }}
            onSave={() => void save()}
            line={line}
            lineKey={lineKey}
            extensions={extensions}
            onDispose={(doc) => {
              if (dirtyRef.current) drafts.set(key, { doc, base: base.current });
            }}
          />
          {md && preview && <MarkdownPreview text={live} />}
        </div>
      )}
    </section>
  );
}

const diffClass = (l: string) =>
  /^(\+\+\+|---|diff |index |new file)/.test(l) ? "d-meta" : l.startsWith("@@") ? "d-hunk" : l.startsWith("+") ? "d-add" : l.startsWith("-") ? "d-del" : "";

/** One file's change since the last commit, coloured. */
function DiffFile({ project, path, version, onClose }: Props) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api
      .gitDiff(project, path)
      .then((t) => {
        if (!live) return;
        setText(t);
        setError(null);
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [project, path, version]);
  return (
    <section className="file-viewer" aria-label={`Changes in ${path}`}>
      <Header path={path} onClose={onClose}>
        <span className="muted">changes since the last commit</span>
      </Header>
      {error && <p className="failure">{error}</p>}
      {text === null && !error && <p className="muted">Loading…</p>}
      {text !== null && (
        <pre className="diff">
          {text.split("\n").map((l, i) => (
            <span key={i} className={diffClass(l)}>
              {l}
              {"\n"}
            </span>
          ))}
        </pre>
      )}
    </section>
  );
}
