// Git in the editor: a gutter marking lines that differ from the staged copy
// (click one to stage that change), and who last changed the cursor's line.

import { RangeSet, StateEffect, StateField, type Extension, type Text } from "@codemirror/state";
import {
  Decoration,
  EditorView,
  GutterMarker,
  ViewPlugin,
  WidgetType,
  gutter,
  showDialog,
  type DecorationSet,
  type ViewUpdate,
} from "@codemirror/view";
import { api, type BlameLine, type GitHunk } from "./api";

/** Editors that show git state, told when the index or commits change. */
const listeners = new Set<() => void>();

/** Say that staging, a commit or a save changed what git sees; every open
 *  editor checks again. */
export function gitChanged(): void {
  for (const l of listeners) l();
}

class Mark extends GutterMarker {
  constructor(readonly kind: GitHunk["kind"]) {
    super();
  }
  eq(other: Mark) {
    return other.kind === this.kind;
  }
  toDOM() {
    const el = document.createElement("div");
    el.className = `cm-git-${this.kind}`;
    el.title = { add: "Added", change: "Changed", delete: "Lines removed below" }[this.kind] + " since staged; click to stage";
    return el;
  }
}
const marks = { add: new Mark("add"), change: new Mark("change"), delete: new Mark("delete") };

/** The hunks as the server sent them, and their marks on the lines now. */
const setHunks = StateEffect.define<GitHunk[]>();
const hunkField = StateField.define<{ hunks: GitHunk[]; set: RangeSet<GutterMarker> }>({
  create: () => ({ hunks: [], set: RangeSet.empty }),
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setHunks)) return { hunks: e.value, set: build(e.value, tr.state.doc) };
    }
    return tr.docChanged ? { ...value, set: value.set.map(tr.changes) } : value;
  },
});

function build(hunks: GitHunk[], doc: Text): RangeSet<GutterMarker> {
  const ranges = [];
  for (const h of hunks) {
    if (h.kind === "delete") {
      ranges.push(marks.delete.range(doc.line(Math.min(Math.max(h.line, 1), doc.lines)).from));
      continue;
    }
    for (let n = h.line; n < h.line + h.count && n <= doc.lines; n++) ranges.push(marks[h.kind].range(doc.line(n).from));
  }
  return RangeSet.of(ranges, true);
}

/** The hunk shown on `line` (1-based), as its index in the list. */
function hunkAt(hunks: GitHunk[], line: number): number {
  return hunks.findIndex((h) =>
    h.kind === "delete" ? Math.max(h.line, 1) === line : line >= h.line && line < h.line + h.count,
  );
}

/** A short message in the editor's own dialog. */
function say(view: EditorView, message: string) {
  const { close, result } = showDialog(view, { label: message, submitLabel: "OK" });
  void result.then(() => view.dispatch({ effects: close }));
}

function stageDialog(view: EditorView, root: string, path: string, index: number, refresh: () => void) {
  const { close, result } = showDialog(view, {
    content: (_v, done) => {
      const box = document.createElement("div");
      box.className = "cm-lsp-actions";
      const stage = document.createElement("button");
      stage.type = "button";
      stage.textContent = "Stage this change";
      stage.onclick = () => {
        done();
        api
          .gitStageHunk(root, path, view.state.doc.toString(), index)
          .then(() => {
            refresh();
            gitChanged();
          })
          .catch((e) => say(view, `Not staged: ${String(e)}`));
      };
      box.append(stage);
      queueMicrotask(() => stage.focus());
      return box;
    },
  });
  void result.then(() => view.dispatch({ effects: close }));
}

