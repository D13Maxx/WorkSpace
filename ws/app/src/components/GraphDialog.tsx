// The knowledge-graph pop-up for one folder: build it free, build it with AI,
// or open the graph it already has.

import { useEffect, useState } from "react";
import { api } from "../api";
import { baseName } from "./Explorer";

interface Props {
  folder: string;
  onClose: () => void;
  /** Start Understand-Anything's /understand on `folder` in a new chat. */
  onStartAi: (folder: string) => void;
  onOpenSettings: () => void;
  /** Open `folder` on the left, as both graph choices do. */
  onAddFolder: (folder: string) => void;
}

export function GraphDialog({ folder, onClose, onStartAi, onOpenSettings, onAddFolder }: Props) {
  const [exists, setExists] = useState(false);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [state, setState] = useState<"idle" | "building" | "opening" | "replace">("idle");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.hasGraph(folder).then(setExists).catch(() => setExists(false));
    api.hasApiKey("anthropic").then(setHasKey).catch(() => setHasKey(false));
  }, [folder]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const open = async () => {
    setError(null);
    setState("opening");
    try {
      await api.openGraph(folder);
      onClose();
    } catch (e) {
      setError(String(e));
      setState("idle");
    }
  };

  const buildFree = async (replace: boolean) => {
    setError(null);
    setState("building");
    try {
      await api.buildGraph(folder, replace);
      onAddFolder(folder);
      await open();
    } catch (e) {
      const message = String(e);
      if (message.startsWith("EXISTS")) {
        setState("replace");
      } else {
        setError(message);
        setState("idle");
      }
    }
  };

  const busy = state === "building" || state === "opening";
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <section className="graph-dialog" role="dialog" aria-modal="true" aria-label="Knowledge graph">
        <header>
          <h2>Knowledge graph: {baseName(folder)}</h2>
          <button onClick={onClose} aria-label="Close">
            ✕
          </button>
        </header>
        <p className="muted graph-folder" title={folder}>
          {folder}
        </p>

        {exists && (
          <div className="graph-existing">
            <span>This folder already has a graph.</span>
            <button onClick={() => void open()} disabled={busy}>
              {state === "opening" ? "Opening…" : "Open it"}
            </button>
          </div>
        )}

        {state === "replace" ? (
          <div className="failure graph-replace">
            <p>
              The graph here was not built by Workspace, perhaps by the AI pipeline. Replacing it with
              a free one cannot be undone.
            </p>
            <div className="row">
              <button onClick={() => void buildFree(true)}>Replace it</button>
              <button onClick={() => setState("idle")}>Keep it</button>
            </div>
          </div>
        ) : (
          <div className="graph-choices">
            <div className="graph-choice">
              <h3>Free graph</h3>
              <p className="muted">
                Workspace reads the code itself, in seconds. No AI, no cost, no API key. Files, what
                they define, what imports what, one layer per folder. Summaries come from each
                file&apos;s comments.
              </p>
              <button className="primary" onClick={() => void buildFree(false)} disabled={busy}>
                {state === "building" ? "Building…" : "Build free graph"}
              </button>
            </div>
            <div className="graph-choice">
              <h3>AI graph</h3>
              <p className="muted">
                Understand-Anything&apos;s own pipeline, run by Claude in a new chat: real
                summaries, architecture layers and a guided tour. It makes many API calls, so it
                costs money and takes a while. Needs Node.js, Python and Git Bash.
              </p>
              {hasKey === false ? (
                <p className="failure">
                  Needs an Anthropic API key.{" "}
                  <button
                    className="link-button"
                    onClick={() => {
                      onClose();
                      onOpenSettings();
                    }}
                  >
                    Add one in Settings
                  </button>
                </p>
              ) : (
                <button onClick={() => onStartAi(folder)} disabled={busy || hasKey === null}>
                  Build with AI
                </button>
              )}
            </div>
          </div>
        )}
        {error && <p className="failure">{error}</p>}
      </section>
    </div>
  );
}
