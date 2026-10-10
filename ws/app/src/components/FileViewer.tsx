// The middle pane: one file, edited or viewed by its type.
//
// Text and code open in an editor. PDFs open in pdf.js, which can annotate
// them. Word files are rendered to HTML in a sandboxed frame with no scripts, never
// into the app's own page (master spec s12): a document is untrusted input.

import { useEffect, useRef, useState } from "react";
import mammoth from "mammoth";
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

export function FileViewer(props: Props) {
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

const DOCX_STYLE = `<style>
body { font: 15px/1.6 system-ui, "Segoe UI", sans-serif; max-width: 50em; margin: 2em auto; padding: 0 1.5em; color: #16191d; background: #fff; }
img { max-width: 100%; }
table { border-collapse: collapse; }
td, th { border: 1px solid #c8cfd8; padding: 4px 8px; }
</style>`;

function DocxFile({ project, path, version, onClose }: Props) {
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let live = true;
    setHtml(null);
    setError(null);
    api
      .readFileBytes(project, path)
      .then(async (bytes) => {
        const result = await mammoth.convertToHtml({ arrayBuffer: bytes });
        if (live) setHtml(result.value);
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [project, path, version, nonce]);

  return (
    <section className="file-viewer" aria-label={path}>
      <Header path={path} onClose={onClose}>
        <span className="muted" title="Ask the agent in the chat to change this document">
          read-only — ask the agent to edit
        </span>
        <button onClick={() => setNonce((n) => n + 1)} title="Reload from disk">
          Reload
        </button>
      </Header>
      {error && <p className="failure">{error}</p>}
      {!error && html === null && <p className="muted">Loading…</p>}
      {html !== null && (
        // No allow-scripts, no same-origin: the document can only be looked at.
        <iframe className="doc-frame" sandbox="" srcDoc={DOCX_STYLE + html} title={path} />
      )}
    </section>
  );
}
