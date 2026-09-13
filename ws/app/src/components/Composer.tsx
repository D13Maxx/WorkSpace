import { useState, type KeyboardEvent } from "react";

interface Props {
  onSend: (text: string) => void;
  onStop: () => void;
  busy: boolean;
}

export function Composer({ onSend, onStop, busy }: Props) {
  const [text, setText] = useState("");

  const send = () => {
    const trimmed = text.trim();
    if (!trimmed || busy) return;
    setText("");
    onSend(trimmed);
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

  return (
    <div className="composer">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={busy ? "Streaming… press Esc to stop" : "Send a message (Enter to send)"}
        rows={3}
        aria-label="Message"
      />
      {busy ? (
        <button className="stop" onClick={onStop}>
          Stop
        </button>
      ) : (
        <button className="primary" onClick={send} disabled={!text.trim()}>
          Send
        </button>
      )}
    </div>
  );
}
