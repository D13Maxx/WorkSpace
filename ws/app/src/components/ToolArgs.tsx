/** Tool arguments as readable fields: code in blocks with real line breaks. */
export function ToolArgs({ args }: { args: string }) {
  let fields: [string, unknown][];
  try {
    const parsed: unknown = JSON.parse(args);
    if (typeof parsed !== "object" || parsed === null) return <pre>{args}</pre>;
    fields = Object.entries(parsed);
  } catch {
    return <pre>{args}</pre>;
  }
  return (
    <dl className="tool-args">
      {fields.map(([key, value]) => {
        const text = typeof value === "string" ? value : JSON.stringify(value, null, 2);
        const block = text.includes("\n") || text.length > 80;
        return (
          <div key={key}>
            <dt>{key}</dt>
            <dd>{block ? <pre>{text}</pre> : <code>{text}</code>}</dd>
          </div>
        );
      })}
    </dl>
  );
}
