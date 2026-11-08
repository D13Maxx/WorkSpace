import { useCallback, useEffect, useRef, useState } from "react";
import type { Worker, WorkerStatus } from "../agents";
import { api, ask, type ChangedFile, type WorkerCopy } from "../api";
import { Step } from "./HelperPanel";
import { ToolArgs } from "./ToolArgs";

const LABEL: Record<WorkerStatus, string> = {
  working: "Working",
  "needs-you": "Needs you",
  retrying: "Reconnecting",
  done: "Done",
  failed: "Failed",
  stopped: "Stopped",
};

/** The latest turn's workers side by side, each with its live log. */
export function AgentView({
  workers,
  onAnswer,
  onMerged,
  onClose,
}: {
  workers: Worker[];
  onAnswer: (callId: string, approved: boolean) => void;
  /** A helper's changes reached the project's files. */
  onMerged: () => void;
  onClose: () => void;
}) {
  return (
    <div className="agent-view">
      <div className="group-bar agent-bar">
        <strong>Agents</strong>
        <span className="muted">
          {workers.length ? `${workers.length} from the latest message` : ""}
        </span>
        <span className="group-actions">
          <button onClick={onClose} title="Back to the editor" aria-label="Close agents">
            ✕
          </button>
        </span>
      </div>
      {workers.length === 0 ? (
        <div className="empty">
          <p>No agents yet. When the chat hands work to helpers, each one shows here as it runs.</p>
        </div>
      ) : (
        <div className="agent-grid">
          {workers.map((w) => (
            <Pane key={w.dispatch} worker={w} onAnswer={onAnswer} onMerged={onMerged} />
          ))}
        </div>
      )}
    </div>
  );
}

function Pane({
  worker: w,
  onAnswer,
  onMerged,
}: {
  worker: Worker;
  onAnswer: (callId: string, approved: boolean) => void;
  onMerged: () => void;
}) {
  const log = useRef<HTMLDivElement>(null);
  // Follow new steps, unless the user scrolled up to read.
  const pinned = useRef(true);
  useEffect(() => {
    const el = log.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [w.entries.length, w.waitingOn]);

  const results = new Map(w.entries.flatMap((e) => (e.kind === "result" ? [[e.call_id, e] as const] : [])));
  return (
    <section className={`agent-pane ${w.status}`} aria-label={`${w.agent}: ${LABEL[w.status]}`}>
      <header>
        <strong>{w.agent}</strong>
        <span className={`agent-status ${w.status}`}>{LABEL[w.status]}</span>
        <span className="muted agent-meta">
          {w.model} · {w.steps} step{w.steps === 1 ? "" : "s"}
        </span>
      </header>
      <details className="helper-brief">
        <summary className="muted">Brief</summary>
        <pre>{w.brief}</pre>
      </details>
      <div
        className="agent-log"
        ref={log}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {w.entries.map((e, i) => (
          <Step key={i} entry={e} result={e.kind === "call" ? results.get(e.call_id) : undefined} />
        ))}
        {w.entries.length === 0 && <p className="muted">{w.status === "working" ? "Starting…" : "Nothing recorded."}</p>}
      </div>
      {w.copy && (w.status === "working" || w.status === "needs-you" || w.status === "retrying") && (
        <p className="muted agent-copy">
          In its own copy, on <code>{w.copy.branch}</code>
        </p>
      )}
      {w.copy && (w.status === "done" || w.status === "failed" || w.status === "stopped") && (
        <CopyChanges copy={w.copy} agent={w.agent} onMerged={onMerged} />
      )}
      {w.waitingOn && (
        <div className="approval agent-approval" role="alertdialog" aria-label={`Approve ${w.agent}'s tool call`}>
          <p>
            Wants to run <code>{w.waitingOn.name}</code>:
          </p>
          <ToolArgs args={w.waitingOn.args} />
          <div className="row">
            <button className="primary" onClick={() => onAnswer(w.waitingOn!.callId, true)}>
              Allow
            </button>
            <button onClick={() => onAnswer(w.waitingOn!.callId, false)}>Deny</button>
          </div>
        </div>
      )}
    </section>
  );
}

/** A finished helper's work in its own copy: what changed, and Merge or Discard. */
function CopyChanges({ copy, agent, onMerged }: { copy: WorkerCopy; agent: string; onMerged: () => void }) {
  // undefined: loading; null: merged or discarded already.
  const [files, setFiles] = useState<ChangedFile[] | null | undefined>(undefined);
  const [diff, setDiff] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { root, branch, base } = copy;

  const refresh = useCallback(() => {
    api
      .workerChanges({ root, branch, base })
      .then(setFiles)
      .catch((e) => setNote(String(e)));
  }, [root, branch, base]);
  useEffect(refresh, [refresh]);

  const act = async (what: "merge" | "discard") => {
    if (what === "discard" && !(await ask(`Throw away ${agent}'s changes? This cannot be undone.`))) return;
    setBusy(true);
    try {
      if (what === "merge") {
        setNote(await api.workerMerge({ root, branch, base }));
        onMerged();
      } else {
        await api.workerDiscard({ root, branch, base });
        setNote("Discarded.");
      }
      setDiff(null);
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
      refresh();
    }
  };

  if (files === undefined) return null;
  return (
    <div className="agent-changes">
      {files === null ? (
        <p className="muted">{note ?? "Nothing to merge: it changed nothing, or its work was already merged or discarded."}</p>
      ) : (
        <>
          <p className="muted">
            {files.length} file{files.length === 1 ? "" : "s"} changed in its copy, not yet in your files:
          </p>
          <ul>
            {files.map((f) => (
              <li key={f.path}>
                <code>{f.path}</code>{" "}
                {f.added === null ? (
                  <span className="muted">binary</span>
                ) : (
                  <span>
                    <span className="added">+{f.added}</span> <span className="removed">−{f.removed}</span>
                  </span>
                )}
              </li>
            ))}
          </ul>
          {diff !== null && <pre className="agent-diff">{diff || "No differences."}</pre>}
          {note && <p className="muted">{note}</p>}
          <div className="row">
            <button className="primary" disabled={busy} onClick={() => void act("merge")}>
              Merge into my files
            </button>
            <button
              disabled={busy}
              onClick={() =>
                diff !== null
                  ? setDiff(null)
                  : void api.workerDiff({ root, branch, base }).then(setDiff, (e) => setNote(String(e)))
              }
            >
              {diff !== null ? "Hide diff" : "View diff"}
            </button>
            <button disabled={busy} onClick={() => void act("discard")}>
              Discard
            </button>
          </div>
        </>
      )}
    </div>
  );
}
