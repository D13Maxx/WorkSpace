// Fuzzy matching for the finder: the query's letters must appear in order;
// matches at word starts, in the file name and in one run score higher.

const SEPARATORS = "/\\_-. :";

/** How well `query` matches `text`: higher is better, null for no match. */
export function score(query: string, text: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const name = Math.max(t.lastIndexOf("/"), t.lastIndexOf("\\")) + 1;
  let s = 0;
  let from = 0;
  let prev = -2;
  for (const ch of q) {
    const i = t.indexOf(ch, from);
    if (i < 0) return null;
    s += 1;
    if (i === prev + 1) s += 5;
    const before = text[i - 1] ?? "";
    const camel = /[a-z]/.test(before) && /[A-Z]/.test(text[i]);
    if (i === 0 || SEPARATORS.includes(before) || camel) s += 8;
    if (i >= name) s += 2;
    prev = i;
    from = i + 1;
  }
  if (t.slice(name).startsWith(q)) s += 30;
  else if (t.includes(q)) s += 15;
  // Shorter wins a tie.
  return s - t.length / 100;
}

/** The best `limit` items for `query`, best first; all of them, in order, when it is empty. */
export function best<T>(items: T[], query: string, text: (item: T) => string, limit = 50): T[] {
  if (!query) return items.slice(0, limit);
  return items
    .map((item) => ({ item, s: score(query, text(item)) }))
    .filter((r): r is { item: T; s: number } => r.s !== null)
    .sort((a, b) => b.s - a.s)
    .slice(0, limit)
    .map((r) => r.item);
}
