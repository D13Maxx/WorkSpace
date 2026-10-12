// The middle pane: one code or Markdown file, in the editor. Other files
// (PDFs, Word, plain text…) stay in the tree but do not open here.

import { Component, useEffect, useRef, useState } from "react";
import type { EditorView } from "codemirror";
import { api } from "../api";
import { CodeEditor } from "./CodeEditor";

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
const drafts = new Map<string, string>();

/** After `from` (a file or folder) in `root` became `to`, keep its drafts. */
export function moveDrafts(root: string, from: string, to: string | null) {
  for (const [key, text] of [...drafts]) {
    const [r, path] = key.split("\n");
    if (r !== root || (path !== from && !path.startsWith(`${from}/`))) continue;
    drafts.delete(key);
    if (to !== null) drafts.set(`${root}\n${to}${path.slice(from.length)}`, text);
  }
}

interface Props {
  project: string;
  path: string;
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
  if (!opensHere(props.path)) {
    return (
      <section className="file-viewer" aria-label={props.path}>
        <Header path={props.path} onClose={props.onClose} />
        <p className="muted file-note">FRIDAY opens code and Markdown files. Open this one in another app.</p>
      </section>
    );
  }
  return <TextFile {...props} ext={extOf(props.path)} />;
}

function TextFile({ project, path, version, onClose, onRun, ext }: Props & { ext: string }) {
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
    setError(null);
    const draft = drafts.get(key);
    if (draft !== undefined && nonce === 0) {
      setText(draft);
      setDirty(true);
      return;
    }
    setText(null);
    setDirty(false);
    api
      .readTextFile(project, path)
      .then(setText)
      .catch((e) => setError(String(e)));
  }, [key, project, path, nonce]);

  // Pick up the agent's edits, but never over unsaved typing.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current) return;
    seen.current = version;
    if (!dirtyRef.current) setNonce((n) => n + 1);
  }, [version]);

  const save = async (): Promise<boolean> => {
    const view = viewRef.current;
    if (!view) return false;
    setSaving(true);
    try {
      await api.writeTextFile(project, path, view.state.sliceDoc());
      drafts.delete(key);
      setDirty(false);
      setError(null);
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
        <CodeEditor
          text={text}
          ext={ext}
          viewRef={viewRef}
          onChange={() => setDirty(true)}
          onSave={() => void save()}
          onDispose={(doc) => {
            if (dirtyRef.current) drafts.set(key, doc);
          }}
        />
      )}
    </section>
  );
}
