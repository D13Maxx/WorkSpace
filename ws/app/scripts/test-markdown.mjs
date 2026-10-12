// Checks the Markdown preview's sanitizer: `node scripts/test-markdown.mjs`.
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";

// DOMPurify needs a DOM; it reads `window` when it loads, so set it first.
globalThis.window = new JSDOM("").window;
const { render } = await import("../src/components/markdown.ts");

const attacks = {
  "<script": "<script>alert(1)</script>",
  onerror: '<img src="x" onerror="alert(2)">',
  onclick: '<a href="#" onclick="alert(3)">c</a>',
  "javascript:": "[bad](javascript:alert(4)) <a href=\"javascript:alert(5)\">x</a>",
  "style=": '<div style="position:fixed;inset:0;background:red">overlay</div>',
  "<style": "<style>body { display: none }</style>",
  "<iframe": '<iframe src="https://example.com"></iframe>',
  "<form": "<form><input name=a><button>go</button></form>",
  "<object": '<object data="x.swf"></object><embed src="x">',
  onload: '<svg onload="alert(6)"></svg>',
};
for (const [bad, input] of Object.entries(attacks)) {
  const out = render(input).toLowerCase();
  assert.ok(!out.includes(bad), `${bad} survived: ${out}`);
}

// What Markdown is for still comes through.
const ok = render("# Title\n\n**bold** [link](https://example.com)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```js\nx()\n```");
for (const good of ["<h1>Title</h1>", "<strong>bold</strong>", 'href="https://example.com"', "<table>", "<code"]) {
  assert.ok(ok.includes(good), `${good} missing: ${ok}`);
}

console.log("markdown sanitizer: ok");
