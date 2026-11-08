// The finder (Ctrl+P): files by default, `@` for symbols, `/` for text in
// files and `>` for commands, like a quick-open palette.

import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, type SearchMatch, type SymbolDef } from "../api";
import { best } from "../fuzzy";
import type { OpenFile } from "./Explorer";

export interface Command {
  /** Stable, for keymap.json. */
  id: string;
  label: string;
  run: () => void;
  /** Keys bound to it, shown beside it. */
  keys?: string[];
  /** Listed only when it applies (a tab to close, a split to leave). */
  when?: boolean;
}

interface Props {
  folders: string[];
  /** What the box starts with: "", "@", "/" or ">". */
  initial: string;
  commands: Command[];
  onOpen: (file: OpenFile) => void;
  onClose: () => void;
}

interface Row {
  key: string;
  main: string;
  detail: string;
  pick: () => void;
}

/** A folder's last name, to tell folders apart when several are open. */
const short = (root: string) => root.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? root;

export function Finder({ folders, initial, commands, onOpen, onClose }: Props) {
  const [text, setText] = useState(initial);
  const [at, setAt] = useState(0);
  const [files, setFiles] = useState<{ root: string; path: string }[] | null>(null);
  const [symbols, setSymbols] = useState<(SymbolDef & { root: string })[] | null>(null);
  const [hits, setHits] = useState<(SearchMatch & { root: string })[]>([]);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);

  const mode = text.startsWith(">") ? ">" : text.startsWith("@") ? "@" : text.startsWith("/") ? "/" : "";
  const query = (mode ? text.slice(1) : text).trim();
  const many = folders.length > 1;
  const where = (root: string, path: string) => (many ? `${short(root)}/${path}` : path);

  useEffect(() => input.current?.focus(), []);

  // Loaded the first time a mode needs them, then kept while the finder is open.
  useEffect(() => {
    if (mode === "" && !files) {
      Promise.all(folders.map((root) => api.projectFiles(root).then((ps) => ps.map((path) => ({ root, path })))))
        .then((all) => setFiles(all.flat()))
        .catch((e) => setError(String(e)));
    }
    if (mode === "@" && !symbols) {
      Promise.all(folders.map((root) => api.projectSymbols(root).then((ds) => ds.map((d) => ({ ...d, root })))))
        .then((all) => setSymbols(all.flat()))
        .catch((e) => setError(String(e)));
    }
  }, [mode, folders, files, symbols]);

  // Text search runs in the backend, a moment after typing stops.
  useEffect(() => {
    if (mode !== "/" || query.length < 2) {
      setHits([]);
      return;
    }
    const timer = setTimeout(() => {
      Promise.all(
        folders.map((root) => api.searchProject(root, query).then((r) => r.matches.map((m) => ({ ...m, root })))),
      )
        .then((all) => setHits(all.flat().slice(0, 100)))
        .catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(timer);
  }, [mode, query, folders]);

  const rows: Row[] = useMemo(() => {
    const open = (f: OpenFile) => () => {
      onClose();
      onOpen(f);
    };
    switch (mode) {
      case ">":
        return best(commands.filter((c) => c.when !== false), query, (c) => c.label).map((c) => ({
          key: c.id,
          main: c.label,
          detail: c.keys?.join(", ") ?? "",
          pick: () => {
            onClose();
            c.run();
          },
        }));
      case "@":
        return best(symbols ?? [], query, (d) => d.name).map((d) => ({
          key: `${d.root}|${d.path}|${d.line}`,
          main: d.name,
          detail: `${d.kind} · ${where(d.root, d.path)}:${d.line}`,
          pick: open({ root: d.root, path: d.path, line: d.line }),
        }));
      case "/":
        return hits.map((m) => ({
          key: `${m.root}|${m.path}|${m.line}`,
          main: m.text.trim(),
          detail: `${where(m.root, m.path)}:${m.line}`,
          pick: open({ root: m.root, path: m.path, line: m.line }),
        }));
      default:
        return best(files ?? [], query, (f) => f.path).map((f) => ({
          key: `${f.root}|${f.path}`,
          main: f.path.split("/").pop() ?? f.path,
          detail: where(f.root, f.path),
          pick: open({ root: f.root, path: f.path }),
        }));
    }
    // `where` depends only on `folders`, listed below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, query, files, symbols, hits, commands, folders, onOpen, onClose]);

  useEffect(() => setAt(0), [text]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      e.preventDefault();
      setAt((i) => Math.min(i + 1, rows.length - 1));
    } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      e.preventDefault();
      setAt((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      rows[at]?.pick();
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  const loading = (mode === "" && !files) || (mode === "@" && !symbols);
  const hint = !folders.length && mode !== ">"
    ? "Open a folder to find files in it. Type > for commands."
    : loading
      ? "Reading the project…"
      : mode === "/" && query.length < 2
        ? "Type at least two characters to search inside files."
        : rows.length === 0
          ? "Nothing matches."
          : null;

  return (
    <div className="modal-backdrop finder-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="finder" role="dialog" aria-modal="true" aria-label="Find">
        <input
          ref={input}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Files · @ symbols · / text in files · > commands"
          aria-label="Find"
          aria-controls="finder-results"
          aria-activedescendant={rows[at] ? `finder-${at}` : undefined}
          spellCheck={false}
        />
        {error && <p className="failure">{error}</p>}
        {hint && !error && <p className="muted finder-hint">{hint}</p>}
        <ul id="finder-results" role="listbox" className="finder-results">
          {rows.map((r, i) => (
            <li
              key={r.key}
              id={`finder-${i}`}
              role="option"
              aria-selected={i === at}
              className={i === at ? "active" : ""}
              onMouseMove={() => setAt(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                r.pick();
              }}
            >
              <span className="finder-main">{r.main}</span>
              {r.detail && <span className="finder-detail">{r.detail}</span>}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
