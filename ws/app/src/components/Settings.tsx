import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  api,
  ask,
  type ApprovalRules,
  type ChatEncryption,
  type BackendInfo,
  type CompatProvider,
  type ModelChoice,
  type ModelOption,
  type McpStatus,
  type RoleModel,
  type SkillList,
  type ModelProfile,
} from "../api";
import { LocalModelSection } from "./LocalModel";
import { Languages } from "./Languages";
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

/** Whether chats are encrypted on this computer; a change applies at the next start. */
function EncryptionSection() {
  const [state, setState] = useState<ChatEncryption | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .chatEncryption()
      .then(setState)
      .catch((e) => setError(String(e)));
  }, []);

  const save = (encrypt: boolean) => {
    if (!state) return;
    setState({ ...state, wanted: encrypt });
    api.setChatEncryption(encrypt).then(() => setError(null), (e) => setError(String(e)));
  };
  if (!state) return error ? <p className="failure">{error}</p> : null;

  return (
    <section>
      <h3>Chat encryption</h3>
      <p className="muted">
        Chats, images and attachments are encrypted on disk, with a key kept in Windows
        Credential Manager beside your API keys. Anyone signed in to this Windows account can
        still open Workspace and read them. Chats are {state.on ? "encrypted" : "not encrypted"}{" "}
        now.
      </p>
      <label className="row">
        <input type="checkbox" checked={state.wanted} onChange={(e) => save(e.target.checked)} />
        Encrypt chats on this computer
      </label>
      {state.wanted !== state.on && (
        <p className="muted">Restart Workspace to {state.wanted ? "encrypt" : "decrypt"} your chats.</p>
      )}
      {error && <p className="failure">{error}</p>}
    </section>
  );
}

/** What workers start on, so a big model can lead and cheaper ones do the parts. */
/** The team: the lead is the chat's model; every worker starts on the worker
 *  model (or its role's) and moves down its fallbacks (its role's, or the
 *  shared list) when that model fails. */
function TeamSection({ models }: { models: ModelOption[] }) {
  const [worker, setWorker] = useState<ModelChoice | null>(null);
  const [shared, setShared] = useState<ModelChoice[]>([]);
  const [roles, setRoles] = useState<RoleModel[]>([]);
  const [error, setError] = useState<string | null>(null);
  const done = () => setError(null);
  const failed = (e: unknown) => setError(String(e));

  useEffect(() => {
    Promise.all([api.workerModel(), api.workerFallbacks(), api.roleModels()])
      .then(([w, f, r]) => {
        setWorker(w);
        setShared(f);
        setRoles(r);
      })
      .catch(failed);
  }, []);

  const setRole = (role: string, change: Partial<RoleModel>) =>
    setRoles((rs) => rs.map((r) => (r.role === role ? { ...r, ...change } : r)));

  return (
    <section>
      <h3>Team mode</h3>
      <p className="muted">
        With <strong>Coordinate</strong> ticked, the chat leads a team: it plans, hands parts to
        workers in six roles, and checks their work. Each worker starts on its model; if that model
        fails after the provider&apos;s own retries (overloaded, down, out of credit), it carries on
        with its fallbacks in order. Models that cannot use tools are skipped. An agent file&apos;s
        own <code>model:</code> still comes first.
      </p>

      <div className="team-card team-lead">
        <div className="team-head">
          <strong>Lead</strong>
          <span className="muted">the chat&apos;s model</span>
        </div>
        <p className="muted">Always the model picked in the chat. It never switches to a fallback.</p>
      </div>

      <div className="team-card">
        <div className="team-head">
          <strong>Every worker</strong>
          <span className="muted">unless its role has its own</span>
        </div>
        <ModelPick
          models={models}
          value={worker}
          none="Same as the chat"
          label="Worker model"
          onChange={(m) => {
            setWorker(m);
            api.setWorkerModel(m).then(done, failed);
          }}
        />
        <FallbackList
          models={models}
          list={shared}
          label="Shared fallbacks"
          empty="None: a worker whose model fails stops."
          onChange={(next) => {
            setShared(next);
            api.setWorkerFallbacks(next).then(done, failed);
          }}
        />
      </div>

      <h4>Roles</h4>
      {roles.map((r) => (
        <div key={r.role} className="team-card">
          <div className="team-head">
            <strong className="team-role">{r.role}</strong>
            <span className="muted">{r.description}</span>
          </div>
          <ModelPick
            models={models}
            value={r.model}
            none="The worker model"
            label={`Model for the ${r.role}`}
            onChange={(m) => {
              setRole(r.role, { model: m });
              api.setRoleModel(r.role, m).then(done, failed);
            }}
          />
          <FallbackList
            models={models}
            list={r.fallbacks}
            label={`Fallbacks for the ${r.role}`}
            empty="Uses the shared fallbacks."
            onChange={(next) => {
              setRole(r.role, { fallbacks: next });
              api.setRoleFallbacks(r.role, next).then(done, failed);
            }}
          />
        </div>
      ))}
      {error && <p className="failure">{error}</p>}
    </section>
  );
}

