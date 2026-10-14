// A blueprint: software drawn as parts wired with connectors, run in a
// simulation to catch bugs, scaling and deployment problems before any code
// exists, then handed to the agent to build in one go.
//
// No imports, so `node src/blueprint/model.test.ts` can test it directly.

export interface ConnectorType {
  id: string;
  name: string;
  color: string;
  /** The caller waits for the answer (HTTP, SQL), or does not (a queued message). */
  sync: boolean;
  /** Network time for one call, ms. */
  latency: number;
  description: string;
  /** The inventory heading it sits under. */
  category?: string;
}

/** A named way in or out of a part, as VectorShift's node handles are: an API's
 *  "public" and "admin" endpoints, a worker's "jobs" in and "calls" out. */
export interface Port {
  /** Stable within its part type: wires keep pointing at it when others change. */
  id: string;
  name: string;
  dir: "in" | "out";
  /** The connector it works with, or "*" for any. */
  kind: string;
}

/** A setting the part is built with: a model name, a database engine, a size. */
export interface Setting {
  key: string;
  label: string;
  type: "text" | "number" | "choice";
  default: string;
  /** For "choice". */
  options?: string[];
}

/** One thing a part does that others can ask for: `createOrder(items, customerId) → orderId`. */
export interface Operation {
  name: string;
  /** What it takes, comma separated: "items, customerId". */
  takes: string;
  /** What it gives back. */
  returns: string;
}

/** What a part does, for the agent that builds it and for Run's checks. */
export interface Contract {
  operations: Operation[];
  /** What it keeps, one line each: "orders(id, items, total, status)". */
  data: string[];
  /** How it must behave, one line each: "reject an order if an item is out of stock". */
  rules: string[];
}

/** A request that exercises one part of the built system. */
export interface Probe {
  method: string;
  url: string;
  body?: string;
}

/** How to start what the agent built and reach each part, so the sandbox can
 *  run it and measure it. Build with agent writes it into the blueprint. */
export interface Sandbox {
  /** Starts every part, run in the blueprint's folder (or `dir` inside it). */
  start: string;
  /** Stops it, when ending `start` is not enough ("docker compose down"). */
  stop?: string;
  dir?: string;
  /** Answers once everything is up. */
  ready?: string;
  /** By part id. */
  probes: Record<string, Probe>;
}

export interface PartType {
  id: string;
  name: string;
  color: string;
  /** What this kind of part is, for the agent that builds it. */
  description: string;
  /** Requests per second one copy handles; 0 for no limit. */
  capacity: number;
  /** Time one request spends in the part, ms. */
  latency: number;
  /** Share of requests that fail on their own, 0..1. */
  errorRate: number;
  /** Copies running. */
  replicas: number;
  /** Holds data, so more copies need replication, not just more instances. */
  stateful: boolean;
  /** Requests per second it starts by itself (users, a schedule); 0 for none. */
  source: number;
  /** Connector ids it can be called over; "*" takes any, empty takes none.
   *  With `ports`, the union of its in-ports' connectors. */
  accepts: string[];
  /** The inventory heading it sits under. */
  category?: string;
  /** Named ways in and out; without them, one "in" (taking `accepts`) and one "out". */
  ports?: Port[];
  settings?: Setting[];
  /** What every part of this kind does, until a placed one has its own. */
  contract?: Contract;
}

export const TUNABLES = ["capacity", "latency", "errorRate", "replicas", "source"] as const;
export type Tunable = (typeof TUNABLES)[number];

export interface Part {
  id: string;
  type: string;
  label: string;
  x: number;
  y: number;
  /** What this one should do, in the user's words. */
  notes: string;
  /** This part's own numbers, over its type's. */
  set: Partial<Record<Tunable, number>>;
  /** Its settings, by key, over each setting's default. */
  values?: Record<string, string>;
  /** Where its code is (a file or folder, from the blueprint's folder), once built. */
  code?: string;
  /** What this one does, over its type's. */
  contract?: Contract;
}

export interface Wire {
  id: string;
  from: string;
  to: string;
  kind: string;
  /** Calls made for each request the caller handles. */
  calls: number;
  notes: string;
  /** The ports it joins; absent on wires drawn before parts had ports. */
  fromPort?: string;
  toPort?: string;
  /** The target's operation it calls. */
  op?: string;
  /** What it sends, comma separated, checked against what that operation takes. */
  sends?: string;
}

export interface Blueprint {
  version: 1;
  name: string;
  description: string;
  /** Languages, frameworks and hosting the user wants. */
  stack: string;
  parts: Part[];
  wires: Wire[];
  customParts: PartType[];
  customConnectors: ConnectorType[];
  /** How to run what the agent built, to measure it. */
  sandbox?: Sandbox;
}

export const CONNECTORS: ConnectorType[] = [
  { id: "http", name: "HTTP / REST", color: "#4f8cff", sync: true, latency: 5, description: "A request and its response over HTTP." },
  { id: "call", name: "Function call", color: "#9aa4b2", sync: true, latency: 0, description: "A direct call inside the same program." },
  { id: "sql", name: "SQL query", color: "#e0a030", sync: true, latency: 1, description: "Queries against a database." },
  { id: "kv", name: "Cache get/set", color: "#e05555", sync: true, latency: 1, description: "Key-value reads and writes." },
  { id: "llm", name: "LLM request", color: "#b07cff", sync: true, latency: 20, description: "A prompt sent to a language model, and its reply." },
  { id: "message", name: "Queue message", color: "#2fbf8f", sync: false, latency: 2, description: "A job put on a queue; the sender does not wait." },
  { id: "event", name: "Event (pub/sub)", color: "#23b5c9", sync: false, latency: 2, description: "An event published to whoever subscribes." },
];

