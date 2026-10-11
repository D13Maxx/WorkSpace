// Checks the blueprint simulator: `node scripts/test-blueprint.mjs`.
import assert from "node:assert/strict";
import { buildPrompt, emptyBlueprint, simulate } from "../src/blueprint/model.ts";

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

console.log("blueprint model: ok");