/** The changed-lines gutter for `path` in `root`. */
function gitGutter(root: string, path: string): Extension {
  const refresher = ViewPlugin.fromClass(
    class {
      timer: ReturnType<typeof setTimeout> | null = null;
      off: () => void;
      constructor(readonly view: EditorView) {
        this.fetch();
        const again = () => this.fetch();
        listeners.add(again);
        this.off = () => listeners.delete(again);
      }
      fetch() {
        const doc = this.view.state.doc;
        api
          .gitHunks(root, path, doc.toString())
          // Not a repository, or git missing: no marks, quietly.
          .catch(() => [] as GitHunk[])
          .then((hunks) => {
            // Stale if the text moved on; the next check, already due, fixes it.
            if (this.view.state.doc === doc) this.view.dispatch({ effects: setHunks.of(hunks) });
          });
      }
      update(u: ViewUpdate) {
        if (!u.docChanged) return;
        if (this.timer) clearTimeout(this.timer);
        this.timer = setTimeout(() => this.fetch(), 400);
      }
      destroy() {
        if (this.timer) clearTimeout(this.timer);
        this.off();
      }
    },
  );
  return [
    hunkField,
    refresher,
    gutter({
      class: "cm-git-gutter",
      markers: (view) => view.state.field(hunkField).set,
      domEventHandlers: {
        mousedown(view, block) {
          const { hunks } = view.state.field(hunkField);
          const index = hunkAt(hunks, view.state.doc.lineAt(block.from).number);
          if (index < 0) return false;
          const plugin = view.plugin(refresher);
          stageDialog(view, root, path, index, () => plugin?.fetch());
          return true;
        },
      },
    }),
  ];
}

// ---- blame for the cursor's line ------------------------------------------

const BLAME_KEY = "ws.lineBlame";
const blameListeners = new Set<() => void>();

export function lineBlameEnabled(): boolean {
  try {
    return localStorage.getItem(BLAME_KEY) !== "0";
  } catch {
    return true;
  }
}

export function setLineBlameEnabled(on: boolean): void {
  try {
    localStorage.setItem(BLAME_KEY, on ? "1" : "0");
  } catch {
    // Storage blocked: it still applies until the app closes.
  }
  for (const l of blameListeners) l();
}

/** "3 days ago" for `seconds` since 1970. */
export function ago(seconds: number, now = Date.now() / 1000): string {
  const s = Math.max(0, now - seconds);
  const steps: [number, string][] = [
    [60, "second"],
    [60, "minute"],
    [24, "hour"],
    [30, "day"],
    [12, "month"],
    [Infinity, "year"],
  ];
  let n = s;
  for (const [size, unit] of steps) {
    if (n < size) {
      const whole = Math.floor(n);
      return unit === "second" ? "just now" : `${whole} ${unit}${whole === 1 ? "" : "s"} ago`;
    }
    n /= size;
  }
  return "long ago";
}

class BlameText extends WidgetType {
  constructor(readonly text: string) {
    super();
  }
  eq(other: BlameText) {
    return other.text === this.text;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-line-blame";
    el.textContent = this.text;
    return el;
  }
}

/** Who last changed the cursor's line, at its end. Shown only while that line
 *  reads as it did in the saved file, so unsaved edits never get stale blame. */
function lineBlame(root: string, path: string): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet = Decoration.none;
      blame: BlameLine[] = [];
      off: () => void;
      constructor(readonly view: EditorView) {
        const again = () => this.fetch();
        const redraw = () => this.redraw();
        listeners.add(again);
        blameListeners.add(redraw);
        this.off = () => {
          listeners.delete(again);
          blameListeners.delete(redraw);
        };
        this.fetch();
      }
      fetch() {
        api
          .gitBlame(root, path)
          .then((b) => {
            this.blame = b;
          })
          .catch(() => {
            this.blame = [];
          })
          .finally(() => this.redraw());
      }
      redraw() {
        // A plain transaction makes the view ask for decorations again.
        this.view.dispatch({});
      }
      update(u: ViewUpdate) {
        if (!u.docChanged && !u.selectionSet && u.transactions.length === 0) return;
        const state = u.state;
        const line = state.doc.lineAt(state.selection.main.head);
        const b = this.blame[line.number - 1];
        if (!lineBlameEnabled() || !b || b.text !== line.text) {
          this.decorations = Decoration.none;
          return;
        }
        const who = /^0+$/.test(b.commit) ? "Not committed yet" : `${b.author}, ${ago(b.time)} · ${b.summary}`;
        this.decorations = Decoration.set([Decoration.widget({ widget: new BlameText(who), side: 1 }).range(line.to)]);
      }
      destroy() {
        this.off();
      }
    },
    { decorations: (p) => p.decorations },
  );
}

/** Everything git adds to an editor for `path` in `root`. */
export function gitExtensions(root: string, path: string): Extension {
  return [gitGutter(root, path), lineBlame(root, path)];
}
