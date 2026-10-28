// Markdown to safe HTML for the preview. The file is untrusted input and the
// page showing it can call Workspace's backend, so the HTML is sanitized: no
// scripts, event handlers, frames, forms or inline styles survive.
// Tested by scripts/test-markdown.mjs.

import { marked } from "marked";
import DOMPurify from "dompurify";

export function render(text: string): string {
  const html = marked.parse(text, { async: false, gfm: true });
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "object", "embed"],
    // A style could lay the preview over the rest of the app.
    FORBID_ATTR: ["style"],
  });
}
