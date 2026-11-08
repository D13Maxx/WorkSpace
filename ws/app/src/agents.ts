// The agent view's model: which workers the latest turn started, and what
// each is doing, from the saved thread plus what is streaming in now.
import type { HelperEntry, HelperLog, Thread } from "./api";

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
  /** Tool results seen while the turn streams, by call id. */
  liveResults: Record<string, boolean>,
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
    const switched = [...entries].reverse().find((e) => e.kind === "switch");
    const result = dispatch in liveResults ? liveResults[dispatch] : failed.get(dispatch);

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
      model: switched?.kind === "switch" ? switched.to : model,
      status,
      steps: entries.filter((e) => e.kind === "call").length,
      entries,
      waitingOn,
    };
  });
}
