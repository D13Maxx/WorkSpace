// A CodeMirror editor for one file's text.
//
// The editor owns the document; the parent reads it through `viewRef` when
// saving, so typing never round-trips the whole file through React state.

import { useEffect, useRef } from "react";
import { EditorView, basicSetup } from "codemirror";
import { EditorState, type Extension } from "@codemirror/state";
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
import { oneDark } from "@codemirror/theme-one-dark";

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
      return html();
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
      return [];
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
}

export function CodeEditor({ text, ext, viewRef, onChange, onSave, onDispose }: Props) {
  const host = useRef<HTMLDivElement>(null);
  // Latest callbacks, without rebuilding the editor when they change.
  const handlers = useRef({ onChange, onSave, onDispose });
  handlers.current = { onChange, onSave, onDispose };

  useEffect(() => {
    if (!host.current) return;
    const view = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: text,
        extensions: [
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
    view.focus();
    return () => {
      handlers.current.onDispose(view.state.sliceDoc());
      view.destroy();
      if (viewRef.current === view) viewRef.current = null;
    };
  }, [text, ext, viewRef]);

  return <div className="code-editor" ref={host} />;
}
