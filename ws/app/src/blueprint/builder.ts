// The parts builder: a description in, a part (or connector) for the
// inventory out. Like VectorShift's node configs, a part is plain data: its
// numbers for the simulation, named ports, and settings.
//
// Two builders, as for knowledge graphs:
// - free: reads the description itself, by keyword. No AI, no cost, instant.
// - AI: a model writes the part as JSON (`partPrompt`, then `parseAiPart`).
// Both go through `normalisePart`/`normaliseConnector`, so whatever comes back,
// above all from a model, is checked before it can reach the inventory.

import { CONNECTORS, acceptsOf, normaliseContract, type ConnectorType, type PartType, type Port, type Setting } from "./model.ts";

const uid = () => Math.random().toString(36).slice(2, 10);

// ---- free builder: parts ---------------------------------------------------------

interface Archetype {
  /** Its keywords. The archetype whose keywords appear most wins; ties go to the earlier one. */
  test: RegExp;
  noun: string;
  category: string;
  color: string;
  base: Pick<PartType, "capacity" | "latency" | "errorRate" | "stateful" | "source">;
  ports: Omit<Port, "id">[];
  settings: Setting[];
  /** Products this archetype knows; the one named in the description becomes a setting's default. */
  products?: { key: string; names: [RegExp, string][] };
}

const choice = (key: string, label: string, options: string[]): Setting => ({ key, label, type: "choice", default: options[0], options });
const text = (key: string, label: string, def = ""): Setting => ({ key, label, type: "text", default: def });
const num = (key: string, label: string, def: string): Setting => ({ key, label, type: "number", default: def });

const LANGUAGES: [RegExp, string][] = [
  [/\btype ?script\b|\bnode(\.?js)?\b|\bts\b/i, "TypeScript"],
  [/\bpython\b|\bfastapi\b|\bdjango\b|\bflask\b/i, "Python"],
  [/\bgo(lang)?\b/i, "Go"],
  [/\brust\b/i, "Rust"],
  [/\bjava\b|\bspring\b/i, "Java"],
];

