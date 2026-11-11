// A CodeMirror editor for one file's text.
//
// The editor owns the document; the parent reads it through `viewRef` when
// saving, so typing never round-trips the whole file through React state.

import { useEffect, useRef } from "react";
import { EditorView, basicSetup } from "codemirror";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { StreamLanguage } from "@codemirror/language";
import { cpp } from "@codemirror/lang-cpp";
import { html } from "@codemirror/lang-html";
import { java } from "@codemirror/lang-java";
import { markdown } from "@codemirror/lang-markdown";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { stex } from "@codemirror/legacy-modes/mode/stex";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { yaml } from "@codemirror/legacy-modes/mode/yaml";
import { xml } from "@codemirror/legacy-modes/mode/xml";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { powerShell } from "@codemirror/legacy-modes/mode/powershell";
import { standardSQL } from "@codemirror/legacy-modes/mode/sql";
import { go } from "@codemirror/legacy-modes/mode/go";
import { ruby } from "@codemirror/legacy-modes/mode/ruby";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { swift } from "@codemirror/legacy-modes/mode/swift";
import { r } from "@codemirror/legacy-modes/mode/r";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { cmake } from "@codemirror/legacy-modes/mode/cmake";
import { protobuf } from "@codemirror/legacy-modes/mode/protobuf";
import { csharp, dart, kotlin, scala } from "@codemirror/legacy-modes/mode/clike";
import { groovy } from "@codemirror/legacy-modes/mode/groovy";
import { css, less, sCSS } from "@codemirror/legacy-modes/mode/css";
import { sass } from "@codemirror/legacy-modes/mode/sass";
import type { StreamParser } from "@codemirror/language";

/** Config, scripts and the rest, by extension: CodeMirror's simpler modes. */
const LEGACY: Record<string, StreamParser<unknown>> = {
  toml,
  // Cargo.lock is TOML.
  lock: toml,
  yaml,
  yml: yaml,
  xml,
  sh: shell,
  bash: shell,
  zsh: shell,
  ps1: powerShell,
  psm1: powerShell,
  sql: standardSQL,
  go,
  rb: ruby,
  lua,
  swift,
  r,
  ini: properties,
  cfg: properties,
  conf: properties,
  env: properties,
  editorconfig: properties,
  gitattributes: properties,
  gitignore: properties,
  dockerignore: properties,
  npmrc: properties,
  cmake,
  proto: protobuf,
  cs: csharp,
  kt: kotlin,
  kts: kotlin,
  scala,
  dart,
  gradle: groovy,
  css,
  scss: sCSS,
  less,
  sass,
} as Record<string, StreamParser<unknown>>;
import { oneDark } from "@codemirror/theme-one-dark";
import { vim } from "@replit/codemirror-vim";
import { onVimChange, setSaver, vimEnabled } from "../vim";

function language(ext: string): Extension {
  switch (ext) {
    case "c":
    case "h":
    case "cpp":
    case "cc":
    case "cxx":
    case "hpp":
      return cpp();
    case "java":
      return java();
    case "html":
    case "htm":
    case "vue":
    case "svelte":
      return html();
    case "jsx":
      return javascript({ jsx: true });
    case "tsx":
      return javascript({ jsx: true, typescript: true });
    case "js":
    case "mjs":
    case "cjs":
    // JSON is JavaScript's literal syntax, which is all a blueprint's text is.
    case "json":
    case "blueprint":
      return javascript();
    case "ts":
    case "mts":
    case "cts":
      return javascript({ typescript: true });
    case "md":
    case "markdown":
    case "mdx":
      // Prose: long lines wrap instead of scrolling sideways.
      return [markdown(), EditorView.lineWrapping];
    case "py":
      return python();
    case "rs":
      return rust();
    case "tex":
    case "sty":
    case "cls":
      return StreamLanguage.define(stex);
    default:
      return ext in LEGACY ? StreamLanguage.define(LEGACY[ext]) : [];
  }
}

const fill = EditorView.theme({
  "&": { height: "100%" },
  ".cm-scroller": { fontFamily: 'ui-monospace, "Cascadia Mono", Consolas, monospace' },
});

interface Props {
  /** The text to start from. A new value replaces the document. */
  text: string;
  ext: string;
  viewRef: React.MutableRefObject<EditorView | null>;
  onChange: () => void;
  onSave: () => void;
  /** The text as it was when the editor closed, to keep unsaved work. */
  onDispose: (text: string) => void;
  /** Move to this line (1-based); a new `lineKey` moves again, to the same line. */
  line?: number;
  lineKey?: unknown;
  /** More extensions (a language server, breakpoints); may change while open. */
  extensions?: Extension;
}

export function CodeEditor({ text, ext, viewRef, onChange, onSave, onDispose, line, lineKey, extensions }: Props) {
  const host = useRef<HTMLDivElement>(null);
  // Latest callbacks, without rebuilding the editor when they change.
  const handlers = useRef({ onChange, onSave, onDispose });
  handlers.current = { onChange, onSave, onDispose };
  const extra = useRef({ slot: new Compartment(), now: extensions });
  extra.current.now = extensions;
  const vimSlot = useRef(new Compartment());
  const vimKeys = (on: boolean) => (on ? vim({ status: true }) : []);

  useEffect(() => {
    if (!host.current) return;
    const view = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: text,
        extensions: [
          // Vim first: in normal mode it must see every key before anything else.
          vimSlot.current.of(vimKeys(vimEnabled())),
          // Then breakpoints, left of the line numbers, and server keys.
          extra.current.slot.of(extra.current.now ?? []),
          basicSetup,
          keymap.of([
            indentWithTab,
            {
              key: "Mod-s",
              preventDefault: true,
              run: () => {
                handlers.current.onSave();
                return true;
              },
            },
          ]),
          // Saving keeps a Windows file's CRLF line endings.
          text.includes("\r\n") ? EditorState.lineSeparator.of("\r\n") : [],
          language(ext),
          oneDark,
          fill,
          EditorView.updateListener.of((u) => {
            if (u.docChanged) handlers.current.onChange();
          }),
        ],
      }),
    });
    viewRef.current = view;
    setSaver(view, () => handlers.current.onSave());
    const stopVim = onVimChange((on) =>
      view.dispatch({ effects: vimSlot.current.reconfigure(vimKeys(on)) }),
    );
    view.focus();
    return () => {
      stopVim();
      handlers.current.onDispose(view.state.sliceDoc());
      view.destroy();
      if (viewRef.current === view) viewRef.current = null;
    };
  }, [text, ext, viewRef]);

  useEffect(() => {
    viewRef.current?.dispatch({ effects: extra.current.slot.reconfigure(extensions ?? []) });
  }, [extensions, viewRef]);

  // Runs after the effect above, so on first show the editor exists. Not tied
  // to `text`: a reload from disk must not yank the cursor back to a search hit.
  useEffect(() => {
    const view = viewRef.current;
    if (!view || !line) return;
    const at = view.state.doc.line(Math.min(Math.max(line, 1), view.state.doc.lines));
    view.dispatch({ selection: { anchor: at.from }, effects: EditorView.scrollIntoView(at.from, { y: "center" }) });
    view.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [line, lineKey, viewRef]);

  return <div className="code-editor" ref={host} />;
}
