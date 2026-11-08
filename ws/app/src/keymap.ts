// The app's own shortcuts, as a table from keys to command ids. The user's
// keymap.json (in the app data folder) adds to it or, with null, unbinds.
// Keys inside the editor (Vim, F2, F12) are the editor's, not this table's.

/** What every install starts with. */
export const DEFAULT_KEYMAP: Record<string, string> = {
  "Ctrl+P": "find.files",
  "Ctrl+Shift+P": "find.commands",
  "Ctrl+T": "find.symbols",
  "Ctrl+Shift+F": "find.text",
  "Ctrl+`": "terminal.toggle",
  "Ctrl+Shift+`": "terminal.new",
  "Ctrl+\\": "split.right",
};

const MODIFIERS = ["Ctrl", "Alt", "Shift", "Meta"] as const;

/** Key names by physical key, so Shift+` stays ` and not ~. */
const CODES: Record<string, string> = {
  Backquote: "`",
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  Minus: "-",
  Equal: "=",
  Space: "Space",
};

/** "ctrl + shift + p" → "Ctrl+Shift+P"; null if it is not a key combination. */
export function normalize(combo: string): string | null {
  const parts = combo.split("+").map((p) => p.trim());
  // "Ctrl++" means the + key.
  if (combo.trim().endsWith("++")) parts.splice(-2, 2, "+");
  const mods = new Set<string>();
  let key: string | null = null;
  for (const part of parts) {
    const mod = MODIFIERS.find((m) => m.toLowerCase() === part.toLowerCase() || (m === "Ctrl" && part.toLowerCase() === "control"));
    if (mod) mods.add(mod);
    else if (key === null && part) key = part.length === 1 ? part.toUpperCase() : part[0].toUpperCase() + part.slice(1);
    else return null;
  }
  if (!key) return null;
  return [...MODIFIERS.filter((m) => mods.has(m)), key].join("+");
}

/** The combination a key press makes, in the same form as `normalize`. */
export function comboOf(e: Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey" | "key" | "code">): string {
  const key = e.code.startsWith("Key")
    ? e.code.slice(3)
    : e.code.startsWith("Digit")
      ? e.code.slice(5)
      : (CODES[e.code] ?? (e.key.length === 1 ? e.key.toUpperCase() : e.key));
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Meta"].filter(Boolean);
  return [...mods, key].join("+");
}

/** The defaults with the user's file over them, and what in it made no sense. */
export function parseKeymap(text: string, known: Set<string>): { keys: Record<string, string>; problems: string[] } {
  const keys: Record<string, string> = {};
  for (const [combo, id] of Object.entries(DEFAULT_KEYMAP)) keys[normalize(combo)!] = id;
  const problems: string[] = [];
  let user: unknown;
  try {
    user = text.trim() ? JSON.parse(text) : {};
  } catch (e) {
    return { keys, problems: [`keymap.json is not valid JSON (${(e as Error).message}); using the defaults`] };
  }
  if (typeof user !== "object" || user === null || Array.isArray(user)) {
    return { keys, problems: ["keymap.json should be one object of key to command; using the defaults"] };
  }
  for (const [combo, id] of Object.entries(user)) {
    // Notes for the reader, such as the list of commands.
    if (combo.startsWith("_")) continue;
    const key = normalize(combo);
    if (!key) problems.push(`"${combo}" is not a key combination`);
    else if (id === null) delete keys[key];
    else if (typeof id !== "string" || !known.has(id)) problems.push(`"${combo}": no command called ${JSON.stringify(id)}`);
    else keys[key] = id;
  }
  return { keys, problems };
}

/** The keys bound to `id`, for showing next to it. */
export function keysFor(keys: Record<string, string>, id: string): string[] {
  return Object.entries(keys)
    .filter(([, v]) => v === id)
    .map(([k]) => k);
}
