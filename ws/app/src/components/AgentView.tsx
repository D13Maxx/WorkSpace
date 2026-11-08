import { useEffect, useRef } from "react";
import type { Worker, WorkerStatus } from "../agents";
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
  onClose,
}: {
  workers: Worker[];
  onAnswer: (callId: string, approved: boolean) => void;
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
            <Pane key={w.dispatch} worker={w} onAnswer={onAnswer} />
          ))}
        </div>
      )}
    </div>
  );
}

function Pane({ worker: w, onAnswer }: { worker: Worker; onAnswer: (callId: string, approved: boolean) => void }) {
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
