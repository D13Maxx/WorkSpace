// A read-only view of one file, with line numbers.
//
// Rendered as text, never HTML, like model output (master spec s12).
// shortcut: no syntax highlighting, add CodeMirror if plain text proves too hard to read.

import { useEffect, useState } from "react";
import { api } from "../api";

interface Props {
  project: string;
  path: string;
  onClose: () => void;
}

export function FileViewer({ project, path, onClose }: Props) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    setText(null);
    setError(null);
    api
      .readTextFile(project, path)
      .then(setText)
      .catch((e) => setError(String(e)));
  }, [project, path, nonce]);

  const lines = text?.split("\n") ?? [];
  // A trailing newline is not an extra, empty line.
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();

  return (
    <section className="file-viewer" aria-label={path}>
      <header>
        <code title={path}>{path}</code>
        <span className="actions">
          {/* The agent may have just edited it. */}
          <button onClick={() => setNonce((n) => n + 1)} title="Reload from disk">
            Reload
          </button>
          <button onClick={onClose} aria-label="Close file">
            ✕
          </button>
        </span>
      </header>
      {error && <p className="failure">{error}</p>}
      {text === null && !error && <p className="muted">Loading…</p>}
      {text !== null && (
        <div className="code">
          <pre className="gutter" aria-hidden="true">
            {lines.map((_, i) => `${i + 1}\n`).join("")}
          </pre>
          <pre className="source">{lines.join("\n")}</pre>
        </div>
      )}
    </section>
  );
}
