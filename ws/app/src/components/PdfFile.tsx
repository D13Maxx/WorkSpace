// A PDF, viewed and annotated with pdf.js, then saved back into the file.
//
// pdf.js renders into this page, not a plugin frame, so its editors (text
// boxes, freehand drawing, highlights) are available, and `saveDocument`
// returns the PDF with them written in as real annotations.

import { useEffect, useRef, useState } from "react";
import * as pdfjs from "pdfjs-dist";
// After pdfjs-dist: the viewer reads the library from globalThis.
import { EventBus, PDFLinkService, PDFViewer } from "pdfjs-dist/web/pdf_viewer.mjs";
import "pdfjs-dist/web/pdf_viewer.css";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { api } from "../api";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

/** pdf.js's data, copied into public/pdfjs by scripts/copy-pdfjs-data.mjs.
 *  Without it, fonts a PDF does not embed and scanned images draw nothing. */
const here = (dir: string) => new URL(`/pdfjs/${dir}/`, window.location.href).href;
const PDF_DATA = {
  cMapUrl: here("cmaps"),
  cMapPacked: true,
  standardFontDataUrl: here("standard_fonts"),
  wasmUrl: here("wasm"),
  iccUrl: here("iccs"),
};

const { NONE, FREETEXT, INK, HIGHLIGHT } = pdfjs.AnnotationEditorType;
const TOOLS: [string, number, string][] = [
  ["Select", NONE, "Scroll, select text and move what you added"],
  ["Text", FREETEXT, "Click on the page to add text"],
  ["Draw", INK, "Draw freehand"],
  ["Highlight", HIGHLIGHT, "Select text to highlight it"],
];

interface Props {
  project: string;
  path: string;
  version: number;
  header: (actions: React.ReactNode) => React.ReactNode;
}

// shortcut: unsaved annotations are lost on switching files, unlike text drafts; keep them if that bites.
export function PdfFile({ project, path, version, header }: Props) {
  const container = useRef<HTMLDivElement>(null);
  const viewerRef = useRef<PDFViewer | null>(null);
  const docRef = useRef<pdfjs.PDFDocumentProxy | null>(null);
  const [mode, setMode] = useState(NONE);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  // For an encrypted PDF: the password tried, and whether one is being asked for.
  const [password, setPassword] = useState<string | null>(null);
  const [asking, setAsking] = useState<null | "needed" | "wrong">(null);
  const [typed, setTyped] = useState("");
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;

  useEffect(() => {
    const el = container.current;
    if (!el) return;
    let live = true;
    let task: pdfjs.PDFDocumentLoadingTask | null = null;
    const eventBus = new EventBus();
    const linkService = new PDFLinkService({ eventBus });
    const viewer = new PDFViewer({ container: el, eventBus, linkService, annotationEditorMode: NONE });
    linkService.setViewer(viewer);
    eventBus.on("pagesinit", () => {
      viewer.currentScaleValue = "page-width";
    });
    viewerRef.current = viewer;
    setDirty(false);
    setMode(NONE);
    setError(null);

    api
      .readFileBytes(project, path)
      .then((bytes) => {
        if (!live) return null;
        task = pdfjs.getDocument({
          data: new Uint8Array(bytes),
          password: password ?? undefined,
          ...PDF_DATA,
        });
        return task.promise;
      })
      .then((doc) => {
        if (!live || !doc) return;
        setAsking(null);
        docRef.current = doc;
        // Typed as null in pdf.js, but it is the documented change hook.
        (doc.annotationStorage as unknown as { onSetModified: () => void }).onSetModified = () =>
          setDirty(true);
        viewer.setDocument(doc);
        linkService.setDocument(doc);
      })
      .catch((e: unknown) => {
        if (!live) return;
        if (e instanceof Error && e.name === "PasswordException") {
          setAsking(password === null ? "needed" : "wrong");
        } else {
          setError(String(e));
        }
      });

    return () => {
      live = false;
      viewerRef.current = null;
      docRef.current = null;
      void (task as pdfjs.PDFDocumentLoadingTask | null)?.destroy();
    };
  }, [project, path, nonce, password]);

  // Reload when the agent may have changed it, unless there is unsaved work.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current) return;
    seen.current = version;
    if (!dirtyRef.current) setNonce((n) => n + 1);
  }, [version]);

  const choose = (next: number) => {
    const viewer = viewerRef.current;
    if (!viewer || !docRef.current) return;
    viewer.annotationEditorMode = { mode: next };
    setMode(next);
  };

  const save = async () => {
    const doc = docRef.current;
    if (!doc) return;
    setSaving(true);
    try {
      const bytes = await doc.saveDocument();
      await api.writeFileBytes(project, path, bytes);
      // Reopen, so what was added becomes part of the file as shown.
      setNonce((n) => n + 1);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="file-viewer" aria-label={path}>
      {header(
        <>
          <span className="pdf-tools" role="group" aria-label="PDF tools">
            {TOOLS.map(([label, value, title]) => (
              <button
                key={label}
                className={mode === value ? "selected" : ""}
                aria-pressed={mode === value}
                title={title}
                onClick={() => choose(value)}
              >
                {label}
              </button>
            ))}
          </span>
          {dirty && <span className="muted unsaved">unsaved</span>}
          <button onClick={() => void save()} disabled={!dirty || saving}>
            {saving ? "Saving…" : "Save"}
          </button>
          <button
            onClick={() => setNonce((n) => n + 1)}
            title={dirty ? "Throw away your changes and reload from disk" : "Reload from disk"}
          >
            {dirty ? "Discard changes" : "Reload"}
          </button>
        </>,
      )}
      {error && <p className="failure">{error}</p>}
      {asking && (
        <form
          className="pdf-password"
          onSubmit={(e) => {
            e.preventDefault();
            setPassword(typed);
            setTyped("");
          }}
        >
          <label>
            {asking === "wrong" ? "That password is wrong. Try again:" : "This PDF needs a password:"}
            <input
              type="password"
              autoFocus
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              aria-label="PDF password"
            />
          </label>
          <button type="submit" disabled={!typed}>
            Open
          </button>
        </form>
      )}
      <div className="pdf-area">
        {/* Fresh DOM per load: a pdf.js viewer cannot take over another's. */}
        <div className="pdf-container" key={nonce} ref={container}>
          <div className="pdfViewer" />
        </div>
      </div>
    </section>
  );
}
