// The middle pane: one file, edited or viewed by its type.
//
// Text and code open in an editor. PDFs open in pdf.js, which can annotate
// them. Word files are drawn into a sandboxed frame where scripts cannot run, never
// into the app's own page (master spec s12): a document is untrusted input.

import { useEffect, useRef, useState } from "react";
import { renderAsync } from "docx-preview";
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

/** A Word file as Word lays it out: fonts, sizes, colours, tables, headers,
 *  footers and page breaks (docx-preview). */
function DocxFile({ project, path, version, onClose }: Props) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  // A fresh frame per load, so nothing from the last render lingers.
  const key = `${version}:${nonce}`;

  useEffect(() => {
    let live = true;
    setReady(false);
    setError(null);
    // A frame with no src keeps its first, empty document: ready at once.
    const doc = frame.current?.contentDocument;
    if (!doc) return;
    api
      .readFileBytes(project, path)
      .then(async (bytes) => {
        if (!live) return;
        doc.body.style.margin = "0";
        await renderAsync(bytes, doc.body, doc.head, {
          inWrapper: true,
          breakPages: true,
          // Images as data: URLs, which the app's content policy allows.
          useBase64URL: true,
          // Embedded HTML chunks are the one way a document carries markup.
          renderAltChunks: false,
          renderComments: false,
          renderChanges: false,
        });
        if (live) setReady(true);
      })
      .catch((e) => live && setError(String(e)));
    // Pages keep their real width (A4 is ~794px), so shrink them to the pane.
    const iframe = frame.current;
    const fit = () => {
      const page = doc.querySelector<HTMLElement>("section.docx");
      // A hidden frame measures 0 wide; the observer fires again once it shows.
      if (iframe && page && iframe.clientWidth > 0) {
        doc.body.style.zoom = String(Math.min(1, (iframe.clientWidth - 32) / page.offsetWidth));
      }
    };
    const resize = new ResizeObserver(fit);
    if (iframe) resize.observe(iframe);
    return () => {
      live = false;
      resize.disconnect();
    };
  }, [project, path, key]);

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
      {!error && !ready && <p className="muted">Loading…</p>}
      {/* Same origin so FRIDAY can draw into it; no allow-scripts, so nothing
          in the document can run. */}
      <iframe
        key={key}
        ref={frame}
        className="doc-frame"
        sandbox="allow-same-origin"
        title={path}
        hidden={!ready}
      />
    </section>
  );
}
