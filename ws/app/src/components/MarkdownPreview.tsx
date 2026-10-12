// A Markdown file, rendered. The file is untrusted input and this page can
// call FRIDAY's backend, so the HTML is sanitized before it is shown: no
// scripts, event handlers, frames, forms or inline styles survive.

import { useDeferredValue, useMemo } from "react";
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

export function MarkdownPreview({ text }: { text: string }) {
  // Typing stays smooth in a long file: the preview catches up when idle.
  const deferred = useDeferredValue(text);
  const html = useMemo(() => render(deferred), [deferred]);
  return (
    <div
      className="md-preview"
      // A followed link would navigate FRIDAY's own window away.
      onClick={(e) => {
        if ((e.target as Element).closest("a")) e.preventDefault();
      }}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
