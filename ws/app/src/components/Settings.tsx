import { useCallback, useEffect, useState } from "react";
import {
  api,
  ask,
  type BackendInfo,
  type CompatProvider,
  type ModelOption,
  type McpStatus,
  type SkillList,
  type ModelProfile,
} from "../api";
import { LocalModelSection } from "./LocalModel";

interface Props {
  onClose: () => void;
  models: ModelOption[];
  onModelsChanged: () => void;
}

/** Keys: set, replace or delete, never read back (master spec s12). */
function KeySection({
  backends,
  onChanged,
}: {
  backends: BackendInfo[];
  onChanged: () => void;
}) {
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [message, setMessage] = useState<string | null>(null);

  const save = async (id: string) => {
    const key = (drafts[id] ?? "").trim();
    if (!key) return;
    await api.setApiKey(id, key);
    setDrafts((d) => ({ ...d, [id]: "" }));
    setMessage(`Key for ${id} saved to Windows Credential Manager.`);
    onChanged();
  };

  const remove = async (id: string) => {
    await api.deleteApiKey(id);
    setMessage(`Key for ${id} deleted.`);
    onChanged();
  };

  return (
    <section>
      <h3>API keys</h3>
      <p className="muted">
        Stored in Windows Credential Manager, never in the database or in logs. A key cannot be
        read back here once saved.
      </p>
      {backends
        .filter((b) => b.needs_key)
        .map((b) => (
        <div key={b.id} className="key-row">
          <div className="key-head">
            <strong>{b.id}</strong>
            <span className={b.has_key ? "ok" : "muted"}>
              {b.has_key ? "key set" : "no key"}
            </span>
          </div>
          <div className="row">
            <input
              type="password"
              value={drafts[b.id] ?? ""}
              placeholder={b.has_key ? "Replace key…" : "Paste key…"}
              onChange={(e) => setDrafts((d) => ({ ...d, [b.id]: e.target.value }))}
              aria-label={`API key for ${b.id}`}
            />
            <button
              className="primary"
              onClick={() => void save(b.id)}
              disabled={!(drafts[b.id] ?? "").trim()}
            >
              {b.has_key ? "Replace" : "Save"}
            </button>
            {b.has_key && <button onClick={() => void remove(b.id)}>Delete</button>}
          </div>
        </div>
      ))}
      {message && <p className="muted">{message}</p>}
    </section>
  );
}

/**
 * Editing a model's capabilities (master spec s7.1). Until a model is edited
 * here it runs on conservative guesses, which is what `unprofiled` warns about.
 */
function ProfileSection({
  models,
  onSaved,
}: {
  models: ModelOption[];
  onSaved: () => void;
}) {
  const [selected, setSelected] = useState<string>("");
  const [profile, setProfile] = useState<ModelProfile | null>(null);
  const [saved, setSaved] = useState(false);

  const model = models.find((m) => `${m.backend}/${m.model_id}` === selected);

  useEffect(() => {
    if (!model) {
      setProfile(null);
      return;
    }
    setSaved(false);
    api
      .getModelProfile(model.backend, model.model_id)
      .then(setProfile)
      .catch(() => setProfile(null));
  }, [model?.backend, model?.model_id]);

  const save = async () => {
    if (!model || !profile) return;
    await api.setModelProfile(model.backend, model.model_id, profile);
    setSaved(true);
    onSaved();
  };

  const edit = (patch: Partial<ModelProfile>) =>
    setProfile((p) => (p ? { ...p, ...patch } : p));

  return (
    <section>
      <h3>Model profiles</h3>
      <p className="muted">
        What each model accepts. Nothing is hardcoded: a model stays marked <em>unprofiled</em>,
        on conservative defaults, until you set real values here.
      </p>

      <select
        value={selected}
        onChange={(e) => setSelected(e.target.value)}
        aria-label="Model to edit"
      >
        <option value="">Choose a model…</option>
        {models.map((m) => (
          <option key={`${m.backend}/${m.model_id}`} value={`${m.backend}/${m.model_id}`}>
            {m.backend} / {m.display_name}
            {m.unprofiled ? " — unprofiled" : ""}
          </option>
        ))}
      </select>

      {model && profile && (
        <div className="profile-form">
          {model.unprofiled && (
            <p className="warn-block">
              This model is using guessed values. A context window set too high will produce
              errors from the provider; too low will summarise sooner than necessary.
            </p>
          )}

          <label>
            Context window (tokens)
            <input
              type="number"
              min={1}
              value={profile.context_window}
              onChange={(e) => edit({ context_window: Number(e.target.value) })}
            />
          </label>

          <label>
            Max output (tokens)
            <input
              type="number"
              min={1}
              value={profile.max_output_tokens}
              onChange={(e) => edit({ max_output_tokens: Number(e.target.value) })}
            />
          </label>

          <label className="checkbox">
            <input
              type="checkbox"
              checked={profile.vision}
              onChange={(e) => edit({ vision: e.target.checked })}
            />
            Accepts images
          </label>

          <label>
            Thinking
            <select
              value={profile.thinking}
              onChange={(e) => edit({ thinking: e.target.value as ModelProfile["thinking"] })}
            >
              <option value="off">Off</option>
              <option value="adaptive">Adaptive (the provider decides)</option>
              <option value="enabled">Enabled with a budget</option>
            </select>
          </label>

          {profile.thinking === "enabled" && (
            <label>
              Thinking budget (tokens)
              <input
                type="number"
                min={1}
                value={profile.thinking_budget ?? 4096}
                onChange={(e) => edit({ thinking_budget: Number(e.target.value) })}
              />
            </label>
          )}

          <div className="row">
            <button className="primary" onClick={() => void save()}>
              Save profile
            </button>
            {saved && <span className="ok">Saved.</span>}
          </div>
        </div>
      )}
    </section>
  );
}