const part = (p: Omit<PartType, "errorRate" | "replicas" | "stateful" | "source"> & Partial<PartType>): PartType => ({
  errorRate: 0,
  replicas: 1,
  stateful: false,
  source: 0,
  ...p,
});

export const PARTS: PartType[] = [
  part({ id: "users", name: "Users", color: "#5b6b82", description: "People using the system from a browser or app.", capacity: 0, latency: 0, source: 50, accepts: [] }),
  part({ id: "schedule", name: "Scheduled job", color: "#5b6b82", description: "Work that starts on a timer (cron).", capacity: 0, latency: 0, source: 0.02, accepts: [] }),
  part({ id: "web", name: "Web front end", color: "#4f8cff", description: "The website or web app the users see.", capacity: 2000, latency: 10, accepts: ["http"] }),
  part({ id: "lb", name: "Load balancer", color: "#4f8cff", description: "Spreads requests over the copies behind it.", capacity: 20000, latency: 1, accepts: ["http"] }),
  part({ id: "api", name: "API service", color: "#3d7be0", description: "A backend service answering requests.", capacity: 200, latency: 25, accepts: ["http", "call"] }),
  part({ id: "worker", name: "Background worker", color: "#2fbf8f", description: "Processes jobs from a queue or events.", capacity: 20, latency: 300, accepts: ["message", "event", "call"] }),
  part({ id: "queue", name: "Queue", color: "#2fbf8f", description: "Holds jobs until a worker takes them.", capacity: 5000, latency: 2, stateful: true, accepts: ["message", "event"] }),
  part({ id: "db", name: "Database", color: "#e0a030", description: "Stores the system's data (SQL).", capacity: 800, latency: 4, stateful: true, accepts: ["sql"] }),
  part({ id: "cache", name: "Cache", color: "#e05555", description: "Keeps hot data in memory (Redis-like).", capacity: 20000, latency: 1, stateful: true, accepts: ["kv"] }),
  part({ id: "storage", name: "File storage", color: "#c98a2e", description: "Stores files and uploads (S3-like).", capacity: 1000, latency: 30, stateful: true, accepts: ["http"] }),
  part({ id: "llm", name: "LLM", color: "#b07cff", description: "A hosted language model.", capacity: 5, latency: 3000, errorRate: 0.01, accepts: ["llm"] }),
  part({ id: "agent", name: "AI agent", color: "#9a5cf0", description: "An LLM loop that plans and uses tools.", capacity: 2, latency: 500, accepts: ["http", "message", "call"] }),
  part({ id: "vector", name: "Vector database", color: "#c8902a", description: "Stores embeddings for search and retrieval.", capacity: 300, latency: 15, stateful: true, accepts: ["sql", "http"] }),
  part({ id: "external", name: "Third-party API", color: "#7a8597", description: "A service someone else runs (payments, email…).", capacity: 50, latency: 200, errorRate: 0.01, accepts: ["http"] }),
];

export function emptyBlueprint(name: string): Blueprint {
  return { version: 1, name, description: "", stack: "", parts: [], wires: [], customParts: [], customConnectors: [] };
}

/** A blueprint read from a file; an empty file is a new blueprint. */
/** The file keeps positions apart from what the parts are, as LTspice keeps a
 *  schematic apart from its netlist: moving a box changes only `layout` lines. */
type Layout = Record<string, { x: number; y: number }>;

/** A blueprint read from a file; an empty file is a new blueprint. The file is
 *  text people and the agent edit by hand, so what is left out gets a default. */
export function parseBlueprint(text: string, name: string): Blueprint {
  if (!text.trim()) return emptyBlueprint(name);
  const raw = JSON.parse(text) as Partial<Blueprint> & { layout?: Layout };
  if (raw.version !== 1 || !Array.isArray(raw.parts) || !Array.isArray(raw.wires)) {
    throw new Error("This is not a Workspace blueprint (version 1).");
  }
  const { layout = {}, ...rest } = raw;
  const parts = raw.parts.map((p, i) => {
    if (typeof p?.id !== "string" || typeof p.type !== "string") throw new Error(`Part ${i + 1} needs an "id" and a "type".`);
    // Files from before `layout` keep x and y on each part.
    const at = layout[p.id] ?? { x: p.x ?? 0, y: p.y ?? 0 };
    const { contract, ...q } = p;
    return { ...q, label: p.label ?? p.id, notes: p.notes ?? "", set: p.set ?? {}, x: Number(at.x) || 0, y: Number(at.y) || 0, ...only("contract", normaliseContract(contract)) };
  });
  const wires = raw.wires.map((w, i) => {
    if (typeof w?.from !== "string" || typeof w.to !== "string") throw new Error(`Wire ${i + 1} needs a "from" and a "to".`);
    const { op, sends, ...v } = w;
    const word = (x: unknown) => (typeof x === "string" && x.trim() ? x.trim() : undefined);
    return { ...v, id: w.id ?? `${w.from}-${w.to}-${i}`, kind: w.kind ?? "http", calls: w.calls ?? 1, notes: w.notes ?? "", ...only("op", word(op)), ...only("sends", word(sends)) };
  });
  const customParts = (Array.isArray(raw.customParts) ? raw.customParts : []).map(({ contract, ...t }) => ({ ...t, ...only("contract", normaliseContract(contract)) }));
  const { sandbox, ...base } = rest;
  return { ...emptyBlueprint(name), ...base, parts, wires, customParts, ...only("sandbox", normaliseSandbox(sandbox)) } as Blueprint;
}

