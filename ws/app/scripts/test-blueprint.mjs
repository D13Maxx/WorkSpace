// Checks the blueprint simulator: `node scripts/test-blueprint.mjs`.
import assert from "node:assert/strict";
import { blueprintText, buildPrompt, byCategory, emptyBlueprint, forSave, matches, mostUsed, parseBlueprint, parseLibrary, readableId, simulate, upsert, withLibrary } from "../src/blueprint/model.ts";

const part = (id, type, set = {}) => ({ id, type, label: id, x: 0, y: 0, notes: "", set });
const wire = (from, to, kind, calls = 1) => ({ id: `${from}-${to}`, from, to, kind, calls, notes: "" });
const has = (sim, area, text) => sim.findings.some((f) => f.area === area && f.text.includes(text));

// Users → API → DB: 50 req/s, two DB calls each.
const shop = {
  ...emptyBlueprint("shop"),
  parts: [part("users", "users"), part("api", "api"), part("db", "db")],
  wires: [wire("users", "api", "http"), wire("api", "db", "sql", 2)],
};
let sim = simulate(shop);
assert.equal(sim.parts.api.load, 50);
assert.equal(sim.parts.db.load, 100);
assert.equal(sim.parts.api.utilization, 0.25);
assert.equal(sim.findings.filter((f) => f.level === "error").length, 0);
// API (200/s) fills before the DB (800/s): 4× headroom.
assert.equal(sim.headroom.part, "api");
assert.ok(Math.abs(sim.headroom.scale - 4) < 1e-9);
// Latency: users 0 + http 5 + api 25/(1-0.25) + 2 × (sql 1 + db 4/(1-0.125)).
const expected = 5 + 25 / 0.75 + 2 * (1 + 4 / 0.875);
assert.ok(Math.abs(sim.entries[0].latency - expected) < 1e-9);
// One copy each of the API and the DB carries everything.
assert.ok(has(sim, "deploy", "api and db are single points of failure"));

// 10× the users: the API is overloaded and drops requests.
sim = simulate(shop, 10);
assert.ok(has(sim, "scale", "api is overloaded"));
assert.ok(has(sim, "scale", "needs 3 copies"));
assert.equal(sim.parts.api.served, 200);
assert.ok(sim.entries[0].ok < 0.5);

// Taking the DB down fails every request.
sim = simulate(shop, 1, new Set(["db"]));
assert.equal(sim.entries[0].ok, 0);

// Wrong connector, a waiting loop, and a loose part.
const broken = {
  ...emptyBlueprint("broken"),
  parts: [part("users", "users"), part("a", "api"), part("b", "api"), part("db", "db"), part("lonely", "cache")],
  wires: [wire("users", "a", "http"), wire("a", "b", "http"), wire("b", "a", "http"), wire("a", "db", "http")],
};
sim = simulate(broken);
assert.ok(has(sim, "bug", "only takes SQL query"));
assert.ok(has(sim, "bug", "wait on each other"));
assert.ok(has(sim, "bug", "lonely is not connected"));
// A message loop is a warning, not the waiting-loop error.
const looping = {
  ...emptyBlueprint("loop"),
  parts: [part("users", "users"), part("q", "queue"), part("w", "worker")],
  wires: [wire("users", "q", "message"), wire("q", "w", "message"), wire("w", "q", "message")],
};
sim = simulate(looping);
assert.ok(has(sim, "bug", "can multiply forever"));
assert.ok(!has(sim, "bug", "wait on each other"));

// A slow LLM behind a waiting call trips the gateway timeout.
const slow = {
  ...emptyBlueprint("slow"),
  parts: [part("users", "users", { source: 1 }), part("api", "api"), part("llm", "llm", { latency: 40000 })],
  wires: [wire("users", "api", "http"), wire("api", "llm", "llm")],
};
assert.ok(has(simulate(slow), "deploy", "past the 30 s"));

// The hand-off names every part and connection.
const prompt = buildPrompt(shop, "shop.blueprint", "", simulate(shop));
assert.ok(prompt.includes("**api**") && prompt.includes("api → db: SQL query") && prompt.includes("2 calls per request"));

