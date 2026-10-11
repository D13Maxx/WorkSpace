// A PDF, viewed, annotated and edited with pdf.js, then saved back into the file.
//
// pdf.js renders into this page, not a plugin frame, so its editors (text
// boxes, freehand drawing, highlights) are available, and `saveDocument`
// returns the PDF with them written in as real annotations.
//
// Edit text changes words already on the page: click a line, retype it, and
// Save covers the old line with white and writes the new words in its place
// (pdf-lib). The old text is covered, not removed from the file.

import { useEffect, useRef, useState } from "react";
import { PDFDocument, StandardFonts, rgb } from "pdf-lib";
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
/** Not a pdf.js mode: FRIDAY's own text editing. */
const EDIT_TEXT = -100;
const TOOLS: [string, number, string][] = [
  ["Select", NONE, "Scroll, select text and move what you added"],
  ["Edit text", EDIT_TEXT, "Click a line of text on the page to change its words"],
  ["Text", FREETEXT, "Click on the page to add text"],
  ["Draw", INK, "Draw freehand"],
  ["Highlight", HIGHLIGHT, "Select text to highlight it"],
];

/** One line of the page's own text, retyped. Coordinates are PDF points. */
interface TextEdit {
  page: number;
  /** The box the old words fill, to cover. */
  box: [number, number, number, number];
  /** Where the line's baseline starts, and its size, for the new words. */
  x: number;
  y: number;
  size: number;
  family: "serif" | "sans-serif" | "monospace";
  original: string;
  text: string;
}

const FONTS = {
  serif: StandardFonts.TimesRoman,
  "sans-serif": StandardFonts.Helvetica,
  monospace: StandardFonts.Courier,
} as const;

