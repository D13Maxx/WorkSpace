// Checks the keymap: `node scripts/test-keymap.mjs`.
import assert from "node:assert/strict";

const { normalize, comboOf, parseKeymap, keysFor, DEFAULT_KEYMAP } = await import("../src/keymap.ts");

assert.equal(normalize("ctrl + shift + p"), "Ctrl+Shift+P");
assert.equal(normalize("Shift+Control+p"), "Ctrl+Shift+P", "modifiers in one order");
assert.equal(normalize("Ctrl+F5"), "Ctrl+F5");
assert.equal(normalize("Ctrl+`"), "Ctrl+`");
assert.equal(normalize("Ctrl++"), "Ctrl++", "the + key itself");
assert.equal(normalize("Ctrl+P+Q"), null, "two keys");
assert.equal(normalize("Ctrl+"), null, "no key");

const press = (code, key, mods = {}) => comboOf({ ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, code, key, ...mods });
assert.equal(press("KeyP", "p", { ctrlKey: true }), "Ctrl+P");
assert.equal(press("Backquote", "~", { ctrlKey: true, shiftKey: true }), "Ctrl+Shift+`", "by key, so Shift does not turn it into ~");
assert.equal(press("Digit1", "!", { altKey: true, shiftKey: true }), "Alt+Shift+1");
assert.equal(press("F5", "F5"), "F5");
assert.equal(press("Backslash", "\\", { ctrlKey: true }), "Ctrl+\\");

const known = new Set([...Object.values(DEFAULT_KEYMAP), "chat.new"]);
const { keys, problems } = parseKeymap(
  JSON.stringify({ "ctrl+shift+n": "chat.new", "Ctrl+T": null, "Ctrl+Q": "no.such", "Ctrl+A+B": "chat.new" }),
  known,
);
assert.equal(keys["Ctrl+Shift+N"], "chat.new", "added");
assert.equal(keys["Ctrl+T"], undefined, "null unbinds a default");
assert.equal(keys["Ctrl+P"], "find.files", "other defaults stay");
assert.equal(problems.length, 2, problems.join("; "));
assert.match(problems.join("; "), /no command called "no.such"/);

const broken = parseKeymap("{ not json", known);
assert.equal(broken.keys["Ctrl+P"], "find.files", "a broken file falls back to the defaults");
assert.match(broken.problems[0], /not valid JSON/);
assert.deepEqual(parseKeymap("", known).problems, [], "an empty file is fine");
assert.deepEqual(parseKeymap('{"_help": "notes", "_commands": ["a"]}', known).problems, [], "_ keys are notes");
assert.deepEqual(keysFor(keys, "find.files"), ["Ctrl+P"]);

console.log("keymap: ok");