/** `{ [key]: v }`, or nothing when `v` is: a file read back keeps no empty keys. */
const only = <V>(key: string, v: V | undefined) => (v === undefined ? {} : { [key]: v });

const text = (v: unknown, max: number) => (typeof v === "string" ? v.trim().slice(0, max) : "");

/** A contract fit to keep, whatever `v` was (a file, a model's reply); none if it says nothing. */
export function normaliseContract(v: unknown): Contract | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  const r = v as Record<string, unknown>;
  const lines = (x: unknown) => (Array.isArray(x) ? x.map((l) => text(l, 300)).filter(Boolean).slice(0, 50) : []);
  const operations = (Array.isArray(r.operations) ? r.operations : [])
    .filter((o): o is Record<string, unknown> => typeof o === "object" && o !== null)
    .map((o) => ({ name: text(o.name, 80), takes: text(o.takes, 300), returns: text(o.returns, 300) }))
    .filter((o) => o.name)
    .slice(0, 50);
  const c = { operations, data: lines(r.data), rules: lines(r.rules) };
  return operations.length || c.data.length || c.rules.length ? c : undefined;
}

/** The sandbox section, whatever the file says; none without a start command. */
export function normaliseSandbox(v: unknown): Sandbox | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  const r = v as Record<string, unknown>;
  const start = text(r.start, 2000);
  if (!start) return undefined;
  const probes: Record<string, Probe> = {};
  for (const [id, p] of Object.entries(typeof r.probes === "object" && r.probes ? r.probes : {})) {
    if (typeof p !== "object" || p === null) continue;
    const q = p as Record<string, unknown>;
    const url = text(q.url, 2000);
    if (!url) continue;
    // The agent may write a body as JSON rather than as text.
    const body = typeof q.body === "object" && q.body !== null ? JSON.stringify(q.body) : text(q.body, 20000);
    probes[id] = { method: (text(q.method, 10) || "GET").toUpperCase(), url, ...(body ? { body } : {}) };
  }
  const opt = (key: string) => (text(r[key], 2000) ? { [key]: text(r[key], 2000) } : {});
  return { start, ...opt("stop"), ...opt("dir"), ...opt("ready"), probes };
}

/** The field names in "items, customerId: string, total". */
export const fieldsOf = (s: string | undefined) =>
  (s ?? "")
    .split(",")
    .map((f) => f.trim().split(/[\s:]/)[0])
    .filter(Boolean);

export const emptyContract = (): Contract => ({ operations: [], data: [], rules: [] });

/** The file's text: types it uses, parts without positions, then the layout. */
export function blueprintText(bp: Blueprint): string {
  const kept = forSave(bp);
  const layout: Layout = Object.fromEntries(kept.parts.map((p) => [p.id, { x: p.x, y: p.y }]));
  const parts = kept.parts.map(({ x: _x, y: _y, ...p }) => p);
  return JSON.stringify({ ...kept, parts, layout }, null, 2) + "\n";
}

/** A readable id from `name`, not in `taken`: "orders-api", then "orders-api-2". */
export function readableId(name: string, taken: Iterable<string>): string {
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "part";
  const used = new Set(taken);
  let id = base;
  for (let n = 2; used.has(id); n++) id = `${base}-${n}`;
  return id;
}

// ---- the inventory: parts and connectors shared by every blueprint ----------

export interface Library {
  version: 1;
  parts: PartType[];
  connectors: ConnectorType[];
}

export const emptyLibrary = (): Library => ({ version: 1, parts: [], connectors: [] });

export function parseLibrary(text: string): Library {
  if (!text.trim()) return emptyLibrary();
  const raw = JSON.parse(text) as Partial<Library>;
  return { version: 1, parts: Array.isArray(raw.parts) ? raw.parts : [], connectors: Array.isArray(raw.connectors) ? raw.connectors : [] };
}

/** The inventory heading a custom part or connector sits under. */
export const categoryOf = (t: { category?: string }) => t.category?.trim() || "General";

/** The `n` most used of `items` (by `uses`, counts per id); ties keep their order. */
export function mostUsed<T extends { id: string }>(items: T[], uses: Record<string, number>, n: number): T[] {
  return items
    .map((t, i) => ({ t, i, c: uses[t.id] ?? 0 }))
    .sort((a, b) => b.c - a.c || a.i - b.i)
    .slice(0, n)
    .map((x) => x.t);
}

/** Does a part or connector type match a search (name, description or category)? */
export const matches = (t: { name: string; description?: string; category?: string }, q: string) =>
  `${t.name} ${t.description ?? ""} ${categoryOf(t)}`.toLowerCase().includes(q.trim().toLowerCase());

/** `items` by category: General first, then the rest A to Z. */
export function byCategory<T extends { category?: string }>(items: T[]): [string, T[]][] {
  const groups = new Map<string, T[]>();
  for (const t of items) {
    const c = categoryOf(t);
    (groups.get(c) ?? groups.set(c, []).get(c)!).push(t);
  }
  return [...groups].sort(([a], [b]) => (a === "General" ? -1 : b === "General" ? 1 : a.localeCompare(b)));
}