// The inventory: every blueprint sees it, the inventory's version wins, and a
// file keeps copies of only the custom types it uses.
const auth = { id: "custom-auth", name: "Auth", color: "#fff", description: "v2", capacity: 10, latency: 5, errorRate: 0, replicas: 1, stateful: false, source: 0, accepts: ["*"] };
const lib = { ...parseLibrary(""), parts: [auth], connectors: [{ id: "custom-grpc", name: "gRPC", color: "#fff", sync: true, latency: 2, description: "" }] };
const own = {
  ...emptyBlueprint("own"),
  parts: [part("a", "custom-auth"), part("u", "users")],
  wires: [wire("u", "a", "http")],
  customParts: [{ ...auth, description: "v1" }, { ...auth, id: "custom-old", name: "Old" }],
};
const merged = withLibrary(own, lib);
assert.equal(merged.customParts.find((t) => t.id === "custom-auth").description, "v2");
assert.ok(merged.customParts.some((t) => t.id === "custom-old"));
assert.ok(merged.customConnectors.some((c) => c.id === "custom-grpc"));
const saved = forSave(merged);
assert.deepEqual(saved.customParts.map((t) => t.id), ["custom-auth"]);
assert.deepEqual(saved.customConnectors, []);
assert.equal(simulate(merged).parts.a.load, 50);
assert.deepEqual(upsert([auth], { ...auth, name: "B" }).map((t) => t.name), ["B"]);
assert.equal(upsert([auth], { ...auth, id: "x" }).length, 2);
assert.equal(parseLibrary('{"parts": 3}').parts.length, 0);

// Categories: unnamed ones are General, which comes first.
const groups = byCategory([{ category: "Data" }, {}, { category: " AI " }, { category: "" }, { category: "Data" }]);
assert.deepEqual(groups.map(([c, list]) => [c, list.length]), [["General", 2], ["AI", 1], ["Data", 2]]);

// The file: positions in their own section, read back as they were.
const placed = { ...shop, parts: shop.parts.map((p, i) => ({ ...p, x: i * 100, y: 7 })) };
const text = blueprintText(placed);
const file = JSON.parse(text);
assert.ok(file.parts.every((p) => !("x" in p) && !("y" in p)));
assert.deepEqual(file.layout.db, { x: 200, y: 7 });
assert.deepEqual(Object.keys(file).at(-1), "layout"); // layout last: a move is a small diff at the end
assert.deepEqual(parseBlueprint(text, "x").parts, placed.parts);
// Moving a part changes only its layout lines.
const moved = blueprintText({ ...placed, parts: placed.parts.map((p) => (p.id === "api" ? { ...p, x: 999 } : p)) });
const changed = text.split("\n").filter((l, i) => l !== moved.split("\n")[i]);
assert.deepEqual(changed, ['      "x": 100,']);
// Older files keep x and y on each part.
const old = JSON.stringify({ version: 1, parts: [{ id: "a", type: "api", x: 5, y: 6 }], wires: [{ from: "a", to: "a" }] });
const read = parseBlueprint(old, "old");
assert.deepEqual([read.parts[0].x, read.parts[0].y, read.parts[0].label, read.parts[0].notes], [5, 6, "a", ""]);
assert.deepEqual(read.parts[0].set, {});
assert.deepEqual([read.wires[0].kind, read.wires[0].calls], ["http", 1]);
assert.throws(() => parseBlueprint('{"version":1,"parts":[{"type":"api"}],"wires":[]}', "x"), /Part 1 needs/);
assert.throws(() => parseBlueprint('{"version":1,"parts":[],"wires":[{"to":"a"}]}', "x"), /Wire 1 needs/);

// Readable ids.
assert.equal(readableId("Orders API", []), "orders-api");
assert.equal(readableId("Orders API", ["orders-api", "orders-api-2"]), "orders-api-3");
assert.equal(readableId("???", []), "part");

// The agent is told each part's id, and to record where its code went.
const request = buildPrompt({ ...shop, parts: shop.parts.map((p) => (p.id === "api" ? { ...p, code: "services/api" } : p)) }, "shop.blueprint", "", simulate(shop));
assert.ok(request.includes("(id `api`;") && request.includes("Its code so far: `services/api`") && request.includes('"code" field'));

// A wire naming a part that is not there (a typo in the text) is a bug.
const typo = parseBlueprint(JSON.stringify({ version: 1, parts: [{ id: "u", type: "users" }, { id: "api", type: "api" }], wires: [{ from: "u", to: "apii", kind: "http" }] }), "t");
assert.ok(simulate(typo).findings.some((f) => f.level === "error" && f.text.includes('("apii")') && f.parts.join() === "u"));

// The palette's common parts: most used first, the rest in their usual order.
const kinds = ["a", "b", "c", "d"].map((id) => ({ id }));
assert.deepEqual(mostUsed(kinds, { c: 3, d: 1 }, 3).map((t) => t.id), ["c", "d", "a"]);
assert.deepEqual(mostUsed(kinds, {}, 2).map((t) => t.id), ["a", "b"]);
assert.ok(matches({ name: "Vector database", category: "AI" }, " ai "));
assert.ok(!matches({ name: "Cache", description: "Redis" }, "postgres"));

console.log("blueprint model: ok");
