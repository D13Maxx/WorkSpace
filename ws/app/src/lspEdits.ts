// Language-server edits for files that are not open in an editor: rename and
// code actions touch them on disk. Kept apart from the client so it can be
// tested on its own.

export interface Position {
  line: number;
  /** UTF-16 code units, the server's default and JavaScript's own. */
  character: number;
}

export interface TextEdit {
  range: { start: Position; end: Position };
  newText: string;
}

/** The two shapes a server sends a workspace edit in, as edits per file URI. */
export function editsByUri(edit: {
  changes?: Record<string, TextEdit[]>;
  documentChanges?: ({ textDocument?: { uri: string }; edits?: TextEdit[]; kind?: string })[];
}): { byUri: Record<string, TextEdit[]>; skipped: string[] } {
  const byUri: Record<string, TextEdit[]> = { ...(edit.changes ?? {}) };
  const skipped: string[] = [];
  for (const d of edit.documentChanges ?? []) {
    if (d.textDocument && d.edits) {
      byUri[d.textDocument.uri] = [...(byUri[d.textDocument.uri] ?? []), ...d.edits];
    } else if (d.kind) {
      // Creating, renaming or deleting whole files is not done here.
      skipped.push(d.kind);
    }
  }
  return { byUri, skipped };
}

/** Where each line starts in `text`. A line ends at \n, \r\n or a lone \r. */
function lineStarts(text: string): number[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "\n" || (c === "\r" && text[i + 1] !== "\n")) starts.push(i + 1);
  }
  return starts;
}

/** `text` with `edits` applied. Positions refer to the text before any edit. */
export function applyTextEdits(text: string, edits: TextEdit[]): string {
  const starts = lineStarts(text);
  const offset = (p: Position) => {
    if (p.line >= starts.length) return text.length;
    const lineEnd = p.line + 1 < starts.length ? starts[p.line + 1] : text.length;
    // A position past the line's end stops before its line break.
    const content = text.slice(starts[p.line], lineEnd).replace(/\r?\n$|\r$/, "").length;
    return starts[p.line] + Math.min(p.character, content);
  };
  const resolved = edits
    .map((e) => ({ from: offset(e.range.start), to: offset(e.range.end), insert: e.newText }))
    // From the end, so earlier offsets stay right.
    .sort((a, b) => b.from - a.from || b.to - a.to);
  let out = text;
  for (const e of resolved) out = out.slice(0, e.from) + e.insert + out.slice(e.to);
  return out;
}

/** The file path a `file:` URI names, with `/` separators. Servers vary in
 *  how they encode it (`c%3A`, a lower-case drive), so it is decoded. */
export function uriPath(uri: string): string {
  return decodeURIComponent(uri.replace(/^file:\/\/\/?/, "")).replace(/\\/g, "/");
}

/** `path` relative to `root`, or null if it is outside it. Case is ignored,
 *  as Windows does. */
export function relativeTo(root: string, path: string): string | null {
  const r = root.replace(/\\/g, "/").replace(/\/+$/, "");
  const p = path.replace(/\\/g, "/");
  if (p.toLowerCase() === r.toLowerCase()) return "";
  return p.toLowerCase().startsWith(r.toLowerCase() + "/") ? p.slice(r.length + 1) : null;
}