/** Put `item` in `list`, replacing the one with its id. */
export const upsert = <T extends { id: string }>(list: T[], item: T) =>
  list.some((x) => x.id === item.id) ? list.map((x) => (x.id === item.id ? item : x)) : [...list, item];

/** The blueprint with the whole inventory to hand; the inventory's version of a type wins. */
export function withLibrary(bp: Blueprint, lib: Library): Blueprint {
  const merge = <T extends { id: string }>(own: T[], shared: T[]) => [...shared, ...own.filter((t) => !shared.some((s) => s.id === t.id))];
  return { ...bp, customParts: merge(bp.customParts, lib.parts), customConnectors: merge(bp.customConnectors, lib.connectors) };
}

/** What the file keeps: a copy of each custom type it uses, so it still works
 *  (and builds) without the inventory, on another machine or for the agent. */
export function forSave(bp: Blueprint): Blueprint {
  const parts = new Set(bp.parts.map((p) => p.type));
  const kinds = new Set(bp.wires.map((w) => w.kind));
  return {
    ...bp,
    customParts: bp.customParts.filter((t) => parts.has(t.id)),
    customConnectors: bp.customConnectors.filter((c) => kinds.has(c.id)),
  };
}

export const partTypes = (bp: Blueprint) => [...PARTS, ...bp.customParts];
export const connectorTypes = (bp: Blueprint) => [...CONNECTORS, ...bp.customConnectors];
/** What a placed part does: its own contract, else its type's. */
export const contractOf = (bp: Blueprint, p: Part): Contract | undefined => p.contract ?? typeOf(bp, p).contract;

/** The type of `p`; a type deleted from the file reads as a plain part. */
export function typeOf(bp: Blueprint, p: Part): PartType {
  return (
    partTypes(bp).find((t) => t.id === p.type) ??
    part({ id: p.type, name: p.type, color: "#888", description: "", capacity: 0, latency: 0, accepts: ["*"] })
  );
}

export const value = (bp: Blueprint, p: Part, key: Tunable) => p.set[key] ?? typeOf(bp, p)[key];

export const accepts = (t: PartType, kind: string) => t.accepts.includes("*") || t.accepts.includes(kind);

/** A part type's ports: its own, or the plain "in" and "out" every part had before. */
export function portsOf(t: PartType): Port[] {
  if (t.ports?.length) return t.ports;
  const ins: Port[] = t.accepts.length
    ? [{ id: "in", name: "in", dir: "in", kind: t.accepts.length === 1 && t.accepts[0] !== "*" ? t.accepts[0] : "*" }]
    : [];
  return [...ins, { id: "out", name: "out", dir: "out", kind: "*" }];
}

/** Whether a call over `kind` may arrive at `t` through its port `port`. */
export function takes(t: PartType, port: string | undefined, kind: string): boolean {
  const p = t.ports?.find((x) => x.id === port && x.dir === "in");
  // No port named (or ports derived from `accepts`): the part as a whole decides.
  return p ? p.kind === "*" || p.kind === kind : accepts(t, kind);
}

/** The connectors `ports` take in, as `accepts` records them. */
export function acceptsOf(ports: Port[]): string[] {
  const ins = ports.filter((p) => p.dir === "in").map((p) => p.kind);
  return ins.includes("*") ? ["*"] : [...new Set(ins)];
}

// ---- simulation --------------------------------------------------------------

export interface PartStats {
  /** Requests per second arriving. */
  load: number;
  /** Of those, how many it can take. */
  served: number;
  /** Load against what all copies handle; 1 and over is overloaded. */
  utilization: number;
  /** Time in this part alone, waiting included, ms. */
  latency: number;
  /** Time until its answer is ready, with every call it waits on, ms. */
  total: number;
  /** Share of its requests that succeed, with every call it waits on. */
  ok: number;
}

export interface Finding {
  level: "error" | "warning" | "info";
  area: "bug" | "scale" | "deploy";
  text: string;
  parts: string[];
}

export interface Entry {
  part: string;
  rps: number;
  latency: number;
  ok: number;
}

export interface SimResult {
  parts: Record<string, PartStats>;
  wires: Record<string, number>;
  entries: Entry[];
  findings: Finding[];
  /** How many times this load the system takes before a part saturates. */
  headroom: { scale: number; part: string } | null;
}

