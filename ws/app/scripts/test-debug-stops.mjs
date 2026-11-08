// Checks which debugger stops are shown: `node scripts/test-debug-stops.mjs`.
import assert from "node:assert/strict";

const { repeatStop } = await import("../src/debugStops.ts");

const at = (spot, resumedBy, where = "main.rs:5") => ({
  resumedBy,
  reason: "breakpoint",
  where,
  description: `breakpoint ${spot}`,
});

// What lldb did in the app on a println! line: 1.1, then 1.2 and 1.3 after Continue.
let r = repeatStop(null, at("1.1", null));
assert.equal(r.skip, false, "the first stop shows");
r = repeatStop(r.last, at("1.2", "continue"));
assert.equal(r.skip, true, "another piece of the same line is skipped");
r = repeatStop(r.last, at("1.3", "continue"));
assert.equal(r.skip, true, "and the next");

// A loop: back at 1.1 is a new pass, so it stops.
r = repeatStop(r.last, at("1.1", "continue"));
assert.equal(r.skip, false, "a loop's next pass stops");
r = repeatStop(r.last, at("1.2", "continue"));
assert.equal(r.skip, true, "and skips that pass's extra pieces again");

// Stepping never skips.
r = repeatStop(repeatStop(null, at("1.1", null)).last, at("1.2", "step"));
assert.equal(r.skip, false, "a step shows wherever it lands");

// Another line, or another breakpoint, is a real stop.
r = repeatStop(repeatStop(null, at("1.1", null)).last, at("1.2", "continue", "main.rs:9"));
assert.equal(r.skip, false, "another line");
r = repeatStop(repeatStop(null, at("1.1", null)).last, at("2.1", "continue"));
assert.equal(r.skip, false, "another breakpoint on the same line");

// Adapters that do not name pieces (gdb, debugpy) are never skipped.
r = repeatStop(null, { resumedBy: null, reason: "breakpoint", where: "a.py:3", description: "Paused on breakpoint" });
assert.equal(r.last, null);
r = repeatStop(r.last, { resumedBy: "continue", reason: "breakpoint", where: "a.py:3", description: "Paused on breakpoint" });
assert.equal(r.skip, false);

console.log("debugger stops: ok");