const ARCHETYPES: Archetype[] = [
  {
    test: /\bvector\b|\bpinecone\b|\bqdrant\b|\bweaviate\b|\bpgvector\b|\bchroma\b|\bembedding (store|index)\b/i,
    noun: "vector store", category: "AI", color: "#c8902a",
    base: { capacity: 300, latency: 15, errorRate: 0, stateful: true, source: 0 },
    ports: [{ name: "search", dir: "in", kind: "http" }, { name: "upsert", dir: "in", kind: "http" }],
    settings: [choice("engine", "Engine", ["pgvector", "Pinecone", "Qdrant", "Weaviate", "Chroma"]), num("dimensions", "Dimensions", "1536")],
    products: { key: "engine", names: [[/pgvector/i, "pgvector"], [/pinecone/i, "Pinecone"], [/qdrant/i, "Qdrant"], [/weaviate/i, "Weaviate"], [/chroma/i, "Chroma"]] },
  },
  {
    test: /\bembed(der|ding|dings)?\b/i,
    noun: "embedder", category: "AI", color: "#e0c040",
    base: { capacity: 50, latency: 80, errorRate: 0.005, stateful: false, source: 0 },
    ports: [{ name: "text", dir: "in", kind: "call" }],
    settings: [choice("model", "Model", ["text-embedding-3-small", "text-embedding-3-large", "bge-small-en"]), num("dimensions", "Dimensions", "1536")],
  },
  {
    test: /\bagent\b/i,
    noun: "agent", category: "AI", color: "#9a5cf0",
    base: { capacity: 2, latency: 500, errorRate: 0.02, stateful: false, source: 0 },
    ports: [{ name: "task", dir: "in", kind: "*" }, { name: "model", dir: "out", kind: "llm" }, { name: "tools", dir: "out", kind: "*" }],
    settings: [text("model", "Model", "claude-sonnet"), text("tools", "Tools it may use")],
  },
  {
    test: /\bllm\b|\bgpt[-\w.]*|\bclaude\b|\bgemini\b|\bllama\b|\bmistral\b|\blanguage model\b|\bchat ?bot model\b/i,
    noun: "LLM", category: "AI", color: "#b07cff",
    base: { capacity: 5, latency: 3000, errorRate: 0.01, stateful: false, source: 0 },
    ports: [{ name: "prompt", dir: "in", kind: "llm" }],
    settings: [text("model", "Model", "claude-sonnet"), num("temperature", "Temperature", "0.2"), num("max_tokens", "Max tokens", "1024")],
    products: { key: "model", names: [[/\b(gpt[-\w.]*)/i, "$1"], [/\bclaude[-\w.]*/i, "claude-sonnet"], [/\bgemini[-\w.]*/i, "gemini"], [/\bllama[-\w.]*/i, "llama"], [/\bmistral[-\w.]*/i, "mistral"]] },
  },
  {
    test: /\breranker?\b|\brerank(ing)?\b/i,
    noun: "reranker", category: "AI", color: "#d06fa0",
    base: { capacity: 30, latency: 120, errorRate: 0.005, stateful: false, source: 0 },
    ports: [{ name: "candidates", dir: "in", kind: "call" }],
    settings: [text("model", "Model", "bge-reranker"), num("top_k", "Keep the top", "5")],
  },
  {
    test: /\bcache\b|\bredis\b|\bmemcached?\b/i,
    noun: "cache", category: "Data", color: "#e05555",
    base: { capacity: 20000, latency: 1, errorRate: 0, stateful: true, source: 0 },
    ports: [{ name: "get/set", dir: "in", kind: "kv" }],
    settings: [choice("engine", "Engine", ["Redis", "Memcached", "In-memory"]), num("ttl", "Expire after (s)", "300")],
    products: { key: "engine", names: [[/redis/i, "Redis"], [/memcache/i, "Memcached"]] },
  },
  {
    test: /\bqueue\b|\bkafka\b|\brabbit(mq)?\b|\bsqs\b|\bpub ?\/? ?sub\b|\bbroker\b|\bstreams?\b/i,
    noun: "queue", category: "Messaging", color: "#2fbf8f",
    base: { capacity: 5000, latency: 2, errorRate: 0, stateful: true, source: 0 },
    ports: [{ name: "publish", dir: "in", kind: "message" }, { name: "deliver", dir: "out", kind: "message" }],
    settings: [choice("broker", "Broker", ["RabbitMQ", "Kafka", "SQS", "Redis Streams"]), text("topic", "Queue or topic")],
    products: { key: "broker", names: [[/kafka/i, "Kafka"], [/rabbit/i, "RabbitMQ"], [/sqs/i, "SQS"], [/redis stream/i, "Redis Streams"]] },
  },
  {
    test: /\bdatabase\b|\bpostgres(ql)?\b|\bmysql\b|\bmariadb\b|\bsqlite\b|\bmongo(db)?\b|\bdb\b|\bsql\b|\bdynamo(db)?\b/i,
    noun: "database", category: "Data", color: "#e0a030",
    base: { capacity: 800, latency: 4, errorRate: 0, stateful: true, source: 0 },
    ports: [{ name: "queries", dir: "in", kind: "sql" }],
    settings: [choice("engine", "Engine", ["PostgreSQL", "MySQL", "SQLite", "MongoDB", "DynamoDB"]), num("storage_gb", "Storage (GB)", "20")],
    products: { key: "engine", names: [[/postgres/i, "PostgreSQL"], [/mysql|mariadb/i, "MySQL"], [/sqlite/i, "SQLite"], [/mongo/i, "MongoDB"], [/dynamo/i, "DynamoDB"]] },
  },
  {
    test: /\bs3\b|\bstorage\b|\bbucket\b|\bblob\b|\bfiles?\b|\buploads?\b/i,
    noun: "file storage", category: "Data", color: "#c98a2e",
    base: { capacity: 1000, latency: 30, errorRate: 0, stateful: true, source: 0 },
    ports: [{ name: "files", dir: "in", kind: "http" }],
    settings: [choice("provider", "Provider", ["S3", "Google Cloud Storage", "Azure Blob", "Local disk"])],
    products: { key: "provider", names: [[/\bs3\b/i, "S3"], [/gcs|google cloud/i, "Google Cloud Storage"], [/azure/i, "Azure Blob"]] },
  },
  {
    test: /\bstripe\b|\btwilio\b|\bsendgrid\b|\bpayments?\b|\bemail\b|\bsms\b|\bthird[- ]party\b|\bexternal\b|\bwebhooks?\b/i,
    noun: "external service", category: "External", color: "#7a8597",
    base: { capacity: 50, latency: 200, errorRate: 0.01, stateful: false, source: 0 },
    ports: [{ name: "requests", dir: "in", kind: "http" }],
    settings: [text("provider", "Provider"), text("api_key_env", "API key variable")],
    products: { key: "provider", names: [[/stripe/i, "Stripe"], [/twilio/i, "Twilio"], [/sendgrid/i, "SendGrid"]] },
  },
  {
    test: /\bworker\b|\bbackground\b|\bjobs?\b|\bconsumer\b|\bprocessor\b|\bpipeline step\b/i,
    noun: "worker", category: "Backend", color: "#2fbf8f",
    base: { capacity: 20, latency: 300, errorRate: 0, stateful: false, source: 0 },
    ports: [{ name: "jobs", dir: "in", kind: "message" }, { name: "calls", dir: "out", kind: "*" }],
    settings: [choice("language", "Language", ["TypeScript", "Python", "Go", "Rust", "Java"]), num("concurrency", "Jobs at once", "4")],
    products: { key: "language", names: LANGUAGES },
  },
  {
    test: /\bfront ?end\b|\bwebsite\b|\bweb ?app\b|\bui\b|\breact\b|\bnext\.?js\b|\bvue\b|\bsvelte\b|\bdashboard\b|\bpage\b/i,
    noun: "front end", category: "Frontend", color: "#4f8cff",
    base: { capacity: 2000, latency: 10, errorRate: 0, stateful: false, source: 0 },
    ports: [{ name: "pages", dir: "in", kind: "http" }, { name: "api", dir: "out", kind: "http" }],
    settings: [choice("framework", "Framework", ["React", "Next.js", "Vue", "Svelte", "Plain HTML"])],
    products: { key: "framework", names: [[/next/i, "Next.js"], [/react/i, "React"], [/vue/i, "Vue"], [/svelte/i, "Svelte"]] },
  },
  {
    test: /\busers?\b|\bcustomers?\b|\bclients?\b|\bvisitors?\b|\bshoppers?\b/i,
    noun: "users", category: "Clients", color: "#5b6b82",
    base: { capacity: 0, latency: 0, errorRate: 0, stateful: false, source: 50 },
    ports: [{ name: "requests", dir: "out", kind: "http" }],
    settings: [],
  },
  {
    // Also what a description with no keywords at all becomes.
    test: /\bapi\b|\bservices?\b|\bback ?end\b|\bservers?\b|\bendpoints?\b|\bmicroservices?\b|\bauth\w*\b|\bgateway\b/i,
    noun: "service", category: "Backend", color: "#3d7be0",
    base: { capacity: 200, latency: 25, errorRate: 0, stateful: false, source: 0 },
    ports: [{ name: "requests", dir: "in", kind: "http" }, { name: "calls", dir: "out", kind: "*" }],
    settings: [choice("language", "Language", ["TypeScript", "Python", "Go", "Rust", "Java"]), text("framework", "Framework")],
    products: { key: "language", names: LANGUAGES },
  },
];

