// The middle pane: one file, edited or viewed by its type.
//
// Text and code open in an editor. PDFs open in pdf.js, which can annotate
// them. Word files open in SuperDoc, a .docx editor that saves back to .docx.

import { Component, useEffect, useRef, useState } from "react";
import { SuperDoc } from "superdoc";
import "superdoc/style.css";
import type { EditorView } from "codemirror";
import { api } from "../api";
import { CodeEditor } from "./CodeEditor";
import { PdfFile } from "./PdfFile";

/** Files the output panel can build and run. */
export const RUNNABLE = new Set([
  ...["c", "cpp", "cc", "cxx", "py", "rs", "tex"],
  ...["js", "mjs", "cjs", "ts", "mts", "cts"],
]);

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
  const ext = extOf(props.path);
  if (ext === "pdf") {
    return (
      <PdfFile
        {...props}
        header={(actions) => (
          <Header path={props.path} onClose={props.onClose}>
            {actions}
          </Header>
        )}
      />
    );
  }
  if (ext === "docx") return <DocxFile {...props} />;
  return <TextFile {...props} ext={ext} />;
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

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/** A Word file, edited as Word lays it out (SuperDoc), saved back as .docx. */
function DocxFile({ project, path, version, onClose }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const editor = useRef<SuperDoc | null>(null);
  const [ready, setReady] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;

  useEffect(() => {
    let live = true;
    setReady(false);
    setDirty(false);
    setError(null);
    api
      .readFileBytes(project, path)
      .then((bytes) => {
        if (!live || !host.current || !bar.current) return;
        editor.current = new SuperDoc({
          selector: host.current,
          document: new File([bytes], path.split("/").pop() ?? "document.docx", { type: DOCX_TYPE }),
          documentMode: "editing",
          // Sized to the middle pane, not the window, so it never runs under the chat.
          ui: { toolbar: { container: bar.current, responsiveTo: "container" }, comments: false },
          // A followed link would navigate FRIDAY's own window away.
          hyperlinks: false,
          onReady: () => live && setReady(true),
          // onReady can be skipped when the editor recovers; the first render counts too.
          onEditorCreate: () => live && setReady(true),
          onEditorUpdate: () => live && setDirty(true),
          onException: (e: unknown) => live && setError(String((e as { error?: unknown })?.error ?? e)),
        });
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
      editor.current?.destroy();
      editor.current = null;
      host.current?.replaceChildren();
      bar.current?.replaceChildren();
    };
  }, [project, path, nonce]);

  // Pick up the agent's edits, but never over unsaved typing.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current) return;
    seen.current = version;
    if (!dirtyRef.current) setNonce((n) => n + 1);
  }, [version]);

  const save = async () => {
    const doc = editor.current;
    if (!doc) return;
    setSaving(true);
    try {
      const blob = await doc.export({ exportType: ["docx"], triggerDownload: false });
      await api.writeFileBytes(project, path, new Uint8Array(await blob.arrayBuffer()));
      setDirty(false);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  // Ctrl+S saves here as in the code editor.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (dirtyRef.current) void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <section className="file-viewer" aria-label={path}>
      <Header path={path} onClose={onClose}>
        {dirty && <span className="muted unsaved">unsaved</span>}
        <button onClick={() => void save()} disabled={!dirty || saving} title="Ctrl+S">
          {saving ? "Saving…" : "Save"}
        </button>
        <button
          onClick={() => setNonce((n) => n + 1)}
          title={dirty ? "Throw away your changes and reload from disk" : "Reload from disk"}
        >
          {dirty ? "Discard changes" : "Reload"}
        </button>
      </Header>
      {error && <p className="failure">{error}</p>}
      {!error && !ready && <p className="muted">Loading…</p>}
      <div className="docx-toolbar" ref={bar} />
      <div className="docx-area" key={nonce}>
        <div className="docx-editor" ref={host} />
      </div>
    </section>
  );
}