/** User-configured OpenAI-compatible endpoints (master spec s7.5). */
/** Skills are SKILL.md folders, the same format Claude Code uses. */
function SkillsSection() {
  const [list, setList] = useState<SkillList | null>(null);
  useEffect(() => {
    api
      .listSkills()
      .then(setList)
      .catch(() => setList(null));
  }, []);
  if (!list) return null;

  return (
    <section>
      <h3>Skills</h3>
      <p className="muted">
        Ponytail and the Understand-Anything graph skills are built in. Add your own skill folders
        to <code>{list.dir}</code> and restart; one with a built-in&apos;s name replaces it. Type{" "}
        <code>/name</code> in a message to run one; Claude can also load them itself.
      </p>
      {list.skills.length === 0 ? (
        <p className="muted">No skills found.</p>
      ) : (
        list.skills.map((s) => (
          <div key={s.name} className="skill-row">
            <code>/{s.name}</code> <span className="muted">{s.description}</span>
          </div>
        ))
      )}
    </section>
  );
}

/** Tool servers come from mcp.json, in the same shape as Claude Code's. */
function McpSection() {
  const [status, setStatus] = useState<McpStatus | null>(null);
  useEffect(() => {
    api
      .mcpStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, []);
  if (!status) return null;

  return (
    <section>
      <h3>Tool servers (MCP)</h3>
      <p className="muted">
        Configured in <code>{status.config_path}</code> using the same <code>mcpServers</code>{" "}
        format as Claude Code. Changes apply on the next launch. Tools that can edit files or run
        commands always ask before running.
      </p>
      {status.servers.length === 0 ? (
        <p className="muted">No tool servers configured.</p>
      ) : (
        status.servers.map((s) => (
          <div key={s.id} className="key-head">
            <strong>{s.id}</strong>
            {s.error ? (
              <span className="failure">{s.error}</span>
            ) : (
              <span className="ok">{s.tools} tools</span>
            )}
          </div>
        ))
      )}
    </section>
  );
}

function CompatSection({ onChanged }: { onChanged: () => void }) {
  const [providers, setProviders] = useState<CompatProvider[]>([]);
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .listCompatProviders()
      .then(setProviders)
      .catch(() => setProviders([]));
  }, []);

  useEffect(refresh, [refresh]);

  const add = async () => {
    setError(null);
    try {
      await api.saveCompatProvider(id.trim(), name.trim() || id.trim(), baseUrl.trim());
      setId("");
      setName("");
      setBaseUrl("");
      refresh();
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section>
      <h3>OpenAI-compatible endpoints</h3>
      <p className="muted">
        Any server speaking Chat Completions: a local llama.cpp, vLLM, or a gateway. Reasoning
        from these is shown but never replayed — the protocol has nothing signed to send back.
        New endpoints are wired up on the next launch.
      </p>

      {providers.length > 0 && (
        <ul className="model-list">
          {providers.map((p) => (
            <li key={p.id}>
              <span style={{ flex: 1 }}>
                <strong>{p.display_name}</strong>
                <br />
                <span className="muted">{p.base_url}</span>
              </span>
              <button
                onClick={async () => {
                  if (await ask(`Remove "${p.display_name}"?`)) {
                    await api.deleteCompatProvider(p.id);
                    refresh();
                    onChanged();
                  }
                }}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="profile-form">
        <label>
          Id (also the name its key is stored under)
          <input
            value={id}
            onChange={(e) => setId(e.target.value)}
            placeholder="local-llama"
          />
        </label>
        <label>
          Display name
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Local llama.cpp"
          />
        </label>
        <label>
          Base URL
          <input
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="http://localhost:8080/v1"
          />
        </label>
        <div className="row">
          <button
            className="primary"
            onClick={() => void add()}
            disabled={!id.trim() || !baseUrl.trim()}
          >
            Add endpoint
          </button>
        </div>
        {error && <p className="failure">{error}</p>}
      </div>
    </section>
  );
}

export function Settings({ onClose, models, onModelsChanged }: Props) {
  const [backends, setBackends] = useState<BackendInfo[]>([]);

  const refreshBackends = useCallback(() => {
    api
      .listBackends()
      .then(setBackends)
      .catch(() => setBackends([]));
  }, []);

  useEffect(refreshBackends, [refreshBackends]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    // A click on the dimmed backdrop closes it; one inside does not.
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
    <aside className="settings-panel" role="dialog" aria-modal="true" aria-label="Settings">
      <header>
        <h2>Settings</h2>
        <button onClick={onClose} aria-label="Close settings">
          ✕
        </button>
      </header>

      <KeySection
        backends={backends}
        onChanged={() => {
          refreshBackends();
          onModelsChanged();
        }}
      />
      <SkillsSection />
      <McpSection />
      <LocalModelSection onChanged={onModelsChanged} />
      <ProfileSection models={models} onSaved={onModelsChanged} />
      <CompatSection onChanged={refreshBackends} />
    </aside>
    </div>
  );
}