const titleCase = (s: string) => s.replace(/^\w/, (c) => c.toUpperCase());

/** "called X" / "named X" / a quoted name, if the description gives one. */
function givenName(desc: string): string | null {
  const m = /\b(?:called|named)\s+["“']?([^"”',.;:]{2,40}?)["”']?(?=$|[,.;:]|\s+(?:that|which|who|to|for|with|and|using|uses|in|on|at|from|by|takes|handles|runs|stores|holds|is|was)\b)/i.exec(desc) ?? /["“]([^"”]{2,40})["”]/.exec(desc);
  return m ? m[1].trim() : null;
}

/** Numbers the description states: a request rate, times, copies, failure rates. */
function numbers(desc: string): Partial<Pick<PartType, "latency" | "replicas" | "errorRate">> & { rate?: number } {
  const out: Partial<Pick<PartType, "latency" | "replicas" | "errorRate">> & { rate?: number } = {};
  const n = (s: string, k?: string) => Number(s) * (k && /k/i.test(k) ? 1000 : 1);
  const rate = /(\d+(?:\.\d+)?)\s*(k)?\s*(?:req(?:uest)?s?|rps|qps|calls|messages|jobs|queries)\s*(?:\/\s*s(?:ec)?|per\s*s(?:ec(?:ond)?)?|a\s*second)\b|(\d+(?:\.\d+)?)\s*(k)?\s*(?:rps|qps)\b/i.exec(desc);
  if (rate) out.rate = rate[1] ? n(rate[1], rate[2]) : n(rate[3], rate[4]);
  const ms = /(\d+(?:\.\d+)?)\s*ms\b/i.exec(desc);
  const sec = /(\d+(?:\.\d+)?)\s*(?:s|secs?|seconds?)\b(?!\s*\/)/i.exec(desc);
  if (ms) out.latency = Number(ms[1]);
  else if (sec) out.latency = Number(sec[1]) * 1000;
  const copies = /(\d+)\s*(?:replicas?|copies|instances?|nodes|pods)\b/i.exec(desc);
  if (copies) out.replicas = Math.max(1, Number(copies[1]));
  const fail = /(\d+(?:\.\d+)?)\s*%\s*(?:of\s*\w+\s*)?(?:errors?|fail(?:s|ures?)?|error rate)/i.exec(desc) ?? /(?:errors?|fails?|failure rate|error rate)\D{0,12}(\d+(?:\.\d+)?)\s*%/i.exec(desc);
  if (fail) out.errorRate = Math.min(1, Number(fail[1]) / 100);
  return out;
}

