import { useState } from "react";
import { api, type ThreadMessage } from "../api";

interface Props {
  messages: ThreadMessage[];
  streaming: string | null;
  streamingReasoning: string;
  onRegenerate: (message: string) => void;
  onInspect: (message: string) => void;
  onSelectBranch: (message: string) => void;
  onUndo: (callId: string) => void;
  busy: boolean;
}

/** A unified diff, each line tinted by what it does. */
function Diff({ text }: { text: string }) {
  return (
    <pre className="diff">
      {text.replace(/\n$/, "").split("\n").map((line, i) => {
        const kind = line.startsWith("+++") || line.startsWith("---")
          ? "file"
          : line.startsWith("@@")
            ? "hunk"
            : line.startsWith("+")
              ? "add"
              : line.startsWith("-")
                ? "del"
                : "ctx";
        return (
          <span key={i} className={`diff-${kind}`}>
            {/* Each line is a block, so an empty one still needs height. */}
            {line || " "}
          </span>
        );
      })}
    </pre>
  );
}

/**
 * Model output is rendered as plain text, never as HTML (master spec s12).
 * `textContent` assignment via React's children is what guarantees that: no
 * dangerouslySetInnerHTML appears anywhere in this file.
 */
function BranchNavigator({
  message,
  onSelectBranch,
}: {
  message: ThreadMessage;
  onSelectBranch: (id: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  if (message.branch_total <= 1) return null;

  const step = async (delta: number) => {
    setBusy(true);
    try {
      const siblings = await api.siblingsOf(message.id);
      const current = siblings.indexOf(message.id);
      const next = siblings[current + delta];
      if (next) onSelectBranch(next);
    } finally {
      setBusy(false);
    }
  };

  return (
    <span className="branches">
      <button
        disabled={busy || message.branch_index <= 1}
        onClick={() => void step(-1)}
        aria-label="Previous version"
      >
        ‹
      </button>
      <span>
        {message.branch_index}/{message.branch_total}
      </span>
      <button
        disabled={busy || message.branch_index >= message.branch_total}
        onClick={() => void step(1)}
        aria-label="Next version"
      >
        ›
      </button>
    </span>
  );
}

export function MessageList(props: Props) {
  return (
    <div className="messages">
      {props.messages.map((m) => (
        <article key={m.id} className={`message ${m.role} ${m.status}`}>
          <header>
            <span className="who">
              {m.role === "user" ? "You" : m.role === "tool" ? "Tool" : (m.model_id ?? "Assistant")}
            </span>
            <BranchNavigator message={m} onSelectBranch={props.onSelectBranch} />
            <span className="actions">
              <button onClick={() => void navigator.clipboard.writeText(m.text)} title="Copy">
                Copy
              </button>
              {m.role === "assistant" && (
                <>
                  <button
                    disabled={props.busy}
                    onClick={() => props.onRegenerate(m.id)}
                    title="Regenerate as a new version"
                  >
                    Regenerate
                  </button>
                  <button onClick={() => props.onInspect(m.id)} title="What this model saw">
                    Inspect
                  </button>
                </>
              )}
            </span>
          </header>

          {m.reasoning && (
            <details className="reasoning">
              <summary>Reasoning</summary>
              <pre>{m.reasoning}</pre>
            </details>
          )}

          {m.skill && (
            <details className="skill-use">
              <summary>/{m.skill.name}</summary>
              <pre>{m.skill.instructions}</pre>
            </details>
          )}
          {m.text && <div className="body">{m.text}</div>}

          {m.tool_calls.map((c) => (
            <div key={c.call_id} className="tool-call">
              <code>{c.name}</code> <span className="muted">{c.args}</span>
            </div>
          ))}
          {m.tool_results.map((r) => (
            <div key={r.call_id}>
              {r.edit && (
                <div className={`file-edit ${r.edit.undone ? "undone" : ""}`}>
                  <header>
                    <code>{r.edit.path}</code>
                    {r.edit.undone ? (
                      <span className="muted">Undone</span>
                    ) : (
                      <button onClick={() => props.onUndo(r.call_id)} disabled={props.busy}>
                        Undo
                      </button>
                    )}
                  </header>
                  <Diff text={r.edit.diff} />
                </div>
              )}
              <details className={`tool-result ${r.is_error ? "failure" : ""}`}>
                <summary>{r.is_error ? "Tool error" : "Tool result"}</summary>
                <pre>{r.text}</pre>
              </details>
            </div>
          ))}

          {m.status === "failed" && (
            <p className="failure">
              Stopped: {m.failure_reason ?? "unknown error"}. The text above is what arrived.
            </p>
          )}
          {m.status === "cancelled" && <p className="failure">Stopped by you.</p>}
        </article>
      ))}

      {props.streaming !== null && (
        <article className="message assistant streaming">
          <header>
            <span className="who">Assistant</span>
          </header>
          {props.streamingReasoning && (
            <details className="reasoning" open>
              <summary>Reasoning</summary>
              <pre>{props.streamingReasoning}</pre>
            </details>
          )}
          <div className="body">
            {props.streaming}
            <span className="caret" />
          </div>
        </article>
      )}
    </div>
  );
}
