import { useCallback, useEffect, useRef, useState } from "react";
import { workerLabels, type TeamItem, type Worker, type WorkerStatus } from "../agents";
import { api, ask, type ChangedFile, type WorkerCopy } from "../api";
import { Step } from "./HelperPanel";
import { ToolArgs } from "./ToolArgs";

const LABEL: Record<WorkerStatus, string> = {
  working: "Working",
  "needs-you": "Needs you",
  retrying: "Waiting to retry",
  done: "Done",
  failed: "Failed",
  stopped: "Stopped",
};

/** The latest turn's team: its chat, or its workers side by side, each with its live log. */
export function AgentView({
  workers,
  team,
  questions,
  onReply,
  onNote,
  onAnswer,
  onMerged,
  onClose,
}: {
  workers: Worker[];
  team: TeamItem[];
  /** The lead's questions waiting on the user. */
  questions: { callId: string; text: string }[];
  onReply: (callId: string, text: string) => void;
  /** Tell the team something; only while a team is working. */
  onNote?: (text: string) => Promise<void>;
  onAnswer: (callId: string, approved: boolean) => void;
  /** A helper's changes reached the project's files. */
  onMerged: () => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<"team" | "workers">("team");
  const labels = workerLabels(workers);
  const openPane = (dispatch: string) => {
    setTab("workers");
    // After the grid renders.
    requestAnimationFrame(() =>
      document.getElementById(`pane-${dispatch}`)?.scrollIntoView({ block: "nearest", behavior: "smooth" }),
    );
  };
  return (
    <div className="agent-view">
      <div className="group-bar agent-bar">
        <strong>Agents</strong>
        <span className="agent-tabs" role="tablist">
          <button role="tab" aria-selected={tab === "team"} className={tab === "team" ? "active" : ""} onClick={() => setTab("team")}>
            Team chat
          </button>
          <button
            role="tab"
            aria-selected={tab === "workers"}
            className={tab === "workers" ? "active" : ""}
            onClick={() => setTab("workers")}
          >
            Workers
          </button>
        </span>
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
      ) : tab === "team" ? (
        <TeamChat
          items={team}
          workers={workers}
          labels={labels}
          questions={questions}
          onReply={onReply}
          onNote={onNote}
          onAnswer={onAnswer}
          onOpen={openPane}
        />
      ) : (
        <div className="agent-grid">
          {workers.map((w) => (
            <Pane key={w.dispatch} worker={w} label={labels.get(w.dispatch) ?? w.agent} onAnswer={onAnswer} onMerged={onMerged} />
          ))}
        </div>
      )}
    </div>
  );
}

const WHAT: Record<TeamItem["what"], string> = {
  brief: "brief",
  note: "",
  report: "report",
  failed: "failed",
  question: "question",
  answer: "answer",
};