/** The free builder: a part from what the description names. */
export function buildPartFree(desc: string): PartType {
  const hits = (re: RegExp) => desc.match(new RegExp(re.source, "gi"))?.length ?? 0;
  const a = ARCHETYPES.reduce((best, x) => (hits(x.test) > hits(best.test) ? x : best), ARCHETYPES[ARCHETYPES.length - 1]);
  const settings = a.settings.map((s) => ({ ...s, options: s.options && [...s.options] }));
  let product: string | null = null;
  if (a.products) {
    for (const [re, label] of a.products.names) {
      const m = re.exec(desc);
      if (m) {
        product = label.replace("$1", m[1] ?? "");
        break;
      }
    }
    const s = settings.find((x) => x.key === a.products!.key);
    if (s && product) {
      s.default = product;
      if (s.type === "choice" && !s.options!.includes(product)) s.options = [product, ...s.options!];
    }
  }
  const name = givenName(desc) ?? titleCase(product && a.noun !== "users" ? `${product} ${a.noun}` : a.noun);
  const { rate, ...stated } = numbers(desc);
  return normalisePart({
    id: `custom-${uid()}`,
    name,
    color: a.color,
    description: desc.trim(),
    category: a.category,
    replicas: 1,
    ...a.base,
    ...stated,
    // Users start requests at the stated rate; every other part handles them.
    ...(rate === undefined ? {} : a.noun === "users" ? { source: rate } : { capacity: rate }),
    accepts: [],
    ports: a.ports.map((p) => ({ ...p, id: "" })),
    settings,
  });
}

// ---- free builder: connectors ---------------------------------------------------------

const PROTOCOLS: [RegExp, string, boolean][] = [
  [/\bgrpc\b/i, "gRPC", true],
  [/\bgraphql\b/i, "GraphQL", true],
  [/\bweb ?sockets?\b/i, "WebSocket", true],
  [/\bwebhooks?\b/i, "Webhook", false],
  [/\bmqtt\b/i, "MQTT", false],
  [/\bkafka\b/i, "Kafka topic", false],
  [/\bsoap\b/i, "SOAP", true],
  [/\bsse\b|server[- ]sent events?/i, "Server-sent events", false],
  [/\btcp\b/i, "TCP", true],
  [/\brest\b|\bhttp\b/i, "HTTP", true],
];

