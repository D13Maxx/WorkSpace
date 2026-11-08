// Checks language-server edits applied to files on disk: `node scripts/test-lsp-edits.mjs`.
import assert from "node:assert/strict";

const { applyTextEdits, editsByUri, uriPath, relativeTo } = await import("../src/lspEdits.ts");

const edit = (sl, sc, el, ec, newText) => ({ range: { start: { line: sl, character: sc }, end: { line: el, character: ec } }, newText });

// A rename touching two lines, given out of order, in a CRLF file.
const crlf = "fn total() {}\r\nfn main() { total(); }\r\n";
assert.equal(
  applyTextEdits(crlf, [edit(1, 12, 1, 17, "sum"), edit(0, 3, 0, 8, "sum")]),
  "fn sum() {}\r\nfn main() { sum(); }\r\n",
  "edits from the end first, CRLF kept",
);

// Two edits on one line; positions refer to the text before either.
assert.equal(applyTextEdits("a + b\n", [edit(0, 0, 0, 1, "x"), edit(0, 4, 0, 5, "yy")]), "x + yy\n");

// Inserting new lines, and a position past a line's end stops before the break.
assert.equal(applyTextEdits("use a;\nfn f() {}\n", [edit(0, 99, 0, 99, "\nuse b;")]), "use a;\nuse b;\nfn f() {}\n");

// UTF-16: an emoji counts as two, as the protocol says.
assert.equal(applyTextEdits("let s = \"😀\"; x\n", [edit(0, 14, 0, 15, "y")]), "let s = \"😀\"; y\n");

// Both shapes of workspace edit, merged per file; file operations are reported, not done.
const { byUri, skipped } = editsByUri({
  changes: { "file:///a.rs": [edit(0, 0, 0, 1, "x")] },
  documentChanges: [
    { textDocument: { uri: "file:///b.rs", version: 3 }, edits: [edit(0, 0, 0, 1, "y")] },
    { kind: "rename", oldUri: "file:///c.rs", newUri: "file:///d.rs" },
  ],
});
assert.deepEqual(Object.keys(byUri).sort(), ["file:///a.rs", "file:///b.rs"]);
assert.deepEqual(skipped, ["rename"]);

// URIs as rust-analyzer sends them, and paths outside the project refused.
assert.equal(uriPath("file:///c%3A/Users/me/proj/src/main.rs"), "c:/Users/me/proj/src/main.rs");
assert.equal(relativeTo("C:\\Users\\me\\proj", "c:/Users/me/proj/src/main.rs"), "src/main.rs");
assert.equal(relativeTo("C:\\Users\\me\\proj", "c:/Users/me/proj2/x.rs"), null, "a sibling folder is outside");
assert.equal(relativeTo("C:\\Users\\me\\proj", "C:/Users/other/x.rs"), null);

console.log("language-server edits: ok");
