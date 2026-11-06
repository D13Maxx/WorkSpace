import { useState, type KeyboardEvent } from "react";

interface Props {
  /** `coordinate`: the model plans in .ws/plan.md and dispatches workers. */
  onSend: (text: string, coordinate: boolean) => void;
  onStop: () => void;
  busy: boolean;
}

export function Composer({ onSend, onStop, busy }: Props) {
  const [text, setText] = useState("");
  const [coordinate, setCoordinate] = useState(false);

  // With Coordinate on, an empty message carries on with the saved plan.
  const canSend = !!text.trim() || coordinate;

  const send = () => {
    if (!canSend || busy) return;
    setText("");
    onSend(text.trim(), coordinate);
  };

  // Master spec s11.1: Enter sends, Shift+Enter is a newline, Esc stops.
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    } else if (e.key === "Escape" && busy) {
      e.preventDefault();
      onStop();
    }
  };

  const placeholder = busy
    ? "Streaming… press Esc to stop"
    : coordinate
      ? "Task for the coordinator, or send empty to continue its plan"
      : "Send a message (Enter to send)";

  return (
    <div className="composer">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        rows={3}
        aria-label="Message"
      />
      <div className="composer-actions">
        <label
          className="coordinate-toggle"
          title="The model plans the task in .ws/plan.md, sends workers to do the parts side by side, checks their work and keeps the plan up to date"
        >
          <input type="checkbox" checked={coordinate} onChange={(e) => setCoordinate(e.target.checked)} />
          Coordinate
        </label>
        {busy ? (
          <button className="stop" onClick={onStop}>
            Stop
          </button>
        ) : (
          <button className="primary" onClick={send} disabled={!canSend}>
            Send
          </button>
        )}
      </div>
    </div>
  );
}