/** Steady-state flow: load through every part, then latency and success back up. */
function flow(bp: Blueprint, scale: number, down: Set<string>) {
  const ids = new Set(bp.parts.map((p) => p.id));
  const kinds = new Map(connectorTypes(bp).map((c) => [c.id, c]));
  const wires = bp.wires.filter((w) => ids.has(w.from) && ids.has(w.to) && w.from !== w.to);
  const byId = new Map(bp.parts.map((p) => [p.id, p]));
  const out = new Map<string, Wire[]>();
  const into = new Map<string, Wire[]>();
  for (const w of wires) {
    (out.get(w.from) ?? out.set(w.from, []).get(w.from)!).push(w);
    (into.get(w.to) ?? into.set(w.to, []).get(w.to)!).push(w);
  }

  // Kahn's order; parts on a loop come last, fed only by what came before.
  const order: string[] = [];
  const left = new Map(bp.parts.map((p) => [p.id, into.get(p.id)?.length ?? 0]));
  const ready = bp.parts.filter((p) => left.get(p.id) === 0).map((p) => p.id);
  while (ready.length) {
    const id = ready.shift()!;
    order.push(id);
    for (const w of out.get(id) ?? []) {
      left.set(w.to, left.get(w.to)! - 1);
      if (left.get(w.to) === 0) ready.push(w.to);
    }
  }
  const looped = bp.parts.map((p) => p.id).filter((id) => !order.includes(id));
  order.push(...looped);

  const stats: Record<string, PartStats> = {};
  const wireLoad: Record<string, number> = {};
  for (const id of order) {
    const p = byId.get(id)!;
    let load = value(bp, p, "source") * scale;
    for (const w of into.get(id) ?? []) load += wireLoad[w.id] ?? 0;
    const room = value(bp, p, "capacity") * Math.max(1, value(bp, p, "replicas"));
    const served = down.has(id) ? 0 : room > 0 ? Math.min(load, room) : load;
    const utilization = room > 0 ? load / room : 0;
    const own = value(bp, p, "latency");
    stats[id] = {
      load,
      served,
      utilization,
      // M/M/1 per copy: waiting grows as 1 / (1 - utilization).
      latency: utilization >= 1 ? Infinity : own / (1 - utilization),
      total: 0,
      ok: 0,
    };
    for (const w of out.get(id) ?? []) wireLoad[w.id] = served * w.calls;
  }

  // Latency and success, children first; a loop back counts as nothing.
  const visiting = new Set<string>();
  const done = new Set<string>();
  const finish = (id: string) => {
    if (done.has(id) || visiting.has(id)) return;
    visiting.add(id);
    const s = stats[id];
    const p = byId.get(id)!;
    let total = s.latency;
    let ok = down.has(id) ? 0 : (1 - value(bp, p, "errorRate")) * (s.load > 0 ? s.served / s.load : 1);
    for (const w of out.get(id) ?? []) {
      const c = kinds.get(w.kind);
      if (!c?.sync) continue;
      finish(w.to);
      if (!done.has(w.to)) continue;
      total += w.calls * (c.latency + stats[w.to].total);
      ok *= stats[w.to].ok ** w.calls;
    }
    s.total = total;
    s.ok = ok;
    visiting.delete(id);
    done.add(id);
  };
  for (const id of order) finish(id);

  const entries: Entry[] = bp.parts
    .filter((p) => value(bp, p, "source") > 0)
    .map((p) => ({ part: p.id, rps: value(bp, p, "source") * scale, latency: stats[p.id].total, ok: stats[p.id].ok }));
  return { stats, wireLoad, entries, wires, out, kinds, byId };
}

/** What the sandbox measured for one part, through its probe. */
export interface Measured {
  requests: number;
  failures: number;
  /** Typical time per request, ms. */
  p50: number;
}

/** Each measured part's own numbers. A probe's time includes the parts it
 *  waits on, so theirs come off: measured where they were, else predicted. */
export function calibrate(bp: Blueprint, measured: Record<string, Measured>): Record<string, { latency: number; errorRate: number }> {
  const { stats, out, kinds, byId } = flow(bp, 1, new Set());
  const result: Record<string, { latency: number; errorRate: number }> = {};
  for (const [id, m] of Object.entries(measured)) {
    if (!byId.has(id) || m.requests <= 0) continue;
    let below = 0;
    let okBelow = 1;
    for (const w of out.get(id) ?? []) {
      const c = kinds.get(w.kind);
      if (!c?.sync) continue;
      const child = measured[w.to];
      // Measured children ran on this machine: their time already holds the network's.
      const seen = child && child.requests > 0;
      below += w.calls * (seen ? child.p50 : c.latency + stats[w.to].total);
      okBelow *= (seen ? 1 - child.failures / child.requests : stats[w.to].ok) ** w.calls;
    }
    const ok = 1 - m.failures / m.requests;
    const errorRate = okBelow > 0 ? Math.min(1, Math.max(0, 1 - ok / okBelow)) : 0;
    result[id] = { latency: Math.max(0, Math.round(m.p50 - below)), errorRate: Math.round(errorRate * 10000) / 10000 };
  }
  return result;
}

