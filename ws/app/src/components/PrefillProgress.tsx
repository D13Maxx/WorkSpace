// Prefill progress with an estimate of the wait (master spec s11.1).
//
// A local model can spend several seconds reading a long prompt before the
// first word appears. Without this the app looks frozen, and the user's only
// signal that anything is happening is that it has not come back yet.
//
// The estimate is deliberately built on `done_tokens` alone. Tokens the KV
// cache already held cost no time at all, so counting them would make the
// first report wildly optimistic and every later one look like a slowdown.

interface Props {
  reusedTokens: number;
  doneTokens: number;
  totalTokens: number;
  /** `performance.now()` when prefill started. */
  startedAt: number;
}

/** Human-readable seconds, or null while there is nothing to go on. */
function estimateRemaining(
  doneTokens: number,
  toRead: number,
  startedAt: number,
): string | null {
  // A rate computed from the first handful of tokens is noise. Wait for a
  // chunk's worth before showing a number that will immediately change.
  if (doneTokens < 64 || doneTokens >= toRead) return null;

  const elapsed = (performance.now() - startedAt) / 1000;
  if (elapsed <= 0) return null;

  const rate = doneTokens / elapsed;
  if (rate <= 0) return null;

  const remaining = (toRead - doneTokens) / rate;
  if (!Number.isFinite(remaining) || remaining < 1) return null;
  if (remaining < 60) return `about ${Math.ceil(remaining)}s left`;
  return `about ${Math.ceil(remaining / 60)} min left`;
}

export function PrefillProgress({
  reusedTokens,
  doneTokens,
  totalTokens,
  startedAt,
}: Props) {
  const toRead = Math.max(totalTokens - reusedTokens, 0);
  // The bar tracks the whole prompt, so a mostly-cached prompt starts nearly
  // full -- which is the honest picture of how much work is left.
  const complete = totalTokens > 0 ? (reusedTokens + doneTokens) / totalTokens : 1;
  const percent = Math.min(Math.max(complete, 0), 1) * 100;
  const eta = estimateRemaining(doneTokens, toRead, startedAt);

  return (
    <div className="prefill" role="status" aria-live="polite">
      <div className="prefill-labels">
        <span>
          Reading prompt: {doneTokens.toLocaleString()} / {toRead.toLocaleString()} tokens
          {reusedTokens > 0 && (
            <span className="prefill-reused">
              {" "}
              ({reusedTokens.toLocaleString()} reused from cache)
            </span>
          )}
        </span>
        {eta && <span className="prefill-eta">{eta}</span>}
      </div>
      <div
        className="prefill-track"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={totalTokens}
        aria-valuenow={reusedTokens + doneTokens}
      >
        {reusedTokens > 0 && (
          <div
            className="prefill-bar prefill-bar-reused"
            style={{ width: `${(reusedTokens / Math.max(totalTokens, 1)) * 100}%` }}
          />
        )}
        <div className="prefill-bar" style={{ width: `${percent}%` }} />
      </div>
    </div>
  );
}
