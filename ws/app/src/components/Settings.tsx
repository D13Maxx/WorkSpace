import { useCallback, useEffect, useState } from "react";
import {
  api,
  ask,
  type ApprovalRules,
  type BackendInfo,
  type CompatProvider,
  type ModelChoice,
  type ModelOption,
  type McpStatus,
  type SkillList,
  type ModelProfile,
} from "../api";
import { LocalModelSection } from "./LocalModel";
import { setVimEnabled, vimEnabled } from "../vim";

interface Props {
  onClose: () => void;
  models: ModelOption[];
  onModelsChanged: () => void;
}

/** Keys: set, replace or delete, never read back. */
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
 * Editing a model's capabilities. Until a model is edited
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
        <ModelGroups models={models} suffix={(m) => (m.unprofiled ? " — unprofiled" : "")} />
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

function EditorSection() {
  const [vim, setVim] = useState(vimEnabled);
  return (
    <section>
      <h3>Editor</h3>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={vim}
          onChange={(e) => {
            setVim(e.target.checked);
            setVimEnabled(e.target.checked);
          }}
        />
        Vim keys
      </label>
      <p className="muted">
        Normal, insert and visual modes, motions, operators, text objects, registers, macros and
        marks; <code>:w</code> saves. Applies to open files straight away.
      </p>
    </section>
  );
}

/** Routine calls that run without asking: fixed rules, not the model's say-so. */
function ApprovalRulesSection() {
  const [rules, setRules] = useState<ApprovalRules | null>(null);
  const [commands, setCommands] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .approvalRules()
      .then((r) => {
        setRules(r);
        setCommands(r.commands.join("\n"));
      })
      .catch((e) => setError(String(e)));
  }, []);

  const save = (next: ApprovalRules) => {
    setRules(next);
    api.setApprovalRules(next).then(() => setError(null), (e) => setError(String(e)));
  };
  if (!rules) return error ? <p className="failure">{error}</p> : null;

  return (
    <section>
      <h3>Approval rules</h3>
      <p className="muted">
        What agents may do without asking you, in any chat. Everything else still asks.
      </p>
      <label className="row">
        <input type="checkbox" checked={rules.edits} onChange={(e) => save({ ...rules, edits: e.target.checked })} />
        Edit files inside the project (each edit is recorded and can be undone)
      </label>
      <p className="muted">
        Commands that run without asking, one per line, as written or followed by arguments:{" "}
        <code>cargo test</code> also covers <code>cargo test -p core</code>. A command with
        quotes, pipes, <code>;</code>, <code>&amp;</code>, <code>&gt;</code>, <code>$</code> or
        similar always asks, so nothing can ride along.
      </p>
      <textarea
        value={commands}
        onChange={(e) => setCommands(e.target.value)}
        onBlur={() =>
          save({
            ...rules,
            commands: commands
              .split("\n")
              .map((c) => c.trim())
              .filter(Boolean),
          })
        }
        rows={4}
        placeholder={"cargo test\nnpm test"}
        aria-label="Commands that run without asking"
      />
      {error && <p className="failure">{error}</p>}
    </section>
  );
}

/** What workers start on, so a big model can lead and cheaper ones do the parts. */
function WorkerModelSection({ models }: { models: ModelOption[] }) {
  const [model, setModel] = useState<ModelChoice | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = (m: ModelChoice) => `${m.backend}/${m.model_id}`;

  useEffect(() => {
    api
      .workerModel()
      .then(setModel)
      .catch((e) => setError(String(e)));
  }, []);

  const save = (value: string) => {
    const m = models.find((o) => key(o) === value);
    const next = m ? { backend: m.backend, model_id: m.model_id } : null;
    setModel(next);
    api.setWorkerModel(next).then(() => setError(null), (e) => setError(String(e)));
  };
  const known = model === null || models.some((o) => key(o) === key(model));

  return (
    <section>
      <h3>Worker model</h3>
      <p className="muted">
        The model workers start on when the chat hands them parts of a task. The model you chat
        with stays the lead. An agent file can name its own with <code>model: backend/model</code>.
      </p>
      <select value={model ? key(model) : ""} onChange={(e) => save(e.target.value)} aria-label="Worker model">
        <option value="">Same as the chat</option>
        {!known && model && <option value={key(model)}>{key(model)} (not available now)</option>}
        <ModelGroups models={models} />
      </select>
      {error && <p className="failure">{error}</p>}
    </section>
  );
}

