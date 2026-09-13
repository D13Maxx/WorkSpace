import { useEffect, useState } from "react";
import { api, type ModelOption } from "../api";

const ANTHROPIC = "anthropic";

interface Props {
  onClose: () => void;
  models: ModelOption[];
}

export function Settings({ onClose, models }: Props) {
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    api.hasApiKey(ANTHROPIC).then(setHasKey).catch(() => setHasKey(null));
  }, []);

  const save = async () => {
    if (!keyInput.trim()) return;
    await api.setApiKey(ANTHROPIC, keyInput.trim());
    // The key is never read back; the UI only ever learns that one is set
    // (master spec s12).
    setKeyInput("");
    setHasKey(await api.hasApiKey(ANTHROPIC));
    setMessage("Key saved to Windows Credential Manager.");
  };

  const remove = async () => {
    await api.deleteApiKey(ANTHROPIC);
    setHasKey(false);
    setMessage("Key deleted.");
  };

  return (
    <aside className="settings-panel">
      <header>
        <h2>Settings</h2>
        <button onClick={onClose} aria-label="Close settings">
          ✕
        </button>
      </header>

      <section>
        <h3>Anthropic API key</h3>
        <p className="muted">
          Stored in Windows Credential Manager, never in the database or in logs. It cannot be
          read back here once saved.
        </p>
        <p>Status: {hasKey === null ? "unknown" : hasKey ? "a key is set" : "no key set"}</p>
        <input
          type="password"
          value={keyInput}
          placeholder={hasKey ? "Replace key…" : "Paste key…"}
          onChange={(e) => setKeyInput(e.target.value)}
          aria-label="API key"
        />
        <div className="row">
          <button className="primary" onClick={() => void save()} disabled={!keyInput.trim()}>
            {hasKey ? "Replace" : "Save"}
          </button>
          {hasKey && <button onClick={() => void remove()}>Delete</button>}
        </div>
        {message && <p className="muted">{message}</p>}
      </section>

      <section>
        <h3>Models</h3>
        {models.length === 0 ? (
          <p className="muted">None yet. Set a key, then reopen this panel.</p>
        ) : (
          <ul className="model-list">
            {models.map((m) => (
              <li key={`${m.backend}/${m.model_id}`}>
                {m.display_name}
                {m.unprofiled && (
                  <span className="warn" title="Using conservative defaults until you set real values">
                    unprofiled
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </aside>
  );
}