/** Run the blueprint at `scale` times its load, with the `down` parts failed. */
export function simulate(bp: Blueprint, scale = 1, down: Set<string> = new Set()): SimResult {
  const { stats, wireLoad, entries, wires, out, kinds, byId } = flow(bp, scale, down);
  const findings: Finding[] = [];
  const name = (id: string) => byId.get(id)?.label || id;
  const add = (level: Finding["level"], area: Finding["area"], text: string, parts: string[]) =>
    findings.push({ level, area, text, parts });

  // ---- bugs: wiring that cannot work
  for (const w of bp.wires) {
    if (!byId.has(w.from) || !byId.has(w.to)) {
      const missing = byId.has(w.from) ? w.to : w.from;
      add("error", "bug", `A wire from "${w.from}" to "${w.to}" names a part that is not in this blueprint ("${missing}").`, [w.from, w.to].filter((id) => byId.has(id)));
      continue;
    }
    if (w.from === w.to) {
      add("error", "bug", `${name(w.from)} calls itself.`, [w.from]);
      continue;
    }
    const c = kinds.get(w.kind);
    if (!c) {
      add("error", "bug", `The wire ${name(w.from)} → ${name(w.to)} uses a connector that no longer exists.`, [w.from, w.to]);
      continue;
    }
    const t = typeOf(bp, byId.get(w.to)!);
    const src = typeOf(bp, byId.get(w.from)!);
    const inPort = t.ports?.find((x) => x.id === w.toPort);
    const outPort = src.ports?.find((x) => x.id === w.fromPort);
    if (t.ports?.length && w.toPort && !inPort) {
      add("error", "bug", `${name(w.from)} is wired to a port ${name(w.to)} no longer has.`, [w.from, w.to]);
    } else if (src.ports?.length && w.fromPort && !outPort) {
      add("error", "bug", `${name(w.from)}'s wire to ${name(w.to)} leaves from a port ${name(w.from)} no longer has.`, [w.from, w.to]);
    } else if (inPort && !takes(t, w.toPort, w.kind)) {
      add("error", "bug", `${name(w.from)} talks to ${name(w.to)}'s "${inPort.name}" over ${c.name}, but that port takes ${kinds.get(inPort.kind)?.name ?? inPort.kind}.`, [w.from, w.to]);
    } else if (!inPort && !accepts(t, w.kind)) {
      const list = t.accepts.map((k) => kinds.get(k)?.name ?? k).join(", ");
      add("error", "bug", `${name(w.from)} talks to ${name(w.to)} over ${c.name}, but a ${t.name} ${list ? `only takes ${list}` : "takes no incoming calls"}.`, [w.from, w.to]);
    } else if (outPort && outPort.kind !== "*" && outPort.kind !== w.kind) {
      add("error", "bug", `${name(w.from)}'s "${outPort.name}" sends ${kinds.get(outPort.kind)?.name ?? outPort.kind}, not ${c.name}.`, [w.from, w.to]);
    }
    // The operation it calls, and what it sends, against the target's contract.
    const ops = contractOf(bp, byId.get(w.to)!)?.operations ?? [];
    const names = ops.map((o) => o.name).join(", ");
    const op = w.op?.trim();
    const found = op && ops.find((o) => o.name === op);
    if (op && !ops.length) {
      add("warning", "bug", `${name(w.from)} calls ${op} on ${name(w.to)}, whose contract lists no operations.`, [w.from, w.to]);
    } else if (op && !found) {
      add("error", "bug", `${name(w.from)} calls ${op} on ${name(w.to)}, which has no such operation (it has ${names}).`, [w.from, w.to]);
    } else if (found && w.sends?.trim()) {
      const sent = new Set(fieldsOf(w.sends));
      const missing = fieldsOf(found.takes).filter((f) => !sent.has(f));
      if (missing.length) {
        add("error", "bug", `${name(w.from)} calls ${name(w.to)}.${op} without ${missing.join(", ")}: it sends ${[...sent].join(", ")}, and ${op} takes ${found.takes}.`, [w.from, w.to]);
      }
    } else if (!op && ops.length > 1) {
      add("warning", "bug", `Which of ${name(w.to)}'s operations does ${name(w.from)} call (${names})? Pick one on the wire.`, [w.from, w.to]);
    }
  }

  // Loops: waiting ones never finish; message ones can run forever.
  for (const sync of [true, false]) {
    const seen = new Set<string>();
    const stack: string[] = [];
    const via: Wire[] = [];
    const reported = new Set<string>();
    const walk = (id: string) => {
      stack.push(id);
      for (const w of out.get(id) ?? []) {
        if (sync && !kinds.get(w.kind)?.sync) continue;
        const at = stack.indexOf(w.to);
        if (at >= 0) {
          const loop = stack.slice(at);
          const key = [...loop].sort().join();
          // A loop of waiting calls is already reported by the first pass.
          const waits = [...via.slice(at), w].every((x) => kinds.get(x.kind)?.sync);
          if (!reported.has(key) && (sync || !waits)) {
            reported.add(key);
            const path = [...loop, w.to].map(name).join(" → ");
            if (sync) add("error", "bug", `${path} wait on each other: a request never finishes.`, loop);
            else add("warning", "bug", `Messages go round ${path} and can multiply forever. Make sure each pass stops somewhere.`, loop);
          }
        } else if (!seen.has(w.to)) {
          seen.add(w.to);
          via.push(w);
          walk(w.to);
          via.pop();
        }
      }
      stack.pop();
    };
    for (const p of bp.parts) if (!seen.has(p.id)) (seen.add(p.id), walk(p.id));
  }

  if (bp.parts.length && !entries.length) {
    add("warning", "bug", "Nothing starts any traffic. Add Users or a Scheduled job.", []);
  }
  for (const p of bp.parts) {
    const linked = wires.some((w) => w.from === p.id || w.to === p.id);
    if (!linked) add("warning", "bug", `${name(p.id)} is not connected to anything.`, [p.id]);
    const contract = contractOf(bp, p);
    const written = p.notes.trim() || contract?.operations.length || contract?.data.length || contract?.rules.length;
    if (!written && value(bp, p, "source") === 0) {
      add("info", "bug", `${name(p.id)} has no function written (What it should do, or its contract): the agent will guess what it does.`, [p.id]);
    }
    else if (entries.length && stats[p.id].load === 0 && value(bp, p, "source") === 0 && !down.has(p.id)) {
      add("warning", "bug", `${name(p.id)} never receives a request.`, [p.id]);
    }
  }

  // ---- scaling
  let worst: { id: string; u: number } | null = null;
  for (const p of bp.parts) {
    const s = stats[p.id];
    if (down.has(p.id) || s.utilization === 0) continue;
    if (!worst || s.utilization > worst.u) worst = { id: p.id, u: s.utilization };
    const each = value(bp, p, "capacity");
    const copies = Math.max(1, value(bp, p, "replicas"));
    if (s.utilization >= 1) {
      add("error", "scale", `${name(p.id)} is overloaded: ${rate(s.load)} req/s against a limit of ${rate(each * copies)} (${copies} × ${rate(each)}). Requests are dropped. It needs ${Math.ceil(s.load / each)} copies, or less traffic sent its way.`, [p.id]);
    } else if (s.utilization >= 0.7) {
      add("warning", "scale", `${name(p.id)} runs at ${Math.round(s.utilization * 100)}% of its limit: little room for a spike.`, [p.id]);
    }
  }
  // Before anything saturates, every part's load grows in step with the
  // traffic, so the first part to fill up sets how far the load can grow.
  const headroom = worst && worst.u < 1 ? { scale: 1 / worst.u, part: worst.id } : null;
  if (headroom) {
    add("info", "scale", `Holds up to ${headroom.scale.toFixed(1)}× this load before ${name(headroom.part)} fills up.`, [headroom.part]);
  }

  // ---- deployment
  for (const e of entries) {
    if (e.latency > 30_000) {
      add("error", "deploy", `A request from ${name(e.part)} takes ${ms(e.latency)}: past the 30 s most gateways and browsers wait. Move the slow work behind a queue.`, [e.part]);
    } else if (e.latency > 10_000) {
      add("warning", "deploy", `A request from ${name(e.part)} takes ${ms(e.latency)}. Stream progress, or answer first and finish the work behind a queue.`, [e.part]);
    }
    if (e.ok < 0.99 && e.ok > 0) {
      add("warning", "deploy", `Only ${(e.ok * 100).toFixed(1)}% of requests from ${name(e.part)} succeed. Add retries or a fallback around what fails.`, [e.part]);
    }
  }
  const traffic = (r: Entry[]) => r.reduce((sum, e) => sum + e.rps * e.ok, 0);
  const base = traffic(entries);
  const spof: { id: string; lost: number }[] = [];
  for (const p of bp.parts) {
    if (down.has(p.id) || value(bp, p, "source") > 0 || stats[p.id].load === 0) continue;
    if (value(bp, p, "replicas") > 1) {
      if (typeOf(bp, p).stateful) {
        add("info", "deploy", `${name(p.id)} runs ${value(bp, p, "replicas")} copies and holds data, so it needs replication (a primary with replicas, or a managed cluster).`, [p.id]);
      }
      continue;
    }
    const lost = base > 0 ? 1 - traffic(flow(bp, scale, new Set([...down, p.id])).entries) / base : 0;
    if (lost >= 0.5) spof.push({ id: p.id, lost });
  }
  if (spof.length === 1) {
    const [{ id, lost }] = spof;
    add("warning", "deploy", `${name(id)} is a single point of failure: if its one copy goes down, ${Math.round(lost * 100)}% of requests fail. Run 2 or more copies, or add a fallback.`, [id]);
  } else if (spof.length) {
    const names = spof.map((x) => name(x.id));
    const least = Math.round(Math.min(...spof.map((x) => x.lost)) * 100);
    add("warning", "deploy", `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]} are single points of failure: if the one copy of any of them goes down, ${least === 100 ? "every request fails" : `at least ${least}% of requests fail`}. Run 2 or more copies of each, or add fallbacks.`, spof.map((x) => x.id));
  }

  const rank = { error: 0, warning: 1, info: 2 };
  findings.sort((a, b) => rank[a.level] - rank[b.level]);
  return { parts: stats, wires: wireLoad, entries, findings, headroom };
}