/** What the team said, in order, following new lines unless the user scrolled up. */
function TeamChat({
  items,
  workers,
  labels,
  questions,
  onReply,
  onNote,
  onAnswer,
  onOpen,
}: {
  items: TeamItem[];
  workers: Worker[];
  labels: Map<string, string>;
  questions: { callId: string; text: string }[];
  onReply: (callId: string, text: string) => void;
  onNote?: (text: string) => Promise<void>;
  onAnswer: (callId: string, approved: boolean) => void;
  onOpen: (dispatch: string) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const waiting = workers.filter((w) => w.waitingOn);
  useEffect(() => {
    const el = box.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [items.length, waiting.length, questions.length]);
  const working = workers.filter((w) => w.status === "working" || w.status === "retrying");

  return (
    <>
    <div
      className="team-chat"
      ref={box}
      aria-live="polite"
      onScroll={(e) => {
        const el = e.currentTarget;
        pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
      }}
    >
      {items.length === 0 && <p className="muted">Nothing said yet. Briefs, messages and reports show here as they happen.</p>}
      {items.map((m) => (
        <article key={m.id} className={`team-line ${m.what} ${m.from === "Lead" ? "lead" : ""}`}>
          <header>
            {m.dispatch ? (
              <button className="linklike team-who" onClick={() => onOpen(m.dispatch)} title="Open this worker's pane">
                {m.from}
              </button>
            ) : (
              <strong className="team-who">{m.from}</strong>
            )}
            <span className="muted">→ {m.to}</span>
            {WHAT[m.what] && <span className={`team-what ${m.what}`}>{WHAT[m.what]}</span>}
            <time className="muted" dateTime={m.at}>
              {new Date(m.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
            </time>
          </header>
          <TeamText text={m.text} />
        </article>
      ))}
      {waiting.map((w) => (
        <Approval key={w.dispatch} worker={w} label={labels.get(w.dispatch) ?? w.agent} onAnswer={onAnswer} />
      ))}
      {questions.map((q) => (
        <Reply key={q.callId} label="The lead asks you" text={q.text} action="Answer" onSend={(a) => onReply(q.callId, a)} />
      ))}
      {working.length > 0 && (
        <p className="muted team-typing">
          {working.map((w) => labels.get(w.dispatch) ?? w.agent).join(", ")} {working.length === 1 ? "is" : "are"} working…
        </p>
      )}
    </div>
    {onNote && <Say onNote={onNote} />}
    </>
  );
}

/** An answer to the lead's question; the team keeps working meanwhile. */
function Reply({
  label,
  text,
  action,
  onSend,
}: {
  label: string;
  text: string;
  action: string;
  onSend: (text: string) => void;
}) {
  const [draft, setDraft] = useState("");
  return (
    <form
      className="approval agent-approval team-question"
      role="alertdialog"
      aria-label={label}
      onSubmit={(e) => {
        e.preventDefault();
        if (draft.trim()) onSend(draft.trim());
      }}
    >
      <p>
        <strong>{label}:</strong> {text}
      </p>
      <textarea value={draft} onChange={(e) => setDraft(e.target.value)} rows={2} aria-label="Your answer" autoFocus />
      <div className="row">
        <button className="primary" type="submit" disabled={!draft.trim()}>
          {action}
        </button>
      </div>
    </form>
  );
}

/** Tell the whole team something while it works; the lead reads it first. */
function Say({ onNote }: { onNote: (text: string) => Promise<void> }) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const send = () => {
    const text = draft.trim();
    if (!text) return;
    onNote(text).then(
      () => {
        setDraft("");
        setError(null);
      },
      (e) => setError(String(e)),
    );
  };
  return (
    <div className="team-say">
      <textarea
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            send();
          }
        }}
        rows={1}
        placeholder="Tell the team…"
        title="The lead reads it first, then every worker"
        aria-label="Tell the team"
      />
      <button onClick={send} disabled={!draft.trim()}>
        Send
      </button>
      {error && <p className="failure">{error}</p>}
    </div>
  );
}

/** Long briefs and reports start folded. */
function TeamText({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const long = text.length > 600;
  return (
    <>
      <pre className="team-text">{long && !open ? `${text.slice(0, 600)}…` : text}</pre>
      {long && (
        <button className="linklike" onClick={() => setOpen(!open)}>
          {open ? "Show less" : "Show all"}
        </button>
      )}
    </>
  );
}

function Approval({
  worker: w,
  label,
  onAnswer,
}: {
  worker: Worker;
  label: string;
  onAnswer: (callId: string, approved: boolean) => void;
}) {
  if (!w.waitingOn) return null;
  const { callId, name, args } = w.waitingOn;
  return (
    <div className="approval agent-approval" role="alertdialog" aria-label={`Approve ${label}'s tool call`}>
      <p>
        {label} wants to run <code>{name}</code>:
      </p>
      <ToolArgs args={args} />
      <div className="row">
        <button className="primary" onClick={() => onAnswer(callId, true)}>
          Allow
        </button>
        <button onClick={() => onAnswer(callId, false)}>Deny</button>
      </div>
    </div>
  );
}

function Pane({
  worker: w,
  label,
  onAnswer,
  onMerged,
}: {
  worker: Worker;
  label: string;
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
    <section id={`pane-${w.dispatch}`} className={`agent-pane ${w.status}`} aria-label={`${label}: ${LABEL[w.status]}`}>
      <header>
        <strong>{label}</strong>
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
      <Approval worker={w} label={label} onAnswer={onAnswer} />
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
