// Search across the project, and what changed since the last commit. Both sit
// under the file tree; a result opens in the middle pane.

import { useEffect, useState } from "react";
import { api, type GitChange, type SearchMatch } from "../api";
import type { OpenFile } from "./Explorer";

type View = "search" | "changes";

interface Props {
  /** The folder searched and diffed; empty with none open. */
  root: string;
  /** Bumped when files may have changed on disk. */
  version: number;
  onOpen: (file: OpenFile) => void;
}

export function SidePanel({ root, version, onOpen }: Props) {
  const [view, setView] = useState<View | null>(null);
  const toggle = (v: View) => setView((cur) => (cur === v ? null : v));
  return (
    <div className="side-panel">
      <div className="side-tabs" role="tablist">
        <button role="tab" aria-selected={view === "search"} className={view === "search" ? "selected" : ""} onClick={() => toggle("search")}>
          Search
        </button>
        <button role="tab" aria-selected={view === "changes"} className={view === "changes" ? "selected" : ""} onClick={() => toggle("changes")}>
          Changes
        </button>
      </div>
      {view && !root && <p className="muted side-note">Open a folder first.</p>}
      {view === "search" && root && <Search root={root} onOpen={onOpen} />}
      {view === "changes" && root && <Changes root={root} version={version} onOpen={onOpen} />}
    </div>
  );
}

function Search({ root, onOpen }: { root: string; onOpen: (f: OpenFile) => void }) {
  const [query, setQuery] = useState("");
  const [result, setResult] = useState<{ query: string; matches: SearchMatch[]; truncated: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // A search belongs to the folder it ran in.
  useEffect(() => {
    setResult(null);
    setError(null);
  }, [root]);

  const run = async () => {
    const q = query.trim();
    if (!q) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.searchProject(root, q);
      setResult({ query: q, ...r });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="side-body">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void run();
        }}
      >
        <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search the project" aria-label="Search the project" autoFocus />
      </form>
      {busy && <p className="muted side-note">Searching…</p>}
      {error && <p className="failure side-note">{error}</p>}
      {result && !busy && (
        <>
          <p className="muted side-note">
            {result.matches.length === 0
              ? `No lines contain "${result.query}".`
              : `${result.matches.length}${result.truncated ? "+" : ""} lines${result.truncated ? " (stopped at the limit; search for more)" : ""}`}
          </p>
          <ul className="side-list">
            {result.matches.map((m, i) => (
              <li key={`${m.path}:${m.line}:${i}`}>
                <button onClick={() => onOpen({ root, path: m.path, line: m.line })} title={`${m.path}:${m.line}`}>
                  <span className="side-where">
                    {m.path}:{m.line}
                  </span>
                  <span className="side-text">{m.text}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

const WORDS: Record<string, string> = { M: "modified", A: "added", D: "deleted", R: "renamed", "?": "new" };

function Changes({ root, version, onOpen }: { root: string; version: number; onOpen: (f: OpenFile) => void }) {
  const [changes, setChanges] = useState<GitChange[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let live = true;
    api
      .gitStatus(root)
      .then((c) => {
        if (!live) return;
        setChanges(c);
        setError(null);
      })
      .catch((e) => {
        if (!live) return;
        setChanges(null);
        setError(String(e));
      });
    return () => {
      live = false;
    };
  }, [root, version, nonce]);

  return (
    <div className="side-body">
      <div className="side-row">
        <span className="muted side-note">{changes ? `${changes.length} changed` : ""}</span>
        <button onClick={() => setNonce((n) => n + 1)} title="Check again">
          Refresh
        </button>
      </div>
      {error && <p className="failure side-note">{error}</p>}
      {changes && changes.length === 0 && <p className="muted side-note">Nothing has changed since the last commit.</p>}
      <ul className="side-list">
        {changes?.map((c) => (
          <li key={c.path}>
            <button onClick={() => onOpen({ root, path: c.path, diff: true })} title={`See what changed in ${c.path}`}>
              <span className={`git-status git-${c.status === "?" ? "new" : c.status}`} title={WORDS[c.status] ?? c.status}>
                {c.status}
              </span>
              <span className="side-text">{c.path}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
