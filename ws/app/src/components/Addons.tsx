// Settings pages for adding to what the agent knows and can do: skills
// (instructions it follows) and tool servers (MCP: tools it can call).

import { useEffect, useState } from "react";
import { api, type McpServer, type McpStatus, type SkillList } from "../api";

/** A button that asks once more before it acts. */
function ConfirmButton({ label, confirm, onConfirm, disabled }: { label: string; confirm: string; onConfirm: () => void; disabled?: boolean }) {
  const [asking, setAsking] = useState(false);
  if (!asking)
    return (
      <button disabled={disabled} onClick={() => setAsking(true)}>
        {label}
      </button>
    );
  return (
    <span className="confirm-pair">
      <button
        className="danger"
        onClick={() => {
          setAsking(false);
          onConfirm();
        }}
      >
        {confirm}
      </button>
      <button onClick={() => setAsking(false)}>Cancel</button>
    </span>
  );
}

export function SkillsSection() {
  const [list, setList] = useState<SkillList | null>(null);
  const [mode, setMode] = useState<null | "new" | "repo">(null);
  const [draft, setDraft] = useState({ name: "", description: "", instructions: "" });
  const [repo, setRepo] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    api
      .listSkills()
      .then(setList)
      .catch((e) => setMessage({ ok: false, text: String(e) }));
  }, []);

  /** Run a change; the list it returns replaces the shown one. */
  const run = async (doing: string, step: () => Promise<SkillList>, done: (l: SkillList) => string) => {
    setBusy(doing);
    setMessage(null);
    try {
      const next = await step();
      setMessage({ ok: true, text: done(next) });
      setList(next);
      return true;
    } catch (e) {
      setMessage({ ok: false, text: String(e) });
      return false;
    } finally {
      setBusy(null);
    }
  };

  if (!list) return message ? <p className="failure">{message.text}</p> : null;
  const own = list.skills.filter((s) => s.own);
  const builtIn = list.skills.filter((s) => !s.own);
  const row = (s: SkillList["skills"][number]) => (
    <div key={s.name} className="addon-row">
      <div>
        <code>/{s.name}</code>
        <p className="muted">{s.description}</p>
      </div>
      {s.own && (
        <ConfirmButton
          label="Remove"
          confirm="Move to Recycle Bin"
          disabled={!!busy}
          onConfirm={() => void run("Removing…", () => api.removeSkill(s.name), () => `Removed ${s.name}.`)}
        />
      )}
    </div>
  );

  return (
    <section>
      <h3>Skills</h3>
      <p className="muted">
        A skill is written instructions for a kind of task. Type <code>/name</code> in a message to
        run one with any model; models that use tools also load one themselves when it fits. Skills
        are kept in <code>{list.dir}</code>.
      </p>

      <div className="addon-actions">
        <button className={mode === "new" ? "primary" : ""} onClick={() => setMode(mode === "new" ? null : "new")}>
          New skill
        </button>
        <button className={mode === "repo" ? "primary" : ""} onClick={() => setMode(mode === "repo" ? null : "repo")}>
          Add from GitHub
        </button>
        <button disabled={!!busy} onClick={() => void run("Reloading…", api.reloadSkills, (l) => `Found ${l.skills.length} skills.`)} title="Look in the skills folder again, for skills added by hand">
          Reload
        </button>
      </div>

      {mode === "new" && (
        <form
          className="addon-form"
          onSubmit={(e) => {
            e.preventDefault();
            void run("Saving…", () => api.createSkill(draft.name, draft.description, draft.instructions), () => `Added /${draft.name.trim()}.`).then((ok) => {
              if (ok) {
                setDraft({ name: "", description: "", instructions: "" });
                setMode(null);
              }
            });
          }}
        >
          <label>
            Name <span className="muted">what you type after /, like code-review</span>
            <input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="code-review" spellCheck={false} />
          </label>
          <label>
            What it is for <span className="muted">one line; the model reads it to decide when to use the skill</span>
            <input value={draft.description} onChange={(e) => setDraft({ ...draft, description: e.target.value })} placeholder="Review a change: bugs first, then clarity" />
          </label>
          <label>
            Instructions <span className="muted">Markdown, followed when the skill runs</span>
            <textarea rows={10} value={draft.instructions} onChange={(e) => setDraft({ ...draft, instructions: e.target.value })} placeholder={"1. Read the whole diff.\n2. List real bugs first, with the line.\n3. Then anything unclear."} />
          </label>
          <div className="row">
            <button className="primary" type="submit" disabled={!!busy}>
              Save skill
            </button>
            <button type="button" onClick={() => setMode(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}

      {mode === "repo" && (
        <form
          className="addon-form"
          onSubmit={(e) => {
            e.preventDefault();
            const before = list.skills.length;
            void run("Cloning… (a big repo takes a while)", () => api.addSkillRepo(repo), (l) => `Cloned. ${l.skills.length - before} new skills.`).then((ok) => {
              if (ok) {
                setRepo("");
                setMode(null);
              }
            });
          }}
        >
          <label>
            Repo <span className="muted">owner/repo or its address; every SKILL.md in it is found</span>
            <input value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="DietrichGebert/ponytail" spellCheck={false} />
          </label>
          <div className="row">
            <button className="primary" type="submit" disabled={!!busy || !repo.trim()}>
              Clone
            </button>
            <button type="button" onClick={() => setMode(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}

      {busy && <p className="muted">{busy}</p>}
      {message && <p className={message.ok ? "ok" : "failure"}>{message.text}</p>}

      <h4>Yours</h4>
      {own.length === 0 ? <p className="muted">None yet.</p> : own.map(row)}
      <h4>Built in</h4>
      {builtIn.map(row)}
    </section>
  );
}

/** The form's fields; turned into a server, keeping fields it does not show. */
interface Draft {
  id: string;
  command: string;
  args: string;
  env: string;
  autoApprove: string;
}

const lines = (text: string) =>
  text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);

function toDraft(id: string, s: McpServer): Draft {
  return {
    id,
    command: s.command ?? "",
    args: (s.args ?? []).join("\n"),
    env: Object.entries(s.env ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join("\n"),
    autoApprove: (s.autoApprove ?? []).join("\n"),
  };
}

function toServer(d: Draft, base: McpServer | undefined): McpServer {
  const server: McpServer = { ...base, command: d.command.trim(), args: lines(d.args) };
  const env = Object.fromEntries(
    lines(d.env)
      .filter((l) => l.includes("="))
      .map((l) => [l.slice(0, l.indexOf("=")).trim(), l.slice(l.indexOf("=") + 1)]),
  );
  if (Object.keys(env).length) server.env = env;
  else delete server.env;
  const approve = lines(d.autoApprove.replace(/,/g, "\n"));
  if (approve.length) server.autoApprove = approve;
  else delete server.autoApprove;
  return server;
}

export function McpSection() {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [servers, setServers] = useState<Record<string, McpServer>>({});
  // The form: null when closed; `was` is the name being edited, if any.
  const [form, setForm] = useState<{ draft: Draft; was: string | null } | null>(null);
  const [pasting, setPasting] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);

  const load = async () => {
    const [st, sv] = await Promise.all([api.mcpStatus(), api.mcpServers()]);
    setStatus(st);
    setServers(sv);
  };
  useEffect(() => {
    load().catch((e) => setMessage({ ok: false, text: String(e) }));
  }, []);

  /** Change mcp.json, then restart the servers so the change is live. */
  const change = async (step: () => Promise<unknown>, done: string) => {
    setBusy("Saving and restarting the tool servers…");
    setMessage(null);
    try {
      await step();
      await api.restartMcp();
      await load();
      setMessage({ ok: true, text: done });
      return true;
    } catch (e) {
      setMessage({ ok: false, text: String(e) });
      await load().catch(() => {});
      return false;
    } finally {
      setBusy(null);
    }
  };

  if (!status) return message ? <p className="failure">{message.text}</p> : null;
  const byId = new Map(status.servers.map((s) => [s.id, s]));
  const ids = [...new Set([...Object.keys(servers), ...status.servers.map((s) => s.id)])].sort();
  const state = (id: string) => {
    const s = byId.get(id);
    if (!s) return <span className="muted">not started</span>;
    if (s.error) return <span className="failure">{s.error}</span>;
    return <span className="ok">{s.tools} tools</span>;
  };

  return (
    <section>
      <h3>Tool servers (MCP)</h3>
      <p className="muted">
        A tool server is a program that gives the agent tools of its own, such as Serena for code or
        a database. Each tool asks before it runs unless you list it as auto-approved; tools that run
        commands always ask. Kept in <code>{status.config_path}</code>, the same format as Claude
        Code&apos;s.
      </p>

      <div className="addon-actions">
        <button
          className={form && !form.was ? "primary" : ""}
          onClick={() => setForm(form && !form.was ? null : { draft: { id: "", command: "", args: "", env: "", autoApprove: "" }, was: null })}
        >
          Add server
        </button>
        <button className={pasting !== null ? "primary" : ""} onClick={() => setPasting(pasting === null ? "" : null)}>
          Paste config
        </button>
        <button disabled={!!busy} onClick={() => void change(async () => {}, "Tool servers restarted.")} title="Stop every tool server and start them again">
          Restart servers
        </button>
      </div>

      {form && (
        <form
          className="addon-form"
          onSubmit={(e) => {
            e.preventDefault();
            const d = form.draft;
            void change(
              () => api.saveMcpServer(d.id.trim(), toServer(d, form.was ? servers[form.was] : undefined), form.was),
              `Saved ${d.id.trim()}.`,
            ).then((ok) => ok && setForm(null));
          }}
        >
          <label>
            Name <span className="muted">its tools are called name__tool</span>
            <input value={form.draft.id} onChange={(e) => setForm({ ...form, draft: { ...form.draft, id: e.target.value } })} placeholder="serena" spellCheck={false} />
          </label>
          <label>
            Command <span className="muted">the program to run, like npx, uvx or a full path</span>
            <input value={form.draft.command} onChange={(e) => setForm({ ...form, draft: { ...form.draft, command: e.target.value } })} placeholder="uvx" spellCheck={false} />
          </label>
          <label>
            Arguments <span className="muted">one per line</span>
            <textarea rows={4} value={form.draft.args} onChange={(e) => setForm({ ...form, draft: { ...form.draft, args: e.target.value } })} placeholder={"--from\ngit+https://github.com/oraios/serena\nserena\nstart-mcp-server"} spellCheck={false} />
          </label>
          <label>
            Environment <span className="muted">NAME=value, one per line; kept in mcp.json as written</span>
            <textarea rows={3} value={form.draft.env} onChange={(e) => setForm({ ...form, draft: { ...form.draft, env: e.target.value } })} placeholder="API_TOKEN=…" spellCheck={false} />
          </label>
          <label>
            Auto-approved tools <span className="muted">run without asking; one per line</span>
            <textarea rows={2} value={form.draft.autoApprove} onChange={(e) => setForm({ ...form, draft: { ...form.draft, autoApprove: e.target.value } })} placeholder="find_symbol" spellCheck={false} />
          </label>
          <div className="row">
            <button className="primary" type="submit" disabled={!!busy || !form.draft.id.trim() || !form.draft.command.trim()}>
              {form.was ? "Save and restart" : "Add and start"}
            </button>
            <button type="button" onClick={() => setForm(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}

      {pasting !== null && (
        <form
          className="addon-form"
          onSubmit={(e) => {
            e.preventDefault();
            let added: string[] = [];
            void change(
              async () => {
                added = await api.addMcpServers(pasting);
              },
              "Added and started.",
            ).then((ok) => {
              if (ok) {
                setPasting(null);
                setMessage({ ok: true, text: `Added ${added.join(", ")}.` });
              }
            });
          }}
        >
          <label>
            Config <span className="muted">the mcpServers JSON from the tool&apos;s README</span>
            <textarea rows={8} value={pasting} onChange={(e) => setPasting(e.target.value)} placeholder={'{\n  "mcpServers": {\n    "name": { "command": "npx", "args": ["-y", "some-mcp-server"] }\n  }\n}'} spellCheck={false} />
          </label>
          <div className="row">
            <button className="primary" type="submit" disabled={!!busy || !pasting.trim()}>
              Add and start
            </button>
            <button type="button" onClick={() => setPasting(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}

      {busy && <p className="muted">{busy}</p>}
      {message && <p className={message.ok ? "ok" : "failure"}>{message.text}</p>}

      <h4>Servers</h4>
      {ids.length === 0 ? (
        <p className="muted">None yet.</p>
      ) : (
        ids.map((id) => (
          <div key={id} className="addon-row">
            <div>
              <strong>{id}</strong> {state(id)}
              {servers[id] ? (
                <p className="muted">
                  <code>{[servers[id].command, ...(servers[id].args ?? [])].join(" ")}</code>
                </p>
              ) : (
                <p className="muted">Turned on in Settings → GitHub.</p>
              )}
            </div>
            {servers[id] && (
              <span className="addon-row-actions">
                <button disabled={!!busy} onClick={() => setForm({ draft: toDraft(id, servers[id]), was: id })}>
                  Edit
                </button>
                <ConfirmButton label="Remove" confirm="Remove it" disabled={!!busy} onConfirm={() => void change(() => api.removeMcpServer(id), `Removed ${id}.`)} />
              </span>
            )}
          </div>
        ))
      )}
    </section>
  );
}
