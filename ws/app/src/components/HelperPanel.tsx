import type { HelperEntry, HelperLog } from "../api";

/** A JSON argument, or "" when the arguments do not parse or lack it. */
function arg(args: string, key: string): string {
  try {
    const v = JSON.parse(args)?.[key];
    return typeof v === "string" ? v : "";
  } catch {
    return "";
  }
}

export function Step({ entry, result }: { entry: HelperEntry; result?: Extract<HelperEntry, { kind: "result" }> }) {
  switch (entry.kind) {
    case "text":
      return <div className="helper-text">{entry.text}</div>;
    case "switch":
      return (
        <div className="helper-note switched">
          <span className="muted">
            Switched from <code>{entry.from}</code> to <code>{entry.to}</code>
          </span>
          <pre>{entry.error}</pre>
        </div>
      );
    case "model":
      return (
        <div className="helper-note">
          <span className="muted">
            Working on <code>{entry.name}</code>
          </span>
        </div>
      );
    case "retry":
      return (
        <div className="helper-note switched">
          <span className="muted">
            {entry.error.includes("(429)") ? "Rate limited" : "Connection lost"}, trying again in {entry.wait_secs} s
          </span>
          <pre>{entry.error}</pre>
        </div>
      );
    case "isolated":
      return (
        <div className="helper-note">
          <span className="muted">
            Working in its own copy of the project, on branch <code>{entry.branch}</code>. Nothing reaches your files until you merge it.
          </span>
        </div>
      );
    case "received":
      return (
        <div className="helper-note received">
          <span className="muted">Heard from the others</span>
          <pre>{entry.text}</pre>
        </div>
      );
    case "call":
      if (entry.name === "post_message") {
        return (
          <div className="helper-note sent">
            <span className="muted">Told the others</span>
            <pre>{arg(entry.args, "text")}</pre>
          </div>
        );
      }
      return (
        <details className={`helper-call ${result?.is_error ? "failure" : ""}`}>
          <summary>
            <code>{entry.name}</code> <span className="muted">{entry.args}</span>
            {!result && <span className="muted"> · running…</span>}
          </summary>
          {result && <pre>{result.text}</pre>}
        </details>
      );
    case "result":
      return null;
  }
}

/** What one helper did for a dispatch_agent call: its brief, each step with
 *  its result, the notes it traded, and its report. Live while it runs. */
export function HelperPanel({ args, log, running }: { args: string; log?: HelperLog; running: boolean }) {
  const agent = log?.agent || arg(args, "agent") || "helper";
  const entries = log?.entries ?? [];
  const results = new Map(
    entries.flatMap((e) => (e.kind === "result" ? [[e.call_id, e] as const] : [])),
  );
  const steps = entries.filter((e) => e.kind === "call").length;
  return (
    <details className="helper-panel">
      <summary>
        <strong>{agent}</strong>{" "}
        <span className="muted">
          {steps} step{steps === 1 ? "" : "s"}
          {running && !log ? " · waiting" : ""}
        </span>
      </summary>
      <div className="helper-brief">
        <span className="muted">Brief</span>
        <pre>{arg(args, "prompt") || args}</pre>
      </div>
      {entries.map((e, i) => (
        <Step key={i} entry={e} result={e.kind === "call" ? results.get(e.call_id) : undefined} />
      ))}
      {entries.length === 0 && !running && <p className="muted">Nothing was recorded for this helper.</p>}
    </details>
  );
}