/**
 * Models under their provider, by name, as the chat's picker shows them: with
 * hundreds from OpenRouter, typing a name in the open list jumps to it.
 */
function ModelGroups({ models, suffix }: { models: ModelOption[]; suffix?: (m: ModelOption) => string }) {
  const groups = new Map<string, ModelOption[]>();
  for (const m of models) groups.set(m.backend, [...(groups.get(m.backend) ?? []), m]);
  return (
    <>
      {[...groups].map(([backend, list]) => (
        <optgroup key={backend} label={backend}>
          {list.map((m) => (
            <option key={`${m.backend}/${m.model_id}`} value={`${m.backend}/${m.model_id}`}>
              {m.display_name}
              {suffix?.(m)}
            </option>
          ))}
        </optgroup>
      ))}
    </>
  );
}

/** Where a worker goes when its model fails: an ordered list, workers only. */
function FallbackSection({ models }: { models: ModelOption[] }) {
  const [list, setList] = useState<ModelChoice[]>([]);
  const [error, setError] = useState<string | null>(null);
  const key = (m: ModelChoice) => `${m.backend}/${m.model_id}`;

  useEffect(() => {
    api
      .workerFallbacks()
      .then(setList)
      .catch((e) => setError(String(e)));
  }, []);

  const save = (next: ModelChoice[]) => {
    setList(next);
    api.setWorkerFallbacks(next).then(() => setError(null), (e) => setError(String(e)));
  };
  const move = (i: number, by: number) => {
    const next = [...list];
    [next[i], next[i + by]] = [next[i + by], next[i]];
    save(next);
  };
  const name = (m: ModelChoice) => {
    const known = models.find((o) => key(o) === key(m));
    return known ? `${m.backend} / ${known.display_name}` : `${key(m)} (not available now)`;
  };
  const addable = models.filter((m) => !list.some((l) => key(l) === key(m)));

  return (
    <section>
      <h3>Worker fallbacks</h3>
      <p className="muted">
        When a worker's model fails (overloaded, down, out of credit) after the provider's own
        retries, the worker carries on with the first model here that works, then the next. The
        model you chat with never switches. Models that cannot use tools are skipped.
      </p>
      {list.length > 0 && (
        <ol className="fallback-list">
          {list.map((m, i) => (
            <li key={key(m)}>
              <span>{name(m)}</span>
              <button onClick={() => move(i, -1)} disabled={i === 0} aria-label={`Move ${key(m)} up`}>
                ↑
              </button>
              <button onClick={() => move(i, 1)} disabled={i === list.length - 1} aria-label={`Move ${key(m)} down`}>
                ↓
              </button>
              <button onClick={() => save(list.filter((_, j) => j !== i))} aria-label={`Remove ${key(m)}`}>
                ✕
              </button>
            </li>
          ))}
        </ol>
      )}
      <select
        value=""
        onChange={(e) => {
          const m = addable.find((o) => key(o) === e.target.value);
          if (m) save([...list, { backend: m.backend, model_id: m.model_id }]);
        }}
        aria-label="Add a fallback model"
      >
        <option value="">{list.length ? "Add another fallback…" : "Add a fallback model…"}</option>
        <ModelGroups models={addable} />
      </select>
      {error && <p className="failure">{error}</p>}
    </section>
  );
}

/** User-configured OpenAI-compatible endpoints. */
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
        format as Claude Code. Changes apply on the next launch. Every tool asks before running unless
        you list it in that server's <code>autoApprove</code>; tools that run commands always ask.
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

      <EditorSection />
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
      <ApprovalRulesSection />
      <WorkerModelSection models={models} />
      <FallbackSection models={models} />
      <CompatSection onChanged={refreshBackends} />
    </aside>
    </div>
  );
}
