// Checks the finder's fuzzy matching: `node scripts/test-fuzzy.mjs`.
import assert from "node:assert/strict";

const { score, best } = await import("../src/fuzzy.ts");

const files = [
  "src/components/Approvals.tsx",
  "src/App.tsx",
  "src/components/FileViewer.tsx",
  "src/lsp.ts",
  "src/longs/path.ts",
  "src-tauri/src/commands.rs",
  "README.md",
];
const top = (q) => best(files, q, (f) => f)[0];

assert.equal(score("xyz", "src/App.tsx"), null, "letters missing");
assert.equal(score("ppa", "src/App.tsx"), null, "letters out of order");
assert.equal(top("app"), "src/App.tsx", "the shorter of two file names starting with it");
assert.equal(top("fv"), "src/components/FileViewer.tsx", "word starts, camel case too");
assert.equal(top("lsp"), "src/lsp.ts", "a run in the file name beats letters spread over a path");
assert.equal(top("cmd"), "src-tauri/src/commands.rs");
assert.deepEqual(best(files, "", (f) => f, 2), files.slice(0, 2), "no query: the first ones, in order");
assert.equal(best(files, "s", (f) => f, 3).length, 3, "cut at the limit");

console.log("fuzzy finder: ok");
