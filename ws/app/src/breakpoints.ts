// Breakpoints: a gutter in the editor to set them, and a store the debugger
// reads, so they outlive the editor showing their file.

import { RangeSet, StateEffect, StateField, type EditorState, type Extension } from "@codemirror/state";
import { EditorView, GutterMarker, gutter } from "@codemirror/view";

const byFile = new Map<string, number[]>();
const listeners = new Set<() => void>();
const keyOf = (root: string, path: string) => `${root}\n${path}`;

/** Every file's breakpoints (1-based lines), including files whose last one
 *  was removed: the debugger must be told those too. */
export function allBreakpoints(): { root: string; path: string; lines: number[] }[] {
  return [...byFile].map(([k, lines]) => {
    const [root, path] = k.split("\n");
    return { root, path, lines };
  });
}

/** Called whenever any breakpoint changes; returns the unsubscribe. */
export function onBreakpoints(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

function save(root: string, path: string, lines: number[]) {
  const before = byFile.get(keyOf(root, path)) ?? [];
  if (before.length === lines.length && before.every((l, i) => l === lines[i])) return;
  byFile.set(keyOf(root, path), lines);
  for (const f of listeners) f();
}

const toggle = StateEffect.define<number>({ map: (pos, mapping) => mapping.mapPos(pos) });

const marker = new (class extends GutterMarker {
  toDOM() {
    const dot = document.createElement("span");
    dot.className = "cm-breakpoint";
    dot.textContent = "●";
    return dot;
  }
})();

const field = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update(set, tr) {
    set = set.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(toggle)) continue;
      let on = false;
      set.between(e.value, e.value, () => {
        on = true;
      });
      set = on ? set.update({ filter: (from) => from !== e.value }) : set.update({ add: [marker.range(e.value)] });
    }
    return set;
  },
});

function linesOf(state: EditorState): number[] {
  const lines = new Set<number>();
  const set = state.field(field);
  set.between(0, state.doc.length, (from) => {
    lines.add(state.doc.lineAt(from).number);
  });
  return [...lines].sort((a, b) => a - b);
}

/** The breakpoint gutter for `path` in `root`: click a line number's left to
 *  set or clear one. Lines move with edits, and the store follows. */
export function breakpointGutter(root: string, path: string): Extension {
  return [
    field.init((state) => {
      const lines = byFile.get(keyOf(root, path)) ?? [];
      return RangeSet.of(
        lines.filter((l) => l <= state.doc.lines).map((l) => marker.range(state.doc.line(l).from)),
        true,
      );
    }),
    gutter({
      class: "cm-breakpoint-gutter",
      markers: (v) => v.state.field(field),
      initialSpacer: () => marker,
      domEventHandlers: {
        mousedown(view, line) {
          view.dispatch({ effects: toggle.of(line.from) });
          return true;
        },
      },
    }),
    EditorView.updateListener.of((u) => {
      if (u.startState.field(field) !== u.state.field(field) || u.docChanged) save(root, path, linesOf(u.state));
    }),
  ];
}