export const rate = (n: number) => (n >= 100 ? Math.round(n).toString() : n >= 1 ? n.toFixed(1) : n.toFixed(2));
export const ms = (n: number) =>
  !Number.isFinite(n) ? "forever (overloaded)" : n >= 1000 ? `${(n / 1000).toFixed(1)} s` : `${Math.round(n)} ms`;

// ---- hand-off to the agent ----------------------------------------------------

/** The blueprint as a build request: everything the agent needs, in words. */
export function buildPrompt(bp: Blueprint, file: string, dir: string, sim: SimResult): string {
  const kinds = new Map(connectorTypes(bp).map((c) => [c.id, c]));
  const byId = new Map(bp.parts.map((p) => [p.id, p]));
  const lines: string[] = [];
  lines.push(`Build the software system drawn in the blueprint \`${file}\`, in one go.`);
  lines.push("");
  lines.push(`Write all of it into ${dir ? `the folder \`${dir}\`` : "this folder"}: real, working code for every part, wired together as drawn. Make sensible choices where the blueprint is silent instead of asking. When you are done there must be a README saying how to run it, the config to run every part together locally (for several services, a docker-compose.yml), and tests for the core paths. Parts that do not depend on each other can be built side by side by helper agents. Run the tests, fix what fails, then summarise what you built.`);
  lines.push("");
  lines.push(`# ${bp.name}`);
  if (bp.description.trim()) lines.push("", bp.description.trim());
  if (bp.stack.trim()) lines.push("", `**Stack wanted:** ${bp.stack.trim()}`);
  lines.push("", "## Parts");
  for (const p of bp.parts) {
    const t = typeOf(bp, p);
    const copies = value(bp, p, "replicas");
    const facts = [
      t.name,
      value(bp, p, "source") > 0 ? `starts ~${rate(value(bp, p, "source"))} requests/s` : "",
      value(bp, p, "capacity") > 0 ? `must handle ${rate(sim.parts[p.id]?.load ?? 0)} req/s (limit ${rate(value(bp, p, "capacity"))} per copy)` : "",
      value(bp, p, "latency") > 0 ? `about ${ms(value(bp, p, "latency"))} per request` : "",
      copies > 1 ? `${copies} copies` : "",
      t.stateful ? "holds data" : "",
    ].filter(Boolean);
    lines.push(`- **${p.label}** (id \`${p.id}\`; ${facts.join("; ")})${t.description ? ` — ${t.description}` : ""}`);
    if (p.code) lines.push(`  - Its code so far: \`${p.code}\``);
    if (p.notes.trim()) lines.push(`  - What it does: ${p.notes.trim().replace(/\n+/g, " ")}`);
    if (t.ports?.length) {
      const port = (x: Port) => `${x.name} (${x.kind === "*" ? "any connector" : (kinds.get(x.kind)?.name ?? x.kind)})`;
      const ins = t.ports.filter((x) => x.dir === "in").map(port);
      const outs = t.ports.filter((x) => x.dir === "out").map(port);
      lines.push(`  - Ports: ${[ins.length ? `in ${ins.join(", ")}` : "", outs.length ? `out ${outs.join(", ")}` : ""].filter(Boolean).join("; ")}`);
    }
    const settings = (t.settings ?? []).map((x) => `${x.label} = ${p.values?.[x.key] ?? x.default}`).filter((x) => !x.endsWith("= "));
    if (settings.length) lines.push(`  - Settings: ${settings.join("; ")}`);
    const contract = contractOf(bp, p);
    if (contract?.operations.length) {
      lines.push(`  - Operations: ${contract.operations.map((o) => `${o.name}(${o.takes})${o.returns ? ` → ${o.returns}` : ""}`).join("; ")}`);
    }
    if (contract?.data.length) lines.push(`  - Keeps: ${contract.data.join("; ")}`);
    if (contract?.rules.length) lines.push(`  - Rules: ${contract.rules.join("; ")}`);
  }
  lines.push("", "## Connections");
  if (!bp.wires.length) lines.push("- (none)");
  for (const w of bp.wires) {
    const portName = (partId: string, port?: string) => {
      const owner = byId.get(partId);
      return (owner && typeOf(bp, owner).ports?.find((x) => x.id === port)?.name) || "";
    };
    const end = (partId: string, port?: string) => {
      const label = byId.get(partId)?.label ?? partId;
      const pn = portName(partId, port);
      return pn ? `${label}.${pn}` : label;
    };
    const from = end(w.from, w.fromPort);
    const to = end(w.to, w.toPort);
    const c = kinds.get(w.kind);
    const how = c ? `${c.name}, ${c.sync ? "waits for the answer" : "does not wait"}` : w.kind;
    const calls = w.op ? `, calls ${w.op}${w.sends ? ` with ${w.sends}` : ""}` : "";
    lines.push(`- ${from} → ${to}: ${how}${calls}${w.calls !== 1 ? `, ${w.calls} calls per request` : ""}${w.notes.trim() ? ` — ${w.notes.trim()}` : ""}`);
  }
  const custom = [...bp.customParts.map((t) => `- Part "${t.name}": ${t.description || "(no description)"}`), ...bp.customConnectors.map((c) => `- Connector "${c.name}": ${c.description || "(no description)"}`)];
  if (custom.length) lines.push("", "## Custom parts and connectors the user defined", ...custom);
  lines.push("", "## What the simulation found");
  for (const e of sim.entries) {
    lines.push(`- From ${byId.get(e.part)?.label}: ${rate(e.rps)} req/s, ~${ms(e.latency)} per request, ${(e.ok * 100).toFixed(1)}% succeed.`);
  }
  for (const f of sim.findings) lines.push(`- ${f.level}: ${f.text}`);
  lines.push("", `The full blueprint (every number and note) is the JSON in \`${file}\`; read it if you need more detail.`);
  lines.push("", `Build every operation each part lists, with the names, inputs and outputs given, and follow its rules.`);
  lines.push("", `When the code is written, update \`${file}\` in two ways and change nothing else in it:`);
  lines.push(`1. Give each entry in "parts" a "code" field with the file or folder you wrote for it, relative to the blueprint's folder (for example "code": "services/orders-api").`);
  lines.push(`2. Add a top-level "sandbox" object, so Workspace can start the system and measure each part: {"start": the one command that starts every part locally (for example "docker compose up --build"), "stop": a command that stops it when ending start is not enough (for example "docker compose down"; leave it out otherwise), "dir": the folder to run them in, relative to the blueprint's folder (leave it out for that folder), "ready": a localhost URL that answers once everything is up, "probes": {"<part id>": {"method": "GET" or "POST", "url": a localhost URL that exercises that part, through one of its operations where it has them, "body": the JSON request body, if it needs one}}}. Give a probe to every part that answers requests directly; leave out the ones only reached through another part (a database, a queue).`);
  return lines.join("\n");
}
