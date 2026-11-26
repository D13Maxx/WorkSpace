// The agent view's model: which workers the latest turn started, and what
// each is doing, from the saved thread plus what is streaming in now.
import type { HelperEntry, HelperLog, TeamLine, Thread, WorkerCopy } from "./api";

export type WorkerStatus = "working" | "needs-you" | "retrying" | "done" | "failed" | "stopped";

export interface Worker {
  dispatch: string;
  agent: string;
  brief: string;
  model: string;
  status: WorkerStatus;
  steps: number;
  entries: HelperEntry[];
  /** The approval it is waiting on, if any. */
  waitingOn: { callId: string; name: string; args: string } | null;
  /** Its own copy of the project, when it had one. */
  copy: (WorkerCopy & { folder: string }) | null;
}

function arg(args: string, key: string): string {
  try {
    const v = JSON.parse(args)?.[key];
    return typeof v === "string" ? v : "";
  } catch {
    return "";
  }
}

/** The workers dispatched since the user's last message, oldest first. */
export function latestWorkers(
  thread: Thread | null,
  logs: Map<string, HelperLog>,
  /**
   * How each dispatch ended (failed?), as the stream or the team chat says;
   * null: still running, whatever its tool result says (a coordinated turn's
   * dispatch returns "Started" at once).
   */
  liveResults: Record<string, boolean | null>,
  approvals: { callId: string; name: string; args: string }[],
  busy: boolean,
): Worker[] {
  const messages = thread?.messages ?? [];
  let start = 0;
  messages.forEach((m, i) => {
    if (m.role === "user") start = i + 1;
  });
  const recent = messages.slice(start);

  // Dispatches the saved thread knows about, then ones only streaming so far.
  const briefs = new Map<string, string>();
  const failed = new Map<string, boolean>();
  for (const m of recent) {
    for (const c of m.tool_calls) if (c.name === "dispatch_agent") briefs.set(c.call_id, c.args);
    for (const r of m.tool_results) failed.set(r.call_id, r.is_error);
  }
  for (const id of logs.keys()) if (!briefs.has(id) && busy) briefs.set(id, "");

  const model = thread ? thread.model_id : "";
  return [...briefs].map(([dispatch, args]) => {
    const log = logs.get(dispatch);
    const entries = log?.entries ?? [];
    const answered = new Set(entries.flatMap((e) => (e.kind === "result" ? [e.call_id] : [])));
    const open = new Set(entries.flatMap((e) => (e.kind === "call" && !answered.has(e.call_id) ? [e.call_id] : [])));
    const waitingOn = approvals.find((a) => open.has(a.callId)) ?? null;
    // Its latest model: a switch, else the model it started on, else the chat's.
    const switched = [...entries].reverse().find((e) => e.kind === "switch" || e.kind === "model");
    const isolated = entries.find((e) => e.kind === "isolated");
    const result = dispatch in liveResults ? (liveResults[dispatch] ?? undefined) : failed.get(dispatch);

    let status: WorkerStatus;
    if (result !== undefined) status = result ? "failed" : "done";
    else if (!busy) status = "stopped";
    else if (waitingOn) status = "needs-you";
    else if (entries[entries.length - 1]?.kind === "retry") status = "retrying";
    else status = "working";

    return {
      dispatch,
      agent: log?.agent || arg(args, "agent") || "worker",
      brief: arg(args, "prompt") || args,
      model: switched?.kind === "switch" ? switched.to : switched?.kind === "model" ? switched.name : model,
      status,
      steps: entries.filter((e) => e.kind === "call").length,
      entries,
      waitingOn,
      copy:
        isolated?.kind === "isolated"
          ? { root: isolated.root, branch: isolated.branch, base: isolated.base, folder: isolated.folder }
          : null,
    };
  });
}

/** A worker at a glance: working, blocked on you or its model, idle between tasks, or finished. */
export type BoardStatus = "working" | "blocked" | "idle" | "done" | "failed" | "stopped";