/** Write `edits` into the PDF `bytes`: cover each old line, then draw the new one. */
async function applyEdits(bytes: Uint8Array, edits: TextEdit[]): Promise<Uint8Array> {
  const pdf = await PDFDocument.load(bytes);
  const fonts = new Map<string, Awaited<ReturnType<typeof pdf.embedFont>>>();
  for (const e of edits) {
    const page = pdf.getPage(e.page);
    const [x0, y0, x1, y1] = e.box;
    page.drawRectangle({ x: x0 - 1, y: y0 - 1, width: x1 - x0 + 2, height: y1 - y0 + 2, color: rgb(1, 1, 1) });
    if (!e.text) continue;
    let font = fonts.get(e.family);
    if (!font) {
      font = await pdf.embedFont(FONTS[e.family]);
      fonts.set(e.family, font);
    }
    // shortcut: the standard fonts cover Western European text only; embed a font file to write other scripts.
    page.drawText(e.text, { x: e.x, y: e.y, size: e.size, font, color: rgb(0, 0, 0) });
  }
  return pdf.save();
}

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
  // Edit text: the lines retyped so far, the one being typed, and a counter
  // bumped when pages re-render, so their boxes are placed again.
  const [edits, setEdits] = useState<TextEdit[]>([]);
  const [typing, setTyping] = useState<TextEdit | null>(null);
  const [draft, setDraft] = useState("");
  const [, setLayout] = useState(0);
  const changed = dirty || edits.length > 0;
  const dirtyRef = useRef(changed);
  dirtyRef.current = changed;

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
    eventBus.on("pagerendered", () => live && setLayout((n) => n + 1));
    viewerRef.current = viewer;
    setDirty(false);
    setMode(NONE);
    setError(null);
    setEdits([]);
    setTyping(null);

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
    viewer.annotationEditorMode = { mode: next === EDIT_TEXT ? NONE : next };
    setMode(next);
    setTyping(null);
  };

  /** Edit text: open the line under a click for retyping. */
  const pickLine = async (e: React.MouseEvent) => {
    const viewer = viewerRef.current;
    const doc = docRef.current;
    const pageEl = (e.target as Element).closest<HTMLElement>(".page");
    if (mode !== EDIT_TEXT || !viewer || !doc || !pageEl || typing) return;
    const page = Number(pageEl.dataset.pageNumber) - 1;
    const view = viewer.getPageView(page);
    const rect = pageEl.getBoundingClientRect();
    const [px, py] = view.viewport.convertToPdfPoint(
      e.clientX - rect.left - pageEl.clientLeft,
      e.clientY - rect.top - pageEl.clientTop,
    );
    const content = await (await doc.getPage(page + 1)).getTextContent();
    for (const item of content.items) {
      if (!("str" in item) || !item.str.trim()) continue;
      const [, , c, d, x, y] = item.transform as number[];
      const size = Math.hypot(c, d) || item.height;
      const box: [number, number, number, number] = [x, y - size * 0.25, x + item.width, y + size * 0.9];
      if (px < box[0] - 1 || px > box[2] + 1 || py < box[1] - 1 || py > box[3] + 1) continue;
      const style = content.styles[item.fontName]?.fontFamily ?? "";
      const family = style.includes("mono") ? "monospace" : style.includes("sans") ? "sans-serif" : style.includes("serif") ? "serif" : "sans-serif";
      const earlier = edits.find((ed) => ed.page === page && ed.x === x && ed.y === y);
      const open = earlier ?? { page, box, x, y, size, family, original: item.str, text: item.str };
      setDraft(open.text);
      setTyping(open);
      return;
    }
  };

  /** Where a PDF box sits inside the scrolling container, in CSS pixels. */
  const place = (edit: TextEdit) => {
    const viewer = viewerRef.current;
    const el = container.current;
    const pageEl = el?.querySelector<HTMLElement>(`.page[data-page-number="${edit.page + 1}"]`);
    if (!viewer || !el || !pageEl) return null;
    // pdf.js 6 converts points only: map the box's two corners.
    const viewport = viewer.getPageView(edit.page).viewport;
    const [vx0, vy0] = viewport.convertToViewportPoint(edit.box[0], edit.box[1]);
    const [vx1, vy1] = viewport.convertToViewportPoint(edit.box[2], edit.box[3]);
    const page = pageEl.getBoundingClientRect();
    const box = el.getBoundingClientRect();
    const left = page.left - box.left + el.scrollLeft + pageEl.clientLeft + Math.min(vx0, vx1);
    const top = page.top - box.top + el.scrollTop + pageEl.clientTop + Math.min(vy0, vy1);
    const height = Math.abs(vy1 - vy0);
    return {
      left,
      top,
      minWidth: Math.abs(vx1 - vx0),
      height,
      fontSize: (height / (edit.box[3] - edit.box[1])) * edit.size,
    };
  };

  const commit = (text: string) => {
    if (!typing) return;
    const done = { ...typing, text };
    setEdits((list) => {
      const rest = list.filter((ed) => !(ed.page === done.page && ed.x === done.x && ed.y === done.y));
      return text === done.original ? rest : [...rest, done];
    });
    setTyping(null);
  };

  const save = async () => {
    const doc = docRef.current;
    if (!doc) return;
    setSaving(true);
    try {
      let bytes: Uint8Array = await doc.saveDocument();
      if (edits.length) bytes = await applyEdits(bytes, edits);
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
                // pdf-lib cannot write into an encrypted PDF.
                disabled={value === EDIT_TEXT && password !== null}
              >
                {label}
              </button>
            ))}
          </span>
          {changed && <span className="muted unsaved">unsaved</span>}
          <button onClick={() => void save()} disabled={!changed || saving}>
            {saving ? "Saving…" : "Save"}
          </button>
          <button
            onClick={() => setNonce((n) => n + 1)}
            title={changed ? "Throw away your changes and reload from disk" : "Reload from disk"}
          >
            {changed ? "Discard changes" : "Reload"}
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
        <div
          className={`pdf-container ${mode === EDIT_TEXT ? "editing-text" : ""}`}
          key={nonce}
          ref={container}
          onClick={(e) => void pickLine(e)}
        >
          <div className="pdfViewer" />
          {/* Retyped lines, drawn over the page until saved. */}
          {edits
            .filter((ed) => ed !== typing)
            .map((ed) => {
              const at = place(ed);
              return at ? (
                <div key={`${ed.page}:${ed.x}:${ed.y}`} className={`pdf-edit ${ed.family}`} style={at}>
                  {ed.text}
                </div>
              ) : null;
            })}
          {typing &&
            (() => {
              const at = place(typing);
              return at ? (
                <input
                  className={`pdf-edit pdf-edit-input ${typing.family}`}
                  // As wide as its text, not an input's default 20 characters.
                  style={{ ...at, width: `calc(${Math.max(draft.length, 2)}ch + 6px)` }}
                  size={1}
                  autoFocus
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commit(e.currentTarget.value);
                    if (e.key === "Escape") setTyping(null);
                  }}
                  onBlur={(e) => commit(e.currentTarget.value)}
                  aria-label={`Replace "${typing.original}"`}
                />
              ) : null;
            })()}
        </div>
      </div>
    </section>
  );
}