/** The free builder: a connector from what the description names. */
export function buildConnectorFree(desc: string): ConnectorType {
  const proto = PROTOCOLS.find(([re]) => re.test(desc));
  const waits = /\b(async(hronous)?|fire[- ]and[- ]forget|does ?n[o']t wait|no reply|events?|queued?|publish(es|ed)?)\b/i.test(desc)
    ? false
    : /\b(sync(hronous)?|waits?|request[- ]response|rpc|replies)\b/i.test(desc)
      ? true
      : (proto?.[2] ?? true);
  const ms = /(\d+(?:\.\d+)?)\s*ms\b/i.exec(desc);
  return normaliseConnector({
    id: `custom-${uid()}`,
    name: givenName(desc) ?? proto?.[1] ?? (waits ? "Call" : "Event"),
    color: waits ? "#4f8cff" : "#2fbf8f",
    sync: waits,
    latency: ms ? Number(ms[1]) : waits ? 5 : 2,
    description: desc.trim(),
    category: waits ? "Network" : "Messaging",
  });
}

// ---- AI builder ---------------------------------------------------------------

/** What the model is told: the shape to answer in, and the connectors that exist. */
export function partPrompt(kind: "part" | "connector", connectors: ConnectorType[]): string {
  const list = connectors.map((c) => `- "${c.id}": ${c.name} (${c.sync ? "caller waits" : "fire and forget"})`).join("\n");
  if (kind === "connector") {
    return `You design connectors for a software architecture diagram. A connector is how one part talks to another.
Reply with ONLY one JSON object, no prose, in this shape:
{"name": string, "description": string, "category": string, "color": "#rrggbb",
 "sync": boolean (true if the caller waits for the answer),
 "latency": number (network ms per call)}
Connectors that already exist (do not recreate them):
${list}`;
  }
  return `You design reusable parts for a software architecture diagram that is simulated, then built by a coding agent.
Reply with ONLY one JSON object, no prose, in this shape:
{"name": string (short), "description": string (what it is and does, for the agent that builds it),
 "category": string (e.g. AI, Data, Backend, Frontend, Messaging, External, Clients), "color": "#rrggbb",
 "capacity": number (requests per second one copy handles; 0 = no limit),
 "latency": number (ms one request takes inside it), "errorRate": number (0..1),
 "replicas": integer >= 1, "stateful": boolean (holds data), "source": number (requests per second it starts by itself; 0 unless it is users or a schedule),
 "ports": [{"name": string, "dir": "in" | "out", "kind": connector id or "*"}],
 "settings": [{"key": string, "label": string, "type": "text" | "number" | "choice", "default": string, "options": [string] (for choice)}],
 "contract": {"operations": [{"name": string (e.g. createOrder), "takes": string (comma separated inputs), "returns": string}],
              "data": [string] (what it stores, e.g. "orders(id, items, total)"), "rules": [string] (how it must behave)}}
"in" ports are how others call it; "out" ports are what it calls. The contract says what it does: list the operations others can call on it,
the data it keeps and its rules; leave a list empty when it has none. Use realistic numbers.
Connector ids you may use for port kinds:
${list}`;
}

/** The first JSON object in a model's reply. */
function jsonIn(reply: string): Record<string, unknown> {
  const start = reply.indexOf("{");
  const end = reply.lastIndexOf("}");
  if (start < 0 || end <= start) throw new Error("The model did not answer with a part.");
  const value: unknown = JSON.parse(reply.slice(start, end + 1));
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("The model did not answer with a part.");
  return value as Record<string, unknown>;
}

/** The part in a model's reply; a fresh id, so a reply cannot replace an inventory part. */
export function parseAiPart(reply: string, description: string, connectors: ConnectorType[]): PartType {
  const raw = jsonIn(reply);
  return normalisePart({ ...raw, id: `custom-${uid()}`, description: str(raw.description, 2000) || description.trim() }, connectors);
}

export function parseAiConnector(reply: string, description: string): ConnectorType {
  const raw = jsonIn(reply);
  return normaliseConnector({ ...raw, id: `custom-${uid()}`, description: str(raw.description, 2000) || description.trim() });
}

// ---- checking what a builder made ------------------------------------------------------

const str = (v: unknown, max: number, fallback = "") => (typeof v === "string" ? v.trim().slice(0, max) : fallback);
const nonNeg = (v: unknown, fallback: number) => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : fallback);
const color = (v: unknown, fallback: string) => (typeof v === "string" && /^#[0-9a-f]{6}$/i.test(v) ? v.toLowerCase() : fallback);
const slug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_|_$/g, "") || "x";

/** A part fit for the inventory, whatever `raw` was. */
export function normalisePart(raw: Record<string, unknown> | PartType, connectors: ConnectorType[] = CONNECTORS): PartType {
  const r = raw as Record<string, unknown>;
  const known = new Set(connectors.map((c) => c.id));
  const used = new Set<string>();
  const unique = (base: string) => {
    let id = base;
    for (let i = 2; used.has(id); i++) id = `${base}_${i}`;
    used.add(id);
    return id;
  };
  const ports: Port[] = (Array.isArray(r.ports) ? r.ports : [])
    .slice(0, 12)
    .filter((p): p is Record<string, unknown> => typeof p === "object" && p !== null)
    .map((p) => {
      const name = str(p.name, 30, "port") || "port";
      const kind = typeof p.kind === "string" && (p.kind === "*" || known.has(p.kind)) ? p.kind : "*";
      return { id: str(p.id, 40) || unique(slug(name)), name, dir: p.dir === "out" ? "out" : "in", kind } as Port;
    });
  // An id the model or the file gave can still clash; make every one unique.
  used.clear();
  for (const p of ports) p.id = unique(slug(p.id));
  const keys = new Set<string>();
  const settings: Setting[] = (Array.isArray(r.settings) ? r.settings : [])
    .slice(0, 12)
    .filter((s): s is Record<string, unknown> => typeof s === "object" && s !== null)
    .map((s) => {
      const label = str(s.label, 40) || str(s.key, 40) || "Setting";
      let key = slug(str(s.key, 40) || label);
      while (keys.has(key)) key += "_";
      keys.add(key);
      const type = s.type === "number" || s.type === "choice" ? s.type : "text";
      const options = type === "choice" && Array.isArray(s.options) ? s.options.filter((o): o is string => typeof o === "string").map((o) => o.slice(0, 60)).slice(0, 20) : undefined;
      const def = typeof s.default === "number" ? String(s.default) : str(s.default, 200);
      return {
        key,
        label,
        type: type === "choice" && !options?.length ? "text" : type,
        default: type === "choice" && options?.length && !options.includes(def) ? options[0] : def,
        ...(type === "choice" && options?.length ? { options } : {}),
      } as Setting;
    });
  const contract = normaliseContract(r.contract);
  return {
    id: str(r.id, 60) || `custom-${uid()}`,
    name: str(r.name, 60) || "New part",
    color: color(r.color, "#d9822b"),
    description: str(r.description, 2000),
    category: str(r.category, 40) || undefined,
    capacity: nonNeg(r.capacity, 100),
    latency: nonNeg(r.latency, 20),
    errorRate: Math.min(1, nonNeg(r.errorRate, 0)),
    replicas: Math.max(1, Math.round(nonNeg(r.replicas, 1))),
    stateful: r.stateful === true,
    source: nonNeg(r.source, 0),
    // With no ports, the part takes any connector, as a new one always has.
    accepts: ports.length ? acceptsOf(ports) : ["*"],
    ...(ports.length ? { ports } : {}),
    ...(settings.length ? { settings } : {}),
    ...(contract ? { contract } : {}),
  };
}

/** A connector fit for the inventory, whatever `raw` was. */
export function normaliseConnector(raw: Record<string, unknown> | ConnectorType): ConnectorType {
  const r = raw as Record<string, unknown>;
  return {
    id: str(r.id, 60) || `custom-${uid()}`,
    name: str(r.name, 60) || "New connector",
    color: color(r.color, "#d9822b"),
    sync: r.sync !== false,
    latency: nonNeg(r.latency, 5),
    description: str(r.description, 2000),
    category: str(r.category, 40) || undefined,
  };
}