export interface BoardRow {
  /** The worker's name: a role's copy, such as "implementer-2". */
  agent: string;
  status: BoardStatus;
  /** Why, in a few words, for blocked and idle. */
  why: string;
  model: string;
  /** Its latest task, whose pane the row opens. */
  dispatch: string;
  /** How many tasks it was given this turn. */
  tasks: number;
}

/**
 * One row per worker, from its latest task. A team role keeps its memory
 * between tasks, so while the team still runs, one that finished its task is
 * idle (free for the next), not done.
 */
export function board(workers: Worker[], busy: boolean): BoardRow[] {
  const rows = new Map<string, BoardRow>();
  for (const w of workers) {
    let status: BoardStatus;
    let why = "";
    if (w.status === "needs-you") [status, why] = ["blocked", "waiting for your approval"];
    else if (w.status === "retrying") [status, why] = ["blocked", "waiting to reach its model"];
    else if (w.status === "done" && busy) [status, why] = ["idle", "finished its task; free for another"];
    else status = w.status;
    const tasks = (rows.get(w.agent)?.tasks ?? 0) + 1;
    // Later tasks replace earlier ones: workers come oldest first.
    rows.set(w.agent, { agent: w.agent, status, why, model: w.model, dispatch: w.dispatch, tasks });
  }
  return [...rows.values()];
}

export interface TeamItem {
  id: string;
  at: string;
  what: TeamLine["what"];
  /** "Lead", or the worker's label, numbered when several share a name. */
  from: string;
  /** The worker it is from (or, for a brief, for); opens its pane. */
  dispatch: string;
  to: string;
  text: string;
}

/** Each worker's name in the team chat: "worker 1", "worker 2" when names repeat. */
export function workerLabels(workers: Worker[]): Map<string, string> {
  const count = new Map<string, number>();
  for (const w of workers) count.set(w.agent, (count.get(w.agent) ?? 0) + 1);
  const seen = new Map<string, number>();
  const out = new Map<string, string>();
  for (const w of workers) {
    const n = (seen.get(w.agent) ?? 0) + 1;
    seen.set(w.agent, n);
    out.set(w.dispatch, count.get(w.agent)! > 1 ? `${w.agent} ${n}` : w.agent);
  }
  return out;
}

/**
 * The latest turn's team chat: saved lines for its workers, then lines only
 * streamed so far (`live`, all from this turn), oldest first, with names.
 */
export function teamChat(saved: TeamLine[], live: TeamLine[], workers: Worker[]): TeamItem[] {
  const labels = workerLabels(workers);
  // The lead's own messages belong to no worker: kept from this turn's first line on.
  const first = saved.find((l) => labels.has(l.dispatch))?.at;
  const kept = saved.filter(
    (l) => labels.has(l.dispatch) || ((l.from === "lead" || l.from === "you") && first !== undefined && l.at >= first),
  );
  const ids = new Set(kept.map((l) => l.id));
  const lines = [...kept, ...live.filter((l) => !ids.has(l.id))];
  const name = (dispatch: string, fallback: string) => labels.get(dispatch) ?? fallback;
  // A note's `to` is whatever the worker wrote: a name, an id, or "name (id)".
  const target = (to: string | null, what: TeamLine["what"]) => {
    if (what === "question") return "You";
    if (what === "answer") return "Lead";
    if (to === null) return "everyone";
    if (to === "lead") return "Lead";
    for (const [dispatch, label] of labels) if (to.includes(dispatch)) return label;
    return to;
  };
  return lines.map((l) => ({
    id: l.id,
    at: l.at,
    what: l.what,
    from: l.what === "brief" || l.from === "lead" ? "Lead" : l.from === "you" ? "You" : name(l.dispatch, l.from),
    dispatch: l.dispatch,
    to: l.what === "brief" ? name(l.dispatch, l.to ?? "worker") : target(l.to, l.what),
    text: l.text,
  }));
}
