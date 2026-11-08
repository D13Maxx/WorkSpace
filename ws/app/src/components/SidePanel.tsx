// Search across the project, and what changed since the last commit. Both sit
// under the file tree; a result opens in the middle pane.

import { useEffect, useState } from "react";
import { api, type GitChange, type SearchMatch } from "../api";
import type { OpenFile } from "./Explorer";
import { gitChanged } from "../git";

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
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);

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

  /** Run a git step, then look again here and in every open editor. */
  const act = (step: () => Promise<unknown>, after?: (result: unknown) => void) => {
    setBusy(true);
    setDone(null);
    step()
      .then((r) => {
        after?.(r);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => {
        setBusy(false);
        setNonce((n) => n + 1);
        gitChanged();
      });
  };

  const staged = changes?.filter((c) => c.staged) ?? [];
  const unstaged = changes?.filter((c) => c.unstaged) ?? [];
  const row = (c: GitChange, action: "stage" | "unstage") => (
    <li key={`${action}-${c.path}`} className="git-row">
      <button onClick={() => onOpen({ root, path: c.path, diff: true })} title={`See what changed in ${c.path}`}>
        <span className={`git-status git-${c.status === "?" ? "new" : c.status}`} title={WORDS[c.status] ?? c.status}>
          {c.status}
        </span>
        <span className="side-text">{c.path}</span>
      </button>
      <button
        className="git-act"
        disabled={busy}
        onClick={() => act(() => (action === "stage" ? api.gitStage(root, c.path) : api.gitUnstage(root, c.path)))}
        aria-label={`${action === "stage" ? "Stage" : "Unstage"} ${c.path}`}
        title={action === "stage" ? "Stage for the next commit" : "Take out of the next commit (the change stays)"}
      >
        {action === "stage" ? "+" : "−"}
      </button>
    </li>
  );

  return (
    <div className="side-body">
      <div className="git-commit">
        <textarea
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && e.ctrlKey && staged.length && message.trim()) {
              e.preventDefault();
              act(() => api.gitCommit(root, message), (id) => {
                setMessage("");
                setDone(`Committed ${String(id)}.`);
              });
            }
          }}
          placeholder="Commit message (Ctrl+Enter commits)"
          rows={2}
          aria-label="Commit message"
        />
        <button
          className="primary"
          disabled={busy || !staged.length || !message.trim()}
          title={staged.length ? "Commit what is staged" : "Stage something first"}
          onClick={() =>
            act(() => api.gitCommit(root, message), (id) => {
              setMessage("");
              setDone(`Committed ${String(id)}.`);
            })
          }
        >
          Commit {staged.length ? `(${staged.length})` : ""}
        </button>
      </div>
      {done && <p className="ok side-note">{done}</p>}
      {error && <p className="failure side-note">{error}</p>}
      <div className="side-row">
        <span className="muted side-note">{changes ? `${changes.length} changed` : ""}</span>
        <span>
          {unstaged.length > 0 && (
            <button disabled={busy} onClick={() => act(() => api.gitStage(root, "."))} title="Stage every change">
              Stage all
            </button>
          )}
          <button onClick={() => setNonce((n) => n + 1)} title="Check again">
            Refresh
          </button>
        </span>
      </div>
      {changes && changes.length === 0 && <p className="muted side-note">Nothing has changed since the last commit.</p>}
      {staged.length > 0 && (
        <>
          <h4 className="side-heading">Staged</h4>
          <ul className="side-list">{staged.map((c) => row(c, "unstage"))}</ul>
        </>
      )}
      {unstaged.length > 0 && (
        <>
          {staged.length > 0 && <h4 className="side-heading">Changes</h4>}
          <ul className="side-list">{unstaged.map((c) => row(c, "stage"))}</ul>
        </>
      )}
    </div>
  );
}
