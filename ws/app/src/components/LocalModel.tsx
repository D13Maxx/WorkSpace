// Choosing and loading a local GGUF (master spec s8, s11.1).
//
// The flow is inspect-then-load on purpose. Loading a model takes seconds and
// gigabytes; reading its header takes neither. So the user sees what a file
// is, whether this build supports it, and whether it will fit in memory,
// before anything is committed.
//
// The path is typed or picked with the native dialog, which only returns a
// path; the webview reads nothing itself (master spec s11.2). The backend
// validates whatever arrives.

import { useCallback, useEffect, useState } from "react";
import { api, pickFile, type LoadedModel, type LocalModelInfo } from "../api";

function bytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${n} B` : `${value.toFixed(1)} ${units[unit]}`;
}

export function LocalModelSection({ onChanged }: { onChanged: () => void }) {
  const [path, setPath] = useState("");
  const [inspected, setInspected] = useState<LocalModelInfo | null>(null);
  const [loaded, setLoaded] = useState<LoadedModel | null>(null);
  const [contextCap, setContextCap] = useState<number | "">("");
  const [busy, setBusy] = useState<null | "inspecting" | "loading" | "unloading">(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .localModelStatus()
      .then(setLoaded)
      .catch(() => setLoaded(null));
  }, []);

  useEffect(refresh, [refresh]);

  const inspect = async () => {
    setError(null);
    setInspected(null);
    setBusy("inspecting");
    try {
      const info = await api.inspectLocalModel(path.trim());
      setInspected(info);
      // Offer the model's own window as the cap when it is smaller than ours.
      setContextCap("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const load = async () => {
    setError(null);
    setBusy("loading");
    try {
      const info = await api.loadLocalModel(
        (inspected?.path ?? path).trim(),
        contextCap === "" ? undefined : contextCap,
      );
      setLoaded(info);
      setInspected(null);
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const unload = async () => {
    setError(null);
    setBusy("unloading");
    try {
      await api.unloadLocalModel();
      setLoaded(null);
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section>
      <h3>Local model</h3>
      <p className="muted">
        A GGUF file runs on this machine, with no API key and no network. Point at the file and
        it joins the model picker like any other.
      </p>

      {loaded ? (
        <div className="local-loaded">
          <div className="local-title">
            <strong>{loaded.file_name}</strong>
            <button onClick={() => void unload()} disabled={busy !== null}>
              {busy === "unloading" ? "Unloading…" : "Unload"}
            </button>
          </div>
          <dl className="local-facts">
            <div>
              <dt>Context</dt>
              <dd>
                {loaded.context_cap.toLocaleString()} tokens
                {loaded.model_context_length !== null &&
                  loaded.model_context_length > loaded.context_cap && (
                    <span className="muted">
                      {" "}
                      (the file supports {loaded.model_context_length.toLocaleString()})
                    </span>
                  )}
              </dd>
            </div>
            <div>
              <dt>Quantisation</dt>
              <dd>{loaded.quant_summary}</dd>
            </div>
            <div>
              <dt>Memory</dt>
              <dd>
                {bytes(loaded.weights_bytes)} weights + {bytes(loaded.kv_cache_bytes)} cache
              </dd>
            </div>
            <div>
              <dt>Threads</dt>
              <dd>{loaded.threads}</dd>
            </div>
          </dl>
          {!loaded.truncatable_cache && (
            <p className="muted">
              Editing or regenerating an earlier message re-reads the whole prompt in this
              build. Continuing a conversation forward does not.
            </p>
          )}
          {loaded.ignored_rope_scaling && (
            <p className="failure">
              This model asks for RoPE position scaling ({loaded.ignored_rope_scaling}) that
              this build does not apply. It will answer, and short exchanges look fine, but
              quality falls off as the conversation grows.
            </p>
          )}
          {loaded.context_cap >= loaded.max_context_cap && (
            <p className="muted">
              {loaded.max_context_cap.toLocaleString()} tokens is the most this build can
              address.
            </p>
          )}
        </div>
      ) : (
        <>
          <div className="local-picker">
            <input
              type="text"
              value={path}
              placeholder={"C:\\models\\smollm2-1.7b-instruct-q4_k_m.gguf"}
              onChange={(e) => setPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && path.trim()) void inspect();
              }}
              spellCheck={false}
            />
            <button
              onClick={async () => {
                const file = await pickFile("GGUF model", ["gguf"]);
                if (file) setPath(file);
              }}
              disabled={busy !== null}
            >
              Browse…
            </button>
            <button onClick={() => void inspect()} disabled={!path.trim() || busy !== null}>
              {busy === "inspecting" ? "Reading…" : "Read file"}
            </button>
          </div>

          {inspected && (
            <div className="local-inspected">
              <dl className="local-facts">
                <div>
                  <dt>Architecture</dt>
                  <dd>
                    {inspected.architecture}
                    {!inspected.supported && <span className="failure"> — not supported</span>}
                  </dd>
                </div>
                <div>
                  <dt>Size</dt>
                  <dd>
                    {bytes(inspected.file_size_bytes)} ({inspected.quant_summary})
                  </dd>
                </div>
                <div>
                  <dt>Context</dt>
                  <dd>
                    {inspected.context_length?.toLocaleString() ?? "not stated"}
                  </dd>
                </div>
                <div>
                  <dt>Needs</dt>
                  <dd>
                    {bytes(inspected.estimated_bytes)} of {bytes(inspected.available_bytes)} free
                    {!inspected.fits && (
                      <span className="failure"> — lower the context cap</span>
                    )}
                  </dd>
                </div>
              </dl>

              {inspected.ignored_rope_scaling && (
                <p className="failure">
                  Needs RoPE position scaling ({inspected.ignored_rope_scaling}) that this
                  build does not apply. It will load and answer, but quality degrades as the
                  conversation grows — a model without rope scaling is a better test.
                </p>
              )}
              {inspected.unsupported_reason && (
                <p className="failure">{inspected.unsupported_reason}</p>
              )}
              {!inspected.has_chat_template && (
                <p className="failure">
                  This file carries no chat template, so there is no way to know how it expects a
                  conversation to be formatted. It cannot be loaded.
                </p>
              )}

              <label className="local-cap">
                Context cap
                <input
                  type="number"
                  min={512}
                  step={512}
                  value={contextCap}
                  placeholder="default"
                  onChange={(e) =>
                    setContextCap(e.target.value === "" ? "" : Number(e.target.value))
                  }
                />
              </label>

              <button
                onClick={() => void load()}
                disabled={
                  busy !== null || !inspected.supported || !inspected.has_chat_template
                }
              >
                {busy === "loading" ? "Loading…" : "Load model"}
              </button>
            </div>
          )}
        </>
      )}

      {error && <p className="failure">{error}</p>}
    </section>
  );
}