const modelKey = (m: ModelChoice) => `${m.backend}/${m.model_id}`;

/** A model picker; `none` names what an empty choice means. */
function ModelPick({
  models,
  value,
  none,
  label,
  onChange,
}: {
  models: ModelOption[];
  value: ModelChoice | null;
  none: string;
  label: string;
  onChange: (m: ModelChoice | null) => void;
}) {
  const known = value === null || models.some((o) => modelKey(o) === modelKey(value));
  return (
    <div className="team-field">
      <span>Model</span>
      <select
        value={value ? modelKey(value) : ""}
        aria-label={label}
        onChange={(e) => {
          const m = models.find((o) => modelKey(o) === e.target.value);
          onChange(m ? { backend: m.backend, model_id: m.model_id } : null);
        }}
      >
        <option value="">{none}</option>
        {!known && value && <option value={modelKey(value)}>{modelKey(value)} (not available now)</option>}
        <ModelGroups models={models} />
      </select>
    </div>
  );
}

/** An ordered list of fallback models: reorder, remove, add. */
function FallbackList({
  models,
  list,
  label,
  empty,
  onChange,
}: {
  models: ModelOption[];
  list: ModelChoice[];
  label: string;
  empty: string;
  onChange: (next: ModelChoice[]) => void;
}) {
  const move = (i: number, by: number) => {
    const next = [...list];
    [next[i], next[i + by]] = [next[i + by], next[i]];
    onChange(next);
  };
  const name = (m: ModelChoice) => {
    const known = models.find((o) => modelKey(o) === modelKey(m));
    return known ? `${m.backend} / ${known.display_name}` : `${modelKey(m)} (not available now)`;
  };
  const addable = models.filter((m) => !list.some((l) => modelKey(l) === modelKey(m)));
  return (
    <div className="team-field">
      <span>Fallbacks</span>
      <div>
        {list.length === 0 ? (
          <p className="muted team-empty">{empty}</p>
        ) : (
          <ol className="fallback-list" aria-label={label}>
            {list.map((m, i) => (
              <li key={modelKey(m)}>
                <span>{name(m)}</span>
                <button onClick={() => move(i, -1)} disabled={i === 0} aria-label={`Move ${modelKey(m)} up`}>
                  ↑
                </button>
                <button onClick={() => move(i, 1)} disabled={i === list.length - 1} aria-label={`Move ${modelKey(m)} down`}>
                  ↓
                </button>
                <button onClick={() => onChange(list.filter((_, j) => j !== i))} aria-label={`Remove ${modelKey(m)}`}>
                  ✕
                </button>
              </li>
            ))}
          </ol>
        )}
        <select
          value=""
          onChange={(e) => {
            const m = addable.find((o) => modelKey(o) === e.target.value);
            if (m) onChange([...list, { backend: m.backend, model_id: m.model_id }]);
          }}
          aria-label={`Add to ${label.toLowerCase()}`}
        >
          <option value="">{list.length ? "Add another fallback…" : "Add a fallback…"}</option>
          <ModelGroups models={addable} />
        </select>
      </div>
    </div>
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
function GithubSection() {
  const [login, setLogin] = useState<string | null | undefined>(undefined);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [tools, setTools] = useState<{ enabled: boolean; installed: boolean } | null>(null);

  useEffect(() => {
    api
      .githubAccount()
      .then((a) => setLogin(a?.login ?? null))
      .catch((e) => {
        setLogin(null);
        setMessage(String(e));
      });
    api
      .githubAgentTools()
      .then(setTools)
      .catch(() => setTools(null));
  }, []);

  const signIn = async (pasted: string | null) => {
    setBusy(true);
    setMessage(null);
    try {
      const a = await api.githubSignIn(pasted);
      setLogin(a.login);
      setToken("");
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section>
      <h3>GitHub</h3>
      <p className="muted">
        Push, Pull, branches and Clone use git's own sign-in and work without this. Signing in here
        shows each branch's pull request and CI in the Changes panel, and lets the agent use GitHub.
        The token is kept in the system keychain.
      </p>
      {login === undefined ? null : login ? (
        <div className="key-head">
          <span>
            Signed in as <strong>{login}</strong>
          </span>
          <button
            onClick={() =>
              void api
                .githubSignOut()
                .then(() => setLogin(null))
                .catch((e) => setMessage(String(e)))
            }
          >
            Sign out
          </button>
        </div>
      ) : (
        <>
          <div className="row">
            <button className="primary" disabled={busy} onClick={() => void signIn(null)} title="Uses the GitHub sign-in git keeps (Git Credential Manager); it may open its own sign-in window">
              {busy ? "Signing in…" : "Sign in with git's GitHub login"}
            </button>
          </div>
          <div className="row">
            <input
              type="password"
              value={token}
              placeholder="…or paste a GitHub token"
              onChange={(e) => setToken(e.target.value)}
              aria-label="GitHub token"
            />
            <button disabled={busy || !token.trim()} onClick={() => void signIn(token)}>
              Save
            </button>
          </div>
        </>
      )}
      {tools && (
        <label className="checkbox row" title="GitHub's own tool server: issues, pull requests, CI and code search for the agent">
          <input
            type="checkbox"
            checked={tools.enabled}
            disabled={!tools.installed && !tools.enabled}
            onChange={(e) => {
              const enabled = e.target.checked;
              void api
                .setGithubAgentTools(enabled)
                .then(() => {
                  setTools({ ...tools, enabled });
                  setMessage("The agent's GitHub tools change at the next start of Workspace.");
                })
                .catch((err) => setMessage(String(err)));
            }}
          />
          <span>
            Give the agent GitHub tools (issues, pull requests, CI)
            {!tools.installed && (
              <span className="muted">
                {" "}
                — needs <code>github-mcp-server</code> on PATH: download it from{" "}
                <button className="linklike" onClick={() => void api.openGithub("https://github.com/github/github-mcp-server/releases")}>
                  its releases
                </button>
              </span>
            )}
            {tools.installed && !login && <span className="muted"> — sign in above first</span>}
          </span>
        </label>
      )}
      {message && <p className="muted">{message}</p>}
    </section>
  );
}

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
        format as Claude Code. Changes apply on the next launch. Models reach these tools in Code
        Mode: they search them and call them by writing short scripts, so many servers cost no more
        context than one. Every tool asks before running unless you list it in that server's{" "}
        <code>autoApprove</code>; tools that run commands always ask, and a script that uses one asks
        once, showing the script.
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

const GROUPS = [
  { id: "general", label: "General", about: "How the editor behaves and which languages run." },
  { id: "models", label: "Models", about: "Where the models come from and what each can do." },
  { id: "agents", label: "Agents", about: "The team: its lead and workers, what they may do, their skills." },
  { id: "connections", label: "Connections", about: "GitHub and other tool servers." },
  { id: "privacy", label: "Privacy", about: "How your chats are kept." },
] as const;
type Group = (typeof GROUPS)[number]["id"];

export function Settings({ onClose, models, onModelsChanged }: Props) {
  const [backends, setBackends] = useState<BackendInfo[]>([]);
  // Null: the overview.
  const [page, setPage] = useState<string | null>(null);

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

  // Every topic is a page of its own, listed under its category.
  const topics: { id: string; group: Group; label: string; about: string; body: () => ReactNode }[] = [
    { id: "editor", group: "general", label: "Editor", about: "Vim keys", body: () => <EditorSection /> },
    {
      id: "languages",
      group: "general",
      label: "Languages",
      about: "Packs that run and debug each language",
      body: () => (
        <section>
          <h3>Languages</h3>
          <Languages />
        </section>
      ),
    },
    {
      id: "keys",
      group: "models",
      label: "API keys",
      about: "Keys for Anthropic, OpenAI, Gemini…",
      body: () => (
        <KeySection
          backends={backends}
          onChanged={() => {
            refreshBackends();
            onModelsChanged();
          }}
        />
      ),
    },
    { id: "compat", group: "models", label: "OpenAI-compatible endpoints", about: "llama.cpp, vLLM, gateways", body: () => <CompatSection onChanged={refreshBackends} /> },
    { id: "local", group: "models", label: "Local model", about: "Run a model on this PC", body: () => <LocalModelSection onChanged={onModelsChanged} /> },
    { id: "profiles", group: "models", label: "Model profiles", about: "Context size, output and vision per model", body: () => <ProfileSection models={models} onSaved={onModelsChanged} /> },
    { id: "team", group: "agents", label: "Team mode", about: "The lead, and each worker's model and fallbacks", body: () => <TeamSection models={models} /> },
    { id: "approvals", group: "agents", label: "Approval rules", about: "What agents may do without asking", body: () => <ApprovalRulesSection /> },
    { id: "skills", group: "agents", label: "Skills", about: "/commands the agent can run", body: () => <SkillsSection /> },
    { id: "github", group: "connections", label: "GitHub", about: "Sign in, PRs, CI and agent tools", body: () => <GithubSection /> },
    { id: "mcp", group: "connections", label: "Tool servers (MCP)", about: "Serena and other tool servers", body: () => <McpSection /> },
    { id: "encryption", group: "privacy", label: "Chat encryption", about: "Encrypt the chats on disk", body: () => <EncryptionSection /> },
  ];
  const open = topics.find((t) => t.id === page);
  const nav = (
    <nav className="settings-nav" aria-label="Settings pages">
      <button className={open ? "" : "selected"} onClick={() => setPage(null)} aria-current={open ? undefined : "page"}>
        Overview
      </button>
      {GROUPS.map((g) => (
        <div key={g.id} className="settings-nav-group">
          <h4>{g.label}</h4>
          {topics
            .filter((t) => t.group === g.id)
            .map((t) => (
              <button key={t.id} className={page === t.id ? "selected" : ""} aria-current={page === t.id ? "page" : undefined} onClick={() => setPage(t.id)}>
                {t.label}
              </button>
            ))}
        </div>
      ))}
    </nav>
  );

  return (
    <div className="settings-screen" role="dialog" aria-modal="true" aria-label="Settings">
      <header className="settings-top">
        <button className="linklike" onClick={() => setPage(null)} disabled={!open}>
          Settings
        </button>
        {open && (
          <span className="muted">
            {" "}
            › {GROUPS.find((g) => g.id === open.group)?.label} › {open.label}
          </span>
        )}
        <span className="activity-spacer" />
        <button onClick={onClose} aria-label="Close settings" title="Close (Esc)">
          ✕
        </button>
      </header>
      <div className="settings-body">
        {nav}
        <div className="settings-page">
          {open ? (
            <div className="settings-content">{open.body()}</div>
          ) : (
            <div className="settings-content">
              <h2 className="settings-title">Settings</h2>
              <div className="settings-cards">
                {GROUPS.map((g) => (
                  <div key={g.id} className="settings-card">
                    <h3>{g.label}</h3>
                    <p className="muted">{g.about}</p>
                    <ul>
                      {topics
                        .filter((t) => t.group === g.id)
                        .map((t) => (
                          <li key={t.id}>
                            <button className="settings-link" onClick={() => setPage(t.id)}>
                              <span>{t.label}</span>
                              <span className="muted">{t.about}</span>
                            </button>
                          </li>
                        ))}
                    </ul>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
