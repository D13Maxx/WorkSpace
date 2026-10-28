// A Markdown file, rendered beside its editor (sanitized: see markdown.ts).

import { useDeferredValue, useMemo } from "react";
import { render } from "./markdown";

export function MarkdownPreview({ text }: { text: string }) {
  // Typing stays smooth in a long file: the preview catches up when idle.
  const deferred = useDeferredValue(text);
  const html = useMemo(() => render(deferred), [deferred]);
  return (
    <div
      className="md-preview"
      // A followed link would navigate Workspace's own window away.
      onClick={(e) => {
        if ((e.target as Element).closest("a")) e.preventDefault();
      }}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
