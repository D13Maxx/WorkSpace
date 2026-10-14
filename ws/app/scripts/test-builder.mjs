// Checks the parts builder: `node scripts/test-builder.mjs`.
import assert from "node:assert/strict";
import { buildConnectorFree, buildPartFree, parseAiConnector, parseAiPart, partPrompt } from "../src/blueprint/builder.ts";
import { CONNECTORS, buildPrompt, emptyBlueprint, simulate } from "../src/blueprint/model.ts";

const setting = (t, key) => t.settings?.find((s) => s.key === key);

// ---- free builder: parts
let t = buildPartFree("A Postgres database storing orders, 2 replicas");
assert.equal(t.name, "PostgreSQL database");
assert.equal(t.category, "Data");
assert.equal(t.stateful, true);
assert.equal(t.replicas, 2);
assert.deepEqual(t.ports.map((p) => [p.name, p.dir, p.kind]), [["queries", "in", "sql"]]);
assert.deepEqual(t.accepts, ["sql"]);
assert.equal(setting(t, "engine").default, "PostgreSQL");

t = buildPartFree("Redis cache for sessions, 1 ms per lookup");
assert.equal(t.name, "Redis cache");
assert.equal(t.latency, 1);
assert.deepEqual(t.accepts, ["kv"]);

// A rate on a service is what it handles, even when users are mentioned.
t = buildPartFree("An API service in Python called Orders API that handles 300 requests/s for customers");
assert.equal(t.name, "Orders API");
assert.equal(t.capacity, 300);
assert.equal(t.source, 0);
assert.equal(setting(t, "language").default, "Python");
assert.deepEqual(t.ports.map((p) => p.dir), ["in", "out"]);

// A rate on users is the traffic they start; nothing calls them.
t = buildPartFree("Customers browsing the shop at 40 requests per second");
assert.equal(t.source, 40);
assert.deepEqual(t.accepts, []);
assert.ok(t.ports.every((p) => p.dir === "out"));

t = buildPartFree("An LLM using gpt-4o that takes 2 seconds per answer");
assert.equal(setting(t, "model").default, "gpt-4o");
assert.equal(t.latency, 2000);
assert.deepEqual(t.accepts, ["llm"]);
assert.equal(buildPartFree("An LLM called Support Brain using gpt-4o that takes 2 seconds per answer").name, "Support Brain");

t = buildPartFree("Kafka queue for order events, 3k messages/s");
assert.equal(setting(t, "broker").default, "Kafka");
assert.equal(t.capacity, 3000);

t = buildPartFree("An AI agent that answers support tickets with gpt-4o and a search tool");
assert.equal(t.category, "AI");
assert.ok(t.name.toLowerCase().includes("agent"));

// Anything else is a service.
t = buildPartFree("Something that resizes images");
assert.equal(t.category, "Backend");
assert.ok(t.ports.length > 0 && t.description === "Something that resizes images");

// ---- free builder: connectors
let c = buildConnectorFree("gRPC between internal services, 3ms");
assert.deepEqual([c.name, c.sync, c.latency], ["gRPC", true, 3]);
c = buildConnectorFree("Webhook events sent to partner systems");
assert.deepEqual([c.name, c.sync], ["Webhook", false]);
c = buildConnectorFree("An async notification nobody waits for");
assert.equal(c.sync, false);

// ---- AI builder: whatever the model says is checked
const reply = `Sure! Here it is:
{"id": "users", "name": "X", "capacity": -5, "errorRate": 7, "replicas": 0, "color": "red",
 "ports": [{"name": "a b", "dir": "sideways", "kind": "bogus"}, {"name": "a b", "dir": "out", "kind": "llm"}],
 "settings": [{"label": "Mode", "type": "choice", "options": ["x", "y"], "default": "z"}, {"label": "Mode"}, {"type": "choice"}]}
Hope that helps.`;
t = parseAiPart(reply, "the description", CONNECTORS);
assert.notEqual(t.id, "users"); // a reply cannot overwrite a built-in or an inventory part
assert.ok(t.id.startsWith("custom-"));
assert.equal(t.capacity, 100);
assert.equal(t.errorRate, 1);
assert.equal(t.replicas, 1);
assert.equal(t.color, "#d9822b");
assert.equal(t.description, "the description");
assert.deepEqual(t.ports.map((p) => [p.id, p.dir, p.kind]), [["a_b", "in", "*"], ["a_b_2", "out", "llm"]]);
assert.deepEqual(t.accepts, ["*"]);
assert.equal(setting(t, "mode").default, "x");
assert.equal(new Set(t.settings.map((s) => s.key)).size, t.settings.length);
assert.equal(t.settings[2].type, "text"); // a choice with no options is plain text
// Its contract is kept, cleaned like the rest.
t = parseAiPart('{"name": "Orders", "contract": {"operations": [{"name": "createOrder", "takes": "items"}, {"takes": "x"}], "rules": "not a list"}}', "d", CONNECTORS);
assert.deepEqual(t.contract, { operations: [{ name: "createOrder", takes: "items", returns: "" }], data: [], rules: [] });
assert.equal(parseAiPart('{"name": "Plain"}', "d", CONNECTORS).contract, undefined);
assert.throws(() => parseAiPart("I can't help with that.", "d", CONNECTORS), /did not answer/);
c = parseAiConnector('{"name": "gRPC", "sync": false, "latency": "fast"}', "d");
assert.deepEqual([c.name, c.sync, c.latency], ["gRPC", false, 5]);
assert.ok(partPrompt("part", CONNECTORS).includes('"sql"'));

// ---- a built part in a blueprint: its ports are checked
const llm = buildPartFree("An LLM using gpt-4o");
const api = buildPartFree("An API service");
const part = (id, type) => ({ id, type, label: id, x: 0, y: 0, notes: "", set: {} });
const bp = {
  ...emptyBlueprint("ports"),
  customParts: [llm, api],
  parts: [{ ...part("u", "users"), set: { source: 1 } }, part("a", api.id), part("l", llm.id)],
  wires: [
    { id: "w1", from: "u", to: "a", kind: "http", calls: 1, notes: "", toPort: "requests" },
    { id: "w2", from: "a", to: "l", kind: "http", calls: 1, notes: "", fromPort: "calls", toPort: "prompt" },
  ],
};
let findings = simulate(bp).findings.filter((f) => f.level === "error").map((f) => f.text);
assert.deepEqual(findings, [`a talks to l's "prompt" over HTTP / REST, but that port takes LLM request.`]);
bp.wires[1].kind = "llm";
assert.equal(simulate(bp).findings.filter((f) => f.level === "error").length, 0);
bp.wires[1].toPort = "gone";
assert.ok(simulate(bp).findings.some((f) => f.text.includes("port l no longer has")));

// The agent hears about ports, settings and which ports each wire joins.
bp.wires[1].toPort = "prompt";
bp.parts[2].values = { temperature: "0.7" };
const request = buildPrompt(bp, "p.blueprint", "", simulate(bp));
assert.ok(request.includes("Ports: in prompt (LLM request)"), request);
assert.ok(request.includes("Model = gpt-4o; Temperature = 0.7"), request);
assert.ok(request.includes("a.calls → l.prompt: LLM request"), request);

console.log("parts builder: ok");
