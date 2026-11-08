// Checks the agent view's worker statuses: `node scripts/test-agents.mjs`.
import assert from "node:assert/strict";

const { latestWorkers } = await import("../src/agents.ts");

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

console.log("agent view: ok");
