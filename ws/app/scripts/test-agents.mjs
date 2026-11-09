// Checks the agent view's worker statuses: `node scripts/test-agents.mjs`.
import assert from "node:assert/strict";

const { latestWorkers, teamChat } = await import("../src/agents.ts");

const msg = (role, extra = {}) => ({ role, tool_calls: [], tool_results: [], ...extra });
const dispatch = (id, agent) => ({ call_id: id, name: "dispatch_agent", args: JSON.stringify({ agent, prompt: `job ${id}` }) });
const thread = (messages) => ({ model_id: "m1", messages, helpers: [] });

// An older turn's worker is left out; this turn's two are in, in order.
const t = thread([
  msg("user"),
  msg("assistant", { tool_calls: [dispatch("old", "worker")] }),
  msg("tool", { tool_results: [{ call_id: "old", is_error: false }] }),
  msg("user"),
  msg("assistant", { tool_calls: [dispatch("a", "worker"), dispatch("b", "reviewer")] }),
]);
const logs = new Map([
  ["a", { dispatch: "a", agent: "worker", entries: [{ kind: "call", call_id: "a.1", name: "write_file", args: "{}" }] }],
  ["b", { dispatch: "b", agent: "reviewer", entries: [
    { kind: "switch", from: "m1", to: "m2", error: "overloaded" },
    { kind: "retry", error: "connection reset", wait_secs: 5 },
  ] }],
]);
const approvals = [{ callId: "a.1", name: "write_file", args: "{}" }];

let w = latestWorkers(t, logs, {}, approvals, true);
assert.deepEqual(w.map((x) => x.dispatch), ["a", "b"]);
assert.equal(w[0].status, "needs-you");
assert.equal(w[0].waitingOn.callId, "a.1");
assert.equal(w[0].brief, "job a");
assert.equal(w[1].status, "retrying");
assert.equal(w[1].model, "m2", "the model it switched to");
assert.equal(w[0].model, "m1");

// Results end them, live or saved.
w = latestWorkers(t, logs, { a: false, b: true }, approvals, true);
assert.deepEqual(w.map((x) => x.status), ["done", "failed"]);

// A turn that ended without a result (stopped) is not still "working".
w = latestWorkers(t, logs, {}, [], false);
assert.deepEqual(w.map((x) => x.status), ["stopped", "stopped"]);

// A worker streaming before its dispatch is saved still shows.
const live = new Map([["c", { dispatch: "c", agent: "worker", entries: [] }]]);
w = latestWorkers(thread([msg("user")]), live, {}, [], true);
assert.deepEqual(w.map((x) => [x.dispatch, x.status]), [["c", "working"]]);

// A worker in its own copy carries where it is, for Merge and Discard.
const copied = new Map([["a", { dispatch: "a", agent: "worker", entries: [
  { kind: "isolated", root: "C:/p", folder: "C:/p/.git/ws-worktrees/a", branch: "ws/a", base: "abc1234" },
] }]]);
w = latestWorkers(t, copied, { a: false }, [], false);
assert.deepEqual(w[0].copy, { root: "C:/p", branch: "ws/a", base: "abc1234", folder: "C:/p/.git/ws-worktrees/a" });
assert.equal(w[1].copy, null);

// A worker on the worker model shows that model, and a later switch wins.
const onOwn = new Map([
  ["a", { dispatch: "a", agent: "worker", entries: [{ kind: "model", name: "groq/llama" }] }],
  ["b", { dispatch: "b", agent: "reviewer", entries: [
    { kind: "model", name: "groq/llama" },
    { kind: "switch", from: "groq/llama", to: "m2", error: "down" },
  ] }],
]);
w = latestWorkers(t, onOwn, {}, [], true);
assert.deepEqual(w.map((x) => x.model), ["groq/llama", "m2"]);

// The team chat: this turn's lines only, saved then live, named and numbered.
const two = latestWorkers(
  thread([msg("user"), msg("assistant", { tool_calls: [dispatch("a", "worker"), dispatch("b", "worker")] })]),
  new Map(), {}, [], true,
);
const line = (id, what, from, d, to, text) => ({ id, at: "2026-11-10T10:00:00Z", what, from, dispatch: d, to, text });
const saved = [
  line("0", "report", "worker", "old", "lead", "an older turn's"),
  line("1", "brief", "lead", "a", "worker", "do a"),
  line("2", "note", "worker", "a", null, "I own units.py"),
];
const streamed = [
  line("2", "note", "worker", "a", null, "I own units.py"), // saved already: shown once
  line("3", "note", "worker", "b", "worker (a)", "ok, cli.py is mine"),
  line("4", "report", "worker", "b", "lead", "done"),
];
const chat = teamChat(saved, streamed, two);
assert.deepEqual(
  chat.map((m) => [m.id, m.from, m.to]),
  [
    ["1", "Lead", "worker 1"],
    ["2", "worker 1", "everyone"],
    ["3", "worker 2", "worker 1"],
    ["4", "worker 2", "Lead"],
  ],
);

console.log("agent view: ok");
