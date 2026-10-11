import { useEffect } from "react";
import type { ContextView } from "../api";

interface Props {
  view: ContextView | null;
  onClose: () => void;
}

/** "What this model sees" (master spec s6.6). */
export function Inspector({ view, onClose }: Props) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
    <aside className="inspector" role="dialog" aria-modal="true" aria-label="What this model saw">
      <header>
        <h2>What this model saw</h2>
        <button onClick={onClose} aria-label="Close inspector">
          ✕
        </button>
      </header>

      {!view ? (
        <p className="muted">No context was recorded for this message.</p>
      ) : (
        <dl>
          <dt>Target</dt>
          <dd>{view.target.model_id}</dd>

          <dt>Mode</dt>
          <dd>
            {view.mode === "full"
              ? "Full history, sent verbatim"
              : "Summary of earlier messages, plus the recent ones"}
          </dd>

          <dt>Estimated input</dt>
          <dd>{view.estimated_input_tokens.toLocaleString()} tokens</dd>

          <dt>Messages sent verbatim</dt>
          <dd>{view.verbatim_messages.length}</dd>

          <dt>Provider-native blocks replayed</dt>
          <dd>
            {view.opaque_replayed
              ? "Yes — same model, full history"
              : "No — dropped, because this is not the model that produced them or the history was summarised"}
          </dd>

          {view.transformations.length > 0 && (
            <>
              <dt>Changes made to fit this model</dt>
              <dd>
                <ul>
                  {view.transformations.map((t, i) => (
                    <li key={i}>{JSON.stringify(t)}</li>
                  ))}
                </ul>
              </dd>
            </>
          )}
        </dl>
      )}
    </aside>
    </div>
  );
}
