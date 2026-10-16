// A blueprint file (.blueprint): software drawn as parts wired with
// connectors, run in a simulation, then handed to the agent to build. Each
// opens in its own window, with the inventory of parts and connectors that
// every blueprint shares.
//
// The file is plain JSON (see blueprint/model.ts), so the agent can read and
// change it too; the view reloads when it does.

import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  ReactFlowProvider,
  applyNodeChanges,
  useReactFlow,
  useUpdateNodeInternals,
  type Connection,
  type Edge,
  type EdgeChange,
  type Node,
  type NodeChange,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, ask, claudeFirst, preferredModel, type ModelOption, type SandboxReport } from "../api";
import {
  buildConnectorFree,
  buildPartFree,
  normaliseConnector,
  normalisePart,
  parseAiConnector,
  parseAiPart,
  partPrompt,
} from "../blueprint/builder";
import {
  CONNECTORS,
  PARTS,
  accepts,
  buildPrompt,
  byCategory,
  calibrate,
  contractOf,
  emptyContract,
  fieldsOf,
  categoryOf,
  connectorTypes,
  blueprintText,
  emptyLibrary,
  forSave,
  matches,
  mostUsed,
  ms,
  parseBlueprint,
  parseLibrary,
  partTypes,
  readableId,
  portsOf,
  rate,
  simulate,
  typeOf,
  upsert,
  withLibrary,
  type Blueprint,
  type ConnectorType,
  type Contract,
  type Finding,
  type Library,
  type Part,
  type PartStats,
  type PartType,
  type Port,
  type Setting,
  type Tunable,
  type Wire,
} from "../blueprint/model";

/** Where a blueprint file is: `path` inside the folder `root`. */
export interface BlueprintPath {
  root: string;
  path: string;
}

interface Props {
  /** Null for an untitled blueprint, written only when it is first saved. */
  file: BlueprintPath | null;
  /** Where the save dialog starts for an untitled blueprint. */
  dir?: string | null;
  version: number;
  header: (actions: ReactNode) => ReactNode;
  /** Hand the finished blueprint, saved as `file`, to the agent as this request. */
  onBuild: (prompt: string, file: BlueprintPath) => void;
  onDirty?: (dirty: boolean) => void;
  /** Saved as `file`; `first` when an untitled blueprint got its file. */
  onSaved?: (file: BlueprintPath, first: boolean) => void;
  /** Show the file's text in the main window's editor. */
  onSource?: (file: BlueprintPath) => void;
  /** Open a part's code (a path inside `file.root`) in the main window. */
  onOpenCode?: (root: string, path: string) => void;
}

/** A file name from the blueprint's own name. */
const fileName = (name: string) => (name.trim().replace(/[\\/:*?"<>|]+/g, "-") || "Untitled") + ".blueprint";

const uid = () => crypto.randomUUID().slice(0, 8);

/** How often each part and connector was used, so the palette leads with the
 * common ones. Per viewer; the Workshop works the same without it. */
const USES = "workshop-uses";
function readUses(): Record<string, number> {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(USES) ?? "{}");
    return v && typeof v === "object" ? (v as Record<string, number>) : {};
  } catch {
    return {};
  }
}
function countUse(id: string): Record<string, number> {
  const uses = readUses();
  uses[id] = (uses[id] ?? 0) + 1;
  try {
    localStorage.setItem(USES, JSON.stringify(uses));
  } catch {
    // Not remembered, which is fine.
  }
  return uses;
}

/** How far each arrow key scrolls the canvas, in screen pixels. */
const ARROWS: Record<string, [number, number] | undefined> = { ArrowLeft: [80, 0], ArrowRight: [-80, 0], ArrowUp: [0, 80], ArrowDown: [0, -80] };

type PartData = { part: Part; type: PartType; stats?: PartStats; down: boolean; level?: Finding["level"] };
type PartNodeT = Node<PartData, "part">;

function PartNode({ id, data, selected }: NodeProps<PartNodeT>) {
  const { part, type, stats, down, level } = data;
  const ports = portsOf(type);
  const ins = ports.filter((p) => p.dir === "in");
  const outs = ports.filter((p) => p.dir === "out");
  // Named ports get labels; the plain "in" and "out" of a part without ports do not.
  const named = !!type.ports?.length;
  // React Flow measures handles once; when a part type's ports change, measure
  // again, or wires attach to where the old ports were.
  const updateInternals = useUpdateNodeInternals();
  const shape = ports.map((p) => p.id + p.dir).join();
  useEffect(() => updateInternals(id), [id, shape, updateInternals]);
  const at = (i: number, n: number) => `${((i + 1) / (n + 1)) * 100}%`;
  return (
    <div
      className={`bp-part ${selected ? "selected" : ""} ${level ?? ""} ${down ? "down" : ""} ${named ? "ported" : ""}`}
      style={{ "--part": type.color, minHeight: named ? 22 + 18 * Math.max(ins.length, outs.length) : undefined } as CSSProperties}
      title={type.description}
    >
      {ins.map((p, i) => (
        <Handle key={p.id} id={p.id} type="target" position={Position.Left} style={{ top: at(i, ins.length) }} title={p.name} />
      ))}
      {named &&
        ins.map((p, i) => (
          <span key={p.id} className="bp-port in" style={{ top: at(i, ins.length) }}>
            {p.name}
          </span>
        ))}
      {named &&
        outs.map((p, i) => (
          <span key={p.id} className="bp-port out" style={{ top: at(i, outs.length) }}>
            {p.name}
          </span>
        ))}
      <div className="bp-part-type">{type.name}</div>
      <div className="bp-part-label">{part.label}</div>
      {stats && (
        <div className="bp-part-stats">
          {down ? (
            "down (simulated)"
          ) : (
            <>
              <span>
                {rate(stats.load)}/s · {ms(stats.latency)}
              </span>
              {stats.utilization > 0 && (
                <span className="bp-meter" aria-label={`${Math.round(stats.utilization * 100)}% of its limit`}>
                  <span style={{ width: `${Math.min(100, stats.utilization * 100)}%` }} />
                </span>
              )}
            </>
          )}
        </div>
      )}
      {outs.map((p, i) => (
        <Handle key={p.id} id={p.id} type="source" position={Position.Right} style={{ top: at(i, outs.length) }} title={p.name} />
      ))}
    </div>
  );
}
const NODE_TYPES = { part: PartNode };

/** A whole window for one blueprint (main.tsx renders it in blueprint windows). */
export function BlueprintWindow({ root, path, dir }: { root: string | null; path: string | null; dir: string | null }) {
  const [file, setFile] = useState<BlueprintPath | null>(root && path ? { root, path } : null);
  // Bumped when the main window's agent or a run may have changed files.
  const [version, setVersion] = useState(0);
  const dirty = useRef(false);
  useEffect(() => {
    const off = listen("files-changed", () => setVersion((v) => v + 1));
    return () => void off.then((f) => f());
  }, []);
  useEffect(() => {
    const off = getCurrentWindow().onCloseRequested(async (e) => {
      if (dirty.current && !(await ask("This blueprint has unsaved changes. Close it anyway?"))) e.preventDefault();
    });
    return () => void off.then((f) => f());
  }, []);
  const onDirty = useCallback((d: boolean) => {
    dirty.current = d;
    // The main window asks about these before closing Workspace.
    void api.blueprintDirty(d);
  }, []);
  return (
    <div className="bp-window">
      <BlueprintFile
        file={file}
        dir={dir}
        version={version}
        onDirty={onDirty}
        onSaved={(f, first) => {
          if (first) {
            setFile(f);
            void getCurrentWindow().setTitle(`${f.path.replace(/\.blueprint$/i, "")} — Workshop — Workspace`);
            // So opening the file again finds this window.
            void api.blueprintSavedAs(f.root, f.path);
          }
          // The main window shows the file, and rereads its text if that is open.
          void emitTo("main", "blueprint-saved", f);
        }}
        onSource={(f) => void emitTo("main", "blueprint-source", f)}
        onOpenCode={(root, path) => void emitTo("main", "open-code", { root, path })}
        // The chat lives in the main window, which starts the agent.
        onBuild={(prompt, f) => void emitTo("main", "blueprint-build", { ...f, prompt })}
        header={(actions) => (
          <header>
            {file ? (
              <code title={file.root + "\\" + file.path.replace(/\//g, "\\")}>{file.path}</code>
            ) : (
              <code className="muted">Untitled (not saved yet)</code>
            )}
            <span className="actions">{actions}</span>
          </header>
        )}
      />
    </div>
  );
}

export function BlueprintFile(props: Props) {
  return (
    <ReactFlowProvider>
      <Editor {...props} />
    </ReactFlowProvider>
  );
}

type Selection = { kind: "part" | "wire"; id: string } | null;
type TypeEdit = { kind: "part"; value: PartType; isNew: boolean } | { kind: "connector"; value: ConnectorType; isNew: boolean };

function Editor({ file, dir, version, header, onBuild, onDirty, onSaved, onSource, onOpenCode }: Props) {
  const name = file ? (file.path.split("/").pop() ?? "blueprint").replace(/\.blueprint$/i, "") : "Untitled";
  const fileKey = file ? `${file.root}\n${file.path}` : null;
  // Set when this editor just saved an untitled blueprint, so it does not reread it.
  const saved = useRef<string | null>(null);
  // The file's text as this editor last read or wrote it: a change on disk to
  // anything else came from elsewhere (its source edited, the agent).
  const written = useRef<string | null>(null);
  const [bp, setBp] = useState<Blueprint | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [sel, setSel] = useState<Selection>(null);
  // The part or wire whose details are open (right-click), apart from what is selected.
  const [open, setOpen] = useState<Selection>(null);
  // Run: the simulation drawn over the parts, at `scale` times the load, with `down` failed.
  const [running, setRunning] = useState(false);
  const [scale, setScale] = useState(1);
  const [down, setDown] = useState<Set<string>>(new Set());
  // The connector new wires use; null picks one the target takes.
  const [pen, setPen] = useState<string | null>(null);
  const [typeEdit, setTypeEdit] = useState<TypeEdit | null>(null);
  // The sandbox: the built system run and measured.
  const [sandbox, setSandbox] = useState<{ running: boolean; status: string; report?: SandboxReport; error?: string } | null>(null);
  // The full inventory (search, every part and connector) over the canvas.
  const [inventory, setInventory] = useState(false);
  const [uses, setUses] = useState(readUses);
  const used = (id: string) => setUses(countUse(id));
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  useEffect(() => {
    onDirty?.(dirty);
  }, [dirty, onDirty]);
  const flow = useReactFlow();

  // The inventory, shared by every blueprint and window; reread when any window changes it.
  const [lib, setLib] = useState<Library>(emptyLibrary);
  const libRef = useRef(lib);
  libRef.current = lib;
  useEffect(() => {
    const load = () =>
      api
        .readPartsLibrary()
        .then((text) => setLib(parseLibrary(text)))
        .catch((e) => setError(`Could not read the parts inventory: ${e}`));
    void load();
    const off = listen("parts-library-changed", () => void load());
    return () => void off.then((f) => f());
  }, []);
  // Its parts are on hand in this blueprint too (not a change to save).
  useEffect(() => {
    setBp((b) => b && withLibrary(b, lib));
  }, [lib]);
  const storeLib = async (next: Library) => {
    try {
      await api.writePartsLibrary(JSON.stringify(next, null, 2) + "\n");
      setLib(next);
      void emit("parts-library-changed");
    } catch (e) {
      setError(`Could not save the parts inventory: ${e}`);
    }
  };
  const keep = (t: TypeEdit) =>
    void storeLib(t.kind === "part" ? { ...lib, parts: upsert(lib.parts, t.value) } : { ...lib, connectors: upsert(lib.connectors, t.value) });
  /** Out of the inventory; this blueprint keeps a copy only if it uses it. */
  const removeFromLib = (t: TypeEdit) => {
    const id = t.value.id;
    void storeLib(
      t.kind === "part" ? { ...lib, parts: lib.parts.filter((x) => x.id !== id) } : { ...lib, connectors: lib.connectors.filter((x) => x.id !== id) },
    );
    setBp(
      (b) =>
        b && {
          ...b,
          customParts: b.customParts.filter((x) => x.id !== id || b.parts.some((p) => p.type === id)),
          customConnectors: b.customConnectors.filter((x) => x.id !== id || b.wires.some((w) => w.kind === id)),
        },
    );
  };
  const canvas = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    if (fileKey && saved.current === fileKey) {
      saved.current = null;
      return;
    }
    setError(null);
    setDirty(false);
    if (!file) {
      // Untitled: nothing on disk yet.
      setBp(withLibrary(parseBlueprint("", name), libRef.current));
      return;
    }
    api
      .readTextFile(file.root, file.path)
      .then((text) => {
        if (!live) return;
        setBp(withLibrary(parseBlueprint(text, name), libRef.current));
        written.current = text;
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
    // `fileKey` stands for `file`, which is a new object on every render.
  }, [fileKey, name, nonce]);

  // Pick up edits made elsewhere (the agent, the file's source in the main
  // window), never over unsaved work, and in place: the view stays where it is.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current || !file) return;
    seen.current = version;
    if (dirtyRef.current) return;
    let live = true;
    api
      .readTextFile(file.root, file.path)
      .then((text) => {
        if (!live || dirtyRef.current || text === written.current) return;
        // Text typed by hand can be half-finished: say so, keep what was drawn.
        setBp(withLibrary(parseBlueprint(text, name), libRef.current));
        written.current = text;
        setError(null);
      })
      .catch((e) => live && setError(`The file's text could not be read as a blueprint: ${e}`));
    return () => {
      live = false;
    };
    // `fileKey` stands for `file`.
  }, [version, fileKey, name]);

  const change = (f: (b: Blueprint) => Blueprint) => {
    setBp((b) => b && f(b));
    setDirty(true);
  };

  // The check the build waits on is always at the drawn load with nothing failed.
  const base = useMemo(() => bp && simulate(bp), [bp]);
  const live = useMemo(() => (bp && running ? simulate(bp, scale, down) : base), [bp, running, scale, down, base]);
  const problems = base?.findings.filter((f) => f.level === "error").length ?? 0;

  const levels = useMemo(() => {
    const worst: Record<string, Finding["level"]> = {};
    const rank = { error: 0, warning: 1, info: 2 };
    for (const f of running ? (live?.findings ?? []) : []) {
      for (const id of f.parts) if (!worst[id] || rank[f.level] < rank[worst[id]]) worst[id] = f.level;
    }
    return worst;
  }, [live, running]);

  // React Flow keeps each node's measured size on the node, so they live in
  // state and are rebuilt from the blueprint without losing it.
  const [nodes, setNodes] = useState<PartNodeT[]>([]);
  useEffect(() => {
    if (!bp) return;
    setNodes((prev) => {
      const old = new Map(prev.map((n) => [n.id, n]));
      return bp.parts.map((p) => ({
        ...old.get(p.id),
        id: p.id,
        type: "part" as const,
        position: { x: p.x, y: p.y },
        selected: sel?.kind === "part" && sel.id === p.id,
        data: {
          part: p,
          type: typeOf(bp, p),
          stats: running ? live?.parts[p.id] : undefined,
          down: running && down.has(p.id),
          level: levels[p.id] === "info" ? undefined : levels[p.id],
        },
      }));
    });
  }, [bp, live, running, sel, down, levels]);

  const kinds = useMemo(() => new Map(bp ? connectorTypes(bp).map((c) => [c.id, c]) : []), [bp]);
  // A port id the part still has; otherwise none, so React Flow draws the wire to
  // the first port (and it can be seen and deleted) while Run reports it.
  const handle = (partId: string, port: string | undefined) => {
    const p = bp?.parts.find((x) => x.id === partId);
    return p && port && portsOf(typeOf(bp!, p)).some((x) => x.id === port) ? port : null;
  };
  const edges: Edge[] = (bp?.wires ?? []).map((w) => {
    const c = kinds.get(w.kind);
    const load = live?.wires[w.id] ?? 0;
    const color = c?.color ?? "#888";
    return {
      id: w.id,
      source: w.from,
      target: w.to,
      sourceHandle: handle(w.from, w.fromPort),
      targetHandle: handle(w.to, w.toPort),
      selected: sel?.kind === "wire" && sel.id === w.id,
      animated: running && load > 0,
      label: running ? `${rate(load)}/s` : (c?.name ?? w.kind) + (w.calls !== 1 ? ` ×${w.calls}` : ""),
      labelStyle: { fill: "var(--text)", fontSize: 11 },
      labelBgStyle: { fill: "var(--panel)" },
      style: {
        stroke: color,
        strokeWidth: running ? 1.5 + Math.min(5, Math.log10(1 + load) * 1.5) : 2,
        strokeDasharray: c && !c.sync ? "6 4" : undefined,
      },
      markerEnd: { type: MarkerType.ArrowClosed, color },
    };
  });

  const removeParts = (ids: string[]) =>
    change((b) => ({
      ...b,
      parts: b.parts.filter((p) => !ids.includes(p.id)),
      wires: b.wires.filter((w) => !ids.includes(w.from) && !ids.includes(w.to)),
    }));
  const removeWires = (ids: string[]) => change((b) => ({ ...b, wires: b.wires.filter((w) => !ids.includes(w.id)) }));

  const onNodesChange = (changes: NodeChange<PartNodeT>[]) => {
    setNodes((ns) => applyNodeChanges(changes, ns));
    const moved = new Map<string, { x: number; y: number }>();
    const removed: string[] = [];
    for (const c of changes) {
      if (c.type === "position" && c.position) moved.set(c.id, c.position);
      if (c.type === "remove") removed.push(c.id);
      if (c.type === "select") {
        if (c.selected) setSel({ kind: "part", id: c.id });
        else setSel((s) => (s?.kind === "part" && s.id === c.id ? null : s));
      }
    }
    if (moved.size) {
      change((b) => ({
        ...b,
        parts: b.parts.map((p) => {
          const at = moved.get(p.id);
          return at ? { ...p, x: Math.round(at.x), y: Math.round(at.y) } : p;
        }),
      }));
    }
    if (removed.length) removeParts(removed);
  };

  const onEdgesChange = (changes: EdgeChange[]) => {
    const removed: string[] = [];
    for (const c of changes) {
      if (c.type === "remove") removed.push(c.id);
      if (c.type === "select") {
        if (c.selected) setSel({ kind: "wire", id: c.id });
        else setSel((s) => (s?.kind === "wire" && s.id === c.id ? null : s));
      }
    }
    if (removed.length) removeWires(removed);
  };

  const onConnect = (c: Connection) => {
    if (!bp || c.source === c.target) return;
    const target = bp.parts.find((p) => p.id === c.target);
    const source = bp.parts.find((p) => p.id === c.source);
    const toType = target && typeOf(bp, target);
    // The connector the ports name, else one the target takes.
    const inKind = toType?.ports?.find((p) => p.id === c.targetHandle)?.kind;
    const outKind = source && typeOf(bp, source).ports?.find((p) => p.id === c.sourceHandle)?.kind;
    const named = [inKind, outKind].find((k) => k && k !== "*" && kinds.has(k));
    const takes = toType ? toType.accepts.filter((k) => kinds.has(k)) : [];
    const kind = pen ?? named ?? takes[0] ?? "http";
    used(kind);
    const id = readableId(`${c.source}-${c.target}`, bp.wires.map((w) => w.id));
    const ends = { fromPort: c.sourceHandle ?? undefined, toPort: c.targetHandle ?? undefined };
    change((b) => ({ ...b, wires: [...b.wires, { id, from: c.source, to: c.target, kind, calls: 1, notes: "", ...ends }] }));
    setSel({ kind: "wire", id });
  };

  const addPart = (typeId: string, at?: { x: number; y: number }) => {
    if (!bp) return;
    const t = partTypes(bp).find((x) => x.id === typeId);
    if (!t) return;
    let where = at;
    if (!where) {
      const r = canvas.current?.getBoundingClientRect();
      where = r ? flow.screenToFlowPosition({ x: r.left + r.width / 2, y: r.top + r.height / 2 }) : { x: 0, y: 0 };
      // Parts added by clicking step aside from ones already there, so none hides another.
      const taken = (x: number, y: number) => bp.parts.some((q) => Math.abs(q.x + 70 - x) < 40 && Math.abs(q.y + 25 - y) < 40);
      while (taken(where.x, where.y)) where = { x: where.x + 60, y: where.y + 60 };
    }
    const same = bp.parts.filter((p) => p.type === typeId).length;
    const label = same ? `${t.name} ${same + 1}` : t.name;
    const id = readableId(label, bp.parts.map((q) => q.id));
    const p: Part = {
      id,
      type: typeId,
      label,
      x: Math.round(where.x - 70),
      y: Math.round(where.y - 25),
      notes: "",
      set: {},
    };
    change((b) => ({ ...b, parts: [...b.parts, p] }));
    used(typeId);
    setSel({ kind: "part", id });
  };

  const setKind = (wire: string, kind: string) => {
    change((b) => ({ ...b, wires: b.wires.map((w) => (w.id === wire ? { ...w, kind } : w)) }));
    used(kind);
  };
  /** A connector clicked: new wires use it, and so does the selected wire. */
  const pickConnector = (id: string | null) => {
    setPen(id);
    if (id && sel?.kind === "wire") setKind(sel.id, id);
  };

  // Parts and connectors are dragged from the palette with pointer events: the
  // webview's own file drop takes over HTML drag and drop. A part dropped on the
  // canvas lands there; a connector dropped on a wire becomes that wire's.
  const [ghost, setGhost] = useState<{ type: PartType | ConnectorType; conn: boolean; x: number; y: number; moved: boolean } | null>(null);
  useEffect(() => {
    if (!ghost) return;
    const move = (e: PointerEvent) =>
      setGhost((g) => g && { ...g, x: e.clientX, y: e.clientY, moved: g.moved || Math.hypot(e.clientX - g.x, e.clientY - g.y) > 4 });
    const up = (e: PointerEvent) => {
      const over = document.elementFromPoint(e.clientX, e.clientY);
      const wire = over?.closest(".react-flow__edge")?.getAttribute("data-id");
      if (ghost.conn) {
        if (!ghost.moved) pickConnector(ghost.type.id);
        else if (wire) {
          setKind(wire, ghost.type.id);
          setSel({ kind: "wire", id: wire });
        }
      } else if (!ghost.moved) addPart(ghost.type.id);
      else if (over && canvas.current?.contains(over) && !over.closest(".bp-inventory"))
        addPart(ghost.type.id, flow.screenToFlowPosition({ x: e.clientX, y: e.clientY }));
      setGhost(null);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
  });

  /** Write the blueprint; an untitled one asks where first. Null if not saved. */
  const save = async (): Promise<BlueprintPath | null> => {
    if (!bp) return null;
    let target = file;
    if (!target) {
      const picked = await saveDialog({
        title: "Save blueprint",
        defaultPath: dir ? `${dir.replace(/[\\/]+$/, "")}\\${fileName(bp.name)}` : fileName(bp.name),
        filters: [{ name: "Blueprint", extensions: ["blueprint"] }],
      });
      if (!picked) return null;
      const full = /\.blueprint$/i.test(picked) ? picked : `${picked}.blueprint`;
      const cut = Math.max(full.lastIndexOf("\\"), full.lastIndexOf("/"));
      target = { root: full.slice(0, cut), path: full.slice(cut + 1) };
    }
    setSaving(true);
    try {
      // The agent records each part's code in this file while it builds;
      // replacing a change made on disk needs a yes.
      if (file) {
        const disk = await api.readTextFile(file.root, file.path).catch(() => null);
        if (disk !== null && written.current !== null && disk !== written.current) {
          const yes = await ask(`${file.path} changed on disk since this window read it (perhaps the agent recorded where it wrote each part's code). Replace it with this drawing?`);
          if (!yes) return null;
        }
      }
      const text = blueprintText(bp);
      await api.writeTextFile(target.root, target.path, text);
      written.current = text;
      setDirty(false);
      setError(null);
      if (!file) saved.current = `${target.root}\n${target.path}`;
      onSaved?.(target, !file);
      return target;
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (dirtyRef.current || !file) void save();
      }
      // Arrows scroll the canvas, except in a field or on a focused part (they move it).
      const step = ARROWS[e.key];
      const at = e.target;
      if (!step || typeEdit || !(at instanceof Element) || at.closest("input, textarea, select, [contenteditable], .react-flow__node")) return;
      e.preventDefault();
      const v = flow.getViewport();
      void flow.setViewport({ ...v, x: v.x + step[0], y: v.y + step[1] });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const build = async () => {
    if (!bp || !base) return;
    const target = dirtyRef.current || !file ? await save() : file;
    if (!target) return;
    const sub = target.path.includes("/") ? target.path.slice(0, target.path.lastIndexOf("/")) : "";
    onBuild(buildPrompt(forSave(bp), target.path, sub, base), target);
  };

  /** Run what the agent built and measure each part, after showing what will run. */
  const runSandbox = async () => {
    const sb = bp?.sandbox;
    if (!sb || !file) return;
    const here = file.path.includes("/") ? file.path.slice(0, file.path.lastIndexOf("/")) : "";
    const where = [here, sb.dir].filter(Boolean).join("/") || "the blueprint's folder";
    const yes = await ask(
      `Start the built system and measure it? This runs, in ${where}:\n\n${sb.start}\n\n` +
        (sb.stop ? `and to stop it:\n\n${sb.stop}\n\n` : "") +
        "Test requests go to this computer only, and the system is stopped afterwards.",
    );
    if (!yes) return;
    setSandbox({ running: true, status: "Starting…" });
    try {
      const report = await api.sandboxRun(file.root, here, sb, 5, 8, (status) => setSandbox((x) => x && { ...x, status }));
      setSandbox({ running: false, status: "Measured", report });
    } catch (e) {
      setSandbox({ running: false, status: "Failed", error: String(e) });
    }
  };

  /** Rename a category: the inventory's parts or connectors under it, and copies only in this blueprint. */
  const renameCategory = (kind: TypeEdit["kind"], from: string, to: string) => {
    const name = to.trim();
    if (!name || name === from) return;
    const move = <T extends { category?: string }>(t: T): T => (categoryOf(t) === from ? { ...t, category: name } : t);
    void storeLib(kind === "part" ? { ...lib, parts: lib.parts.map(move) } : { ...lib, connectors: lib.connectors.map(move) });
    setBp((b) => b && (kind === "part" ? { ...b, customParts: b.customParts.map(move) } : { ...b, customConnectors: b.customConnectors.map(move) }));
  };

  /** A part's code, from the blueprint's folder, opened in the main window. */
  const openCode = (p: Part) => {
    if (!file || !p.code?.trim()) return;
    const here = file.path.includes("/") ? file.path.slice(0, file.path.lastIndexOf("/") + 1) : "";
    const rel = p.code.trim().replace(/\\/g, "/").replace(/^\.\//, "").replace(/\/+$/, "");
    onOpenCode?.(file.root, here + rel);
  };

  const selPart = open?.kind === "part" ? bp?.parts.find((p) => p.id === open.id) : undefined;
  const selWire = open?.kind === "wire" ? bp?.wires.find((w) => w.id === open.id) : undefined;
  const inspect = (e: React.MouseEvent, kind: "part" | "wire", id: string) => {
    e.preventDefault();
    setSel({ kind, id });
    setOpen({ kind, id });
  };

  const palette = {
    pen,
    uses,
    onPen: pickConnector,
    onGrab: (type: PartType | ConnectorType, conn: boolean, e: React.PointerEvent) => {
      e.preventDefault();
      setGhost({ type, conn, x: e.clientX, y: e.clientY, moved: false });
    },
    lib,
    onEdit: setTypeEdit,
    onStore: keep,
    onRenameCategory: renameCategory,
    onRemove: async (t: TypeEdit) => {
      if (await ask(`Remove ${t.value.name} from the inventory? Blueprints that use it keep their own copy.`)) removeFromLib(t);
    },
  };

  const actions = (
    <>
      {dirty && <span className="muted unsaved">unsaved</span>}
      <button
        onClick={() => file && onSource?.(file)}
        disabled={!file}
        title={file ? "The file's text in the main window's editor: edit it there, and saving it updates this canvas" : "Save it first"}
      >
        Source
      </button>
      <button
        className={running ? "primary" : ""}
        onClick={() => {
          setRunning(!running);
          setDown(new Set());
        }}
        disabled={!bp}
        title="Simulate traffic through the blueprint to find bugs, bottlenecks and deployment risks"
      >
        {running ? "■ Stop" : "▶ Run"}
      </button>
      <button
        onClick={() => void runSandbox()}
        disabled={!file || !bp?.sandbox || !!sandbox?.running}
        title={
          bp?.sandbox
            ? "Run what the agent built and measure each part: real speeds and failures to set against the drawing"
            : "Build with agent first: it records how to run what it builds"
        }
      >
        Sandbox
      </button>
      <button
        onClick={() => void save()}
        disabled={(!dirty && !!file) || saving}
        title={file ? "Ctrl+S" : "Choose where to keep it (Ctrl+S)"}
      >
        {saving ? "Saving…" : file ? "Save" : "Save as…"}
      </button>
      <button
        onClick={() => setNonce((n) => n + 1)}
        disabled={!file && !dirty}
        title={dirty ? "Throw away your changes" : "Reload from disk"}
      >
        {dirty ? "Discard changes" : "Reload"}
      </button>
      <button
        className="primary"
        onClick={() => void build()}
        disabled={!bp || !bp.parts.length || problems > 0}
        title={problems ? `Run finds ${problems} problem${problems > 1 ? "s" : ""} to fix first` : "Save, then have the agent write all of it in one go"}
      >
        Build with agent
      </button>
    </>
  );

  return (
    <section className="file-viewer" aria-label={file?.path ?? "Untitled blueprint"}>
      {header(actions)}
      {error && <p className="failure">{error}</p>}
      {!bp && !error && <p className="muted">Loading…</p>}
      {bp && (
        <div className="bp">
          <Palette {...palette} bp={bp} onOpenInventory={() => setInventory(true)} />
          <div className="bp-canvas" ref={canvas}>
            {inventory && <Palette {...palette} bp={bp} full onClose={() => setInventory(false)} />}
            <ReactFlow
              key={nonce}
              nodes={nodes}
              edges={edges}
              nodeTypes={NODE_TYPES}
              onNodesChange={onNodesChange}
              onEdgesChange={onEdgesChange}
              onConnect={onConnect}
              onNodeContextMenu={(e, n) => inspect(e, "part", n.id)}
              onEdgeContextMenu={(e, w) => inspect(e, "wire", w.id)}
              onPaneContextMenu={(e) => e.preventDefault()}
              // The wheel and two fingers scroll; Ctrl+wheel and a pinch zoom.
              panOnScroll
              minZoom={0.2}
              onNodeDoubleClick={(_, n) => {
                const p = bp.parts.find((x) => x.id === n.id);
                if (p) openCode(p);
              }}
              isValidConnection={(c) => c.source !== c.target}
              deleteKeyCode={["Delete", "Backspace"]}
              colorMode="dark"
              fitView
              fitViewOptions={{ maxZoom: 1.2 }}
            >
              <Background gap={16} />
              <Controls showInteractive={false} />
            </ReactFlow>
            {!bp.parts.length && (
              <p className="bp-hint muted">
                Drag parts here from the left (or click one). Then drag from a part&apos;s right dot to
                another part&apos;s left dot to connect them, and press Run. Right-click a part or wire
                for its details. Scroll with the wheel, arrows or two fingers; zoom with Ctrl+wheel or a pinch.
              </p>
            )}
            {running && live && (
              <Results
                bp={bp}
                sim={live}
                scale={scale}
                onScale={setScale}
                onPick={(id) => {
                  setSel({ kind: "part", id });
                  setOpen({ kind: "part", id });
                  void flow.fitView({ nodes: [{ id }], duration: 300, maxZoom: 1.2 });
                }}
              />
            )}
            {sandbox && base && (
              <SandboxPanel
                bp={bp}
                sim={base}
                state={sandbox}
                onStop={() => void api.sandboxStop()}
                onClose={() => setSandbox(null)}
                onApply={(numbers) =>
                  change((b) => ({
                    ...b,
                    parts: b.parts.map((p) => (numbers[p.id] ? { ...p, set: { ...p.set, ...numbers[p.id] } } : p)),
                  }))
                }
              />
            )}
            {(selPart || selWire) && (
              <Details
                bp={bp}
                part={selPart}
                wire={selWire}
                running={running}
                down={down}
                onDown={(id, on) =>
                  setDown((d) => {
                    const next = new Set(d);
                    if (on) next.add(id);
                    else next.delete(id);
                    return next;
                  })
                }
                change={change}
                onRemovePart={(id) => {
                  removeParts([id]);
                  setSel(null);
                  setOpen(null);
                }}
                onRemoveWire={(id) => {
                  removeWires([id]);
                  setSel(null);
                  setOpen(null);
                }}
                onEditType={setTypeEdit}
                onOpenCode={file ? openCode : undefined}
                onClose={() => setOpen(null)}
              />
            )}
          </div>
        </div>
      )}
      {ghost && (
        <div className="bp-ghost" style={{ left: ghost.x + 8, top: ghost.y + 8, "--part": ghost.type.color } as CSSProperties}>
          {ghost.type.name}
        </div>
      )}
      {typeEdit && bp && (
        <TypeEditor
          bp={bp}
          edit={typeEdit}
          stored={[...lib.parts, ...lib.connectors].some((x) => x.id === typeEdit.value.id)}
          onClose={() => setTypeEdit(null)}
          onSave={(t) => {
            // Saving a part or connector always keeps it in the inventory.
            keep(t);
            setTypeEdit(null);
          }}
          onDelete={(t) => {
            removeFromLib(t);
            setTypeEdit(null);
          }}
        />
      )}
    </section>
  );
}

/** The common parts and connectors at the left, or (`full`) the whole inventory with search. */
function Palette({
  bp,
  lib,
  pen,
  uses,
  full,
  onClose,
  onOpenInventory,
  onPen,
  onGrab,
  onEdit,
  onStore,
  onRemove,
  onRenameCategory,
}: {
  bp: Blueprint;
  lib: Library;
  pen: string | null;
  uses: Record<string, number>;
  full?: boolean;
  onClose?: () => void;
  onOpenInventory?: () => void;
  onPen: (id: string | null) => void;
  onGrab: (t: PartType | ConnectorType, conn: boolean, e: React.PointerEvent) => void;
  onEdit: (t: TypeEdit) => void;
  onStore: (t: TypeEdit) => void;
  onRemove: (t: TypeEdit) => void;
  onRenameCategory: (kind: TypeEdit["kind"], from: string, to: string) => void;
}) {
  // Collapsed categories, and the one being renamed, as "part:Data" or "connector:Data".
  const [closed, setClosed] = useState<Set<string>>(new Set());
  const [renaming, setRenaming] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const searching = q.trim() !== "";
  const hit = <T extends PartType | ConnectorType>(list: T[]) => (searching ? list.filter((t) => matches(t, q)) : list);
  const grouped = <T extends PartType | ConnectorType>(kind: TypeEdit["kind"], items: T[], render: (t: T) => ReactNode) =>
    byCategory(items).map(([cat, list]) => {
      const k = `${kind}:${cat}`;
      // A search shows its matches, collapsed category or not.
      const open = searching || !closed.has(k);
      return (
        <div key={k} className="bp-group">
          {renaming === k ? (
            <form
              className="bp-group-rename"
              onSubmit={(e) => {
                e.preventDefault();
                onRenameCategory(kind, cat, (e.currentTarget.elements.namedItem("name") as HTMLInputElement).value);
                setRenaming(null);
              }}
            >
              <input
                name="name"
                autoFocus
                defaultValue={cat}
                onFocus={(e) => e.currentTarget.select()}
                onBlur={() => setRenaming(null)}
                onKeyDown={(e) => {
                  if (e.key !== "Escape") return;
                  // Ends the rename, not the inventory.
                  e.stopPropagation();
                  setRenaming(null);
                }}
                aria-label={`New name for the ${cat} category`}
              />
            </form>
          ) : (
            <div className="bp-group-head">
              <button
                className="bp-group-name"
                aria-expanded={open}
                onClick={() =>
                  setClosed((c) => {
                    const next = new Set(c);
                    if (open) next.add(k);
                    else next.delete(k);
                    return next;
                  })
                }
              >
                <span aria-hidden="true">{open ? "▾" : "▸"}</span> {cat} <span className="muted">{list.length}</span>
              </button>
              <button className="icon-button" onClick={() => setRenaming(k)} aria-label={`Rename the ${cat} category`} title="Rename this category">
                ✎
              </button>
            </div>
          )}
          {open && list.map(render)}
        </div>
      );
    });
  // A custom type is in the inventory, or only in this blueprint (made before
  // the inventory existed, or removed from it while still used here).
  const stored = new Set([...lib.parts, ...lib.connectors].map((t) => t.id));
  const tools = (t: TypeEdit) =>
    stored.has(t.value.id) ? (
      <>
        <button className="icon-button" onClick={() => onEdit(t)} aria-label={`Edit ${t.value.name}`} title="Edit (every blueprint using it gets the change)">
          ✎
        </button>
        <button className="icon-button" onClick={() => onRemove(t)} aria-label={`Remove ${t.value.name} from the inventory`} title="Remove from the inventory">
          🗑
        </button>
      </>
    ) : (
      <button className="icon-button" onClick={() => onStore(t)} aria-label={`Keep ${t.value.name} in the inventory`} title="Only in this blueprint: keep it in the inventory for every blueprint">
        ⤓
      </button>
    );
  const item = (t: PartType, custom: boolean) => (
    <div key={t.id} className="bp-item">
      <button
        className="bp-item-grab"
        style={{ "--part": t.color } as CSSProperties}
        onPointerDown={(e) => onGrab(t, false, e)}
        title={`${t.description || t.name} — drag onto the canvas, or click to add`}
      >
        <span className="bp-swatch" />
        {t.name}
      </button>
      {custom && tools({ kind: "part", value: t, isNew: false })}
    </div>
  );
  const conn = (c: ConnectorType, custom: boolean) => (
    <div key={c.id} className="bp-item">
      <button
        className={`bp-conn ${pen === c.id ? "picked" : ""}`}
        onPointerDown={(e) => onGrab(c, true, e)}
        // A key press (no pointer, so detail is 0) picks it too.
        onClick={(e) => e.detail === 0 && onPen(c.id)}
        title={`${c.description || c.name} — click to use it for new wires (and the selected one), or drag it onto a wire`}
      >
        <span className="bp-line" style={{ borderColor: c.color, borderTopStyle: c.sync ? "solid" : "dashed" }} />
        {c.name}
      </button>
      {custom && tools({ kind: "connector", value: c, isNew: false })}
    </div>
  );
  const automatic = (
    <button className={`bp-conn ${pen === null ? "picked" : ""}`} onClick={() => onPen(null)}>
      <span className="bp-line" style={{ borderColor: "var(--muted)" }} />
      Automatic
    </button>
  );
  const connNote = <p className="muted bp-note">The one picked is used for new wires. Drag one onto a wire to change it.</p>;
  if (!full) {
    const parts = mostUsed(partTypes(bp), uses, 8);
    const conns = mostUsed(connectorTypes(bp), uses, 5);
    // The connector in use stays in sight, common or not.
    const picked = connectorTypes(bp).find((c) => c.id === pen);
    if (picked && !conns.includes(picked)) conns.push(picked);
    return (
      <aside className="bp-palette" aria-label="Common parts and connectors">
        <h3 title="The ones you use most">Parts</h3>
        {parts.map((t) => item(t, false))}
        <h3 title="The ones you use most">Connectors</h3>
        {connNote}
        {automatic}
        {conns.map((c) => conn(c, false))}
        <button className="bp-new bp-open-inventory" onClick={onOpenInventory} title="Every part and connector, with search, and making your own">
          View full inventory
        </button>
      </aside>
    );
  }
  const none = searching && ![...PARTS, ...bp.customParts, ...CONNECTORS, ...bp.customConnectors].some((t) => matches(t, q));
  return (
    <aside className="bp-inventory" aria-label="Full inventory" onKeyDown={(e) => e.key === "Escape" && onClose?.()}>
      <div className="bp-inventory-head">
        <h3>Inventory</h3>
        <button className="icon-button" onClick={onClose} aria-label="Close the inventory" title="Close (Esc)">
          ✕
        </button>
      </div>
      <input
        type="search"
        className="bp-search"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder="Search parts and connectors"
        aria-label="Search the inventory"
        autoFocus
      />
      <div className="bp-inventory-list">
        {none && <p className="muted bp-note">Nothing matches &ldquo;{q.trim()}&rdquo;.</p>}
        <h3>Parts</h3>
        {hit(PARTS).map((t) => item(t, false))}
        <h3 title="Your own parts and connectors, kept for every blueprint">Your parts</h3>
        {bp.customParts.length === 0 && <p className="muted bp-note">Parts you make are kept here for every blueprint.</p>}
        {grouped("part", hit(bp.customParts), (t) => item(t, true))}
        <button
          className="bp-new"
          onClick={() =>
            onEdit({
              kind: "part",
              isNew: true,
              value: { id: `custom-${uid()}`, name: "", color: "#d9822b", description: "", capacity: 100, latency: 20, errorRate: 0, replicas: 1, stateful: false, source: 0, accepts: ["*"] },
            })
          }
        >
          + New part
        </button>
        <h3>Connectors</h3>
        {connNote}
        {!searching && automatic}
        {hit(CONNECTORS).map((c) => conn(c, false))}
        {grouped("connector", hit(bp.customConnectors), (c) => conn(c, true))}
        <button
          className="bp-new"
          onClick={() =>
            onEdit({ kind: "connector", isNew: true, value: { id: `custom-${uid()}`, name: "", color: "#d9822b", sync: true, latency: 5, description: "" } })
          }
        >
          + New connector
        </button>
      </div>
    </aside>
  );
}

/** A number field over a default: empty means "use the default". */
function NumberField({
  label,
  value: v,
  fallback,
  percent,
  onChange,
}: {
  label: string;
  value: number | undefined;
  fallback: number;
  percent?: boolean;
  onChange: (n: number | undefined) => void;
}) {
  const k = percent ? 100 : 1;
  return (
    <label className="bp-field">
      <span>{label}</span>
      <input
        type="number"
        min={0}
        step="any"
        value={v === undefined ? "" : +(v * k).toFixed(4)}
        placeholder={String(+(fallback * k).toFixed(4))}
        onChange={(e) => onChange(e.target.value === "" ? undefined : Math.max(0, Number(e.target.value)) / k)}
      />
    </label>
  );
}

const FIELDS: [Tunable, string, boolean?][] = [
  ["source", "Starts requests / s"],
  ["capacity", "Handles requests / s per copy (0 = no limit)"],
  ["latency", "Time per request (ms)"],
  ["errorRate", "Fails on its own (%)", true],
  ["replicas", "Copies running"],
];

function Details({
  bp,
  part,
  wire,
  running,
  down,
  onDown,
  change,
  onRemovePart,
  onRemoveWire,
  onEditType,
  onOpenCode,
  onClose,
}: {
  bp: Blueprint;
  part?: Part;
  wire?: Wire;
  running: boolean;
  down: Set<string>;
  onDown: (id: string, on: boolean) => void;
  change: (f: (b: Blueprint) => Blueprint) => void;
  onRemovePart: (id: string) => void;
  onRemoveWire: (id: string) => void;
  onEditType: (t: TypeEdit) => void;
  /** Absent until the blueprint is saved: a part's code is found from its file. */
  onOpenCode?: (p: Part) => void;
  onClose: () => void;
}) {
  const close = (
    <button className="icon-button bp-close" onClick={onClose} aria-label="Close details">
      ✕
    </button>
  );
  const setPart = (id: string, f: (p: Part) => Part) => change((b) => ({ ...b, parts: b.parts.map((p) => (p.id === id ? f(p) : p)) }));
  if (part) {
    const t = typeOf(bp, part);
    const custom = bp.customParts.find((x) => x.id === t.id);
    return (
      <aside className="bp-details" aria-label="Part details">
        {close}
        <h3 style={{ color: t.color }}>{t.name}</h3>
        {t.description && <p className="muted bp-note">{t.description}</p>}
        <label className="bp-field">
          <span>Name</span>
          <input value={part.label} onChange={(e) => setPart(part.id, (p) => ({ ...p, label: e.target.value }))} />
        </label>
        <label className="bp-field">
          <span>What it should do (for the agent)</span>
          <textarea rows={4} value={part.notes} onChange={(e) => setPart(part.id, (p) => ({ ...p, notes: e.target.value }))} />
        </label>
        <ContractEditor
          key={part.id}
          legend={part.contract || !t.contract ? "Contract: what it does" : "Contract (from its type; changing it changes only this one)"}
          contract={contractOf(bp, part)}
          onChange={(contract) => setPart(part.id, (p) => ({ ...p, contract }))}
        />
        {FIELDS.map(([key, label, percent]) => (
          <NumberField
            key={key}
            label={label}
            value={part.set[key]}
            fallback={t[key]}
            percent={percent}
            onChange={(n) =>
              setPart(part.id, (p) => {
                const set = { ...p.set };
                if (n === undefined) delete set[key];
                else set[key] = key === "replicas" ? Math.max(1, Math.round(n)) : n;
                return { ...p, set };
              })
            }
          />
        ))}
        {(t.settings ?? []).length > 0 && <h4>Settings</h4>}
        {(t.settings ?? []).map((s) => (
          <SettingField
            key={s.key}
            setting={s}
            value={part.values?.[s.key]}
            onChange={(v) =>
              setPart(part.id, (p) => {
                const values = { ...p.values };
                if (v === undefined) delete values[s.key];
                else values[s.key] = v;
                return { ...p, values };
              })
            }
          />
        ))}
        {t.ports?.length ? (
          <p className="muted bp-note">
            Ports: {t.ports.map((x) => `${x.dir === "in" ? "→" : ""}${x.name}${x.dir === "out" ? "→" : ""}`).join("  ")}
          </p>
        ) : null}
        <label className="bp-field">
          <span>Its code (file or folder, from the blueprint's folder)</span>
          <span className="bp-code">
            <input
              value={part.code ?? ""}
              placeholder="set by Build with agent"
              onChange={(e) => setPart(part.id, (p) => ({ ...p, code: e.target.value || undefined }))}
            />
            <button onClick={() => onOpenCode?.(part)} disabled={!part.code?.trim() || !onOpenCode} title="Or double-click the part">
              Open
            </button>
          </span>
        </label>
        {running && (
          <label className="bp-check">
            <input type="checkbox" checked={down.has(part.id)} onChange={(e) => onDown(part.id, e.target.checked)} />
            Simulate this part failing
          </label>
        )}
        <div className="row">
          {custom && <button onClick={() => onEditType({ kind: "part", value: custom, isNew: false })}>Edit part type</button>}
          <button className="danger" onClick={() => onRemovePart(part.id)}>
            Delete
          </button>
        </div>
      </aside>
    );
  }
  if (wire) {
    const from = bp.parts.find((p) => p.id === wire.from);
    const to = bp.parts.find((p) => p.id === wire.to);
    const target = to && typeOf(bp, to);
    const setWire = (f: (w: Wire) => Wire) => change((b) => ({ ...b, wires: b.wires.map((w) => (w.id === wire.id ? f(w) : w)) }));
    return (
      <aside className="bp-details" aria-label="Connection details">
        {close}
        <h3>
          {from?.label}
          {from && typeOf(bp, from).ports?.find((x) => x.id === wire.fromPort) ? `.${typeOf(bp, from).ports!.find((x) => x.id === wire.fromPort)!.name}` : ""} → {to?.label}
          {target?.ports?.find((x) => x.id === wire.toPort) ? `.${target.ports.find((x) => x.id === wire.toPort)!.name}` : ""}
        </h3>
        <label className="bp-field">
          <span>Connector</span>
          <select value={wire.kind} onChange={(e) => setWire((w) => ({ ...w, kind: e.target.value }))}>
            {connectorTypes(bp).map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
                {target && !accepts(target, c.id) ? ` (a ${target.name} does not take this)` : ""}
              </option>
            ))}
          </select>
        </label>
        {(() => {
          const ops = to ? (contractOf(bp, to)?.operations ?? []) : [];
          const op = ops.find((o) => o.name === wire.op);
          if (!ops.length && !wire.op) return null;
          return (
            <>
              <label className="bp-field">
                <span>Calls ({to?.label}&apos;s operation)</span>
                <select value={wire.op ?? ""} onChange={(e) => setWire((w) => ({ ...w, op: e.target.value || undefined }))}>
                  <option value="">(not said)</option>
                  {wire.op && !op && <option value={wire.op}>{wire.op} (not in its contract)</option>}
                  {ops.map((o) => (
                    <option key={o.name} value={o.name}>
                      {o.name}({o.takes})
                    </option>
                  ))}
                </select>
              </label>
              {wire.op && (
                <label className="bp-field">
                  <span>Sends (comma separated; Run checks it against what {wire.op} takes)</span>
                  <input
                    value={wire.sends ?? ""}
                    placeholder={op?.takes ? fieldsOf(op.takes).join(", ") : ""}
                    onChange={(e) => setWire((w) => ({ ...w, sends: e.target.value || undefined }))}
                  />
                </label>
              )}
            </>
          );
        })()}
        <label className="bp-field">
          <span>Calls per request</span>
          <input
            type="number"
            min={0}
            step="any"
            value={wire.calls}
            onChange={(e) => setWire((w) => ({ ...w, calls: Math.max(0, Number(e.target.value) || 0) }))}
          />
        </label>
        <label className="bp-field">
          <span>What goes over it (for the agent)</span>
          <textarea rows={3} value={wire.notes} onChange={(e) => setWire((w) => ({ ...w, notes: e.target.value }))} />
        </label>
        <div className="row">
          <button className="danger" onClick={() => onRemoveWire(wire.id)}>
            Delete
          </button>
        </div>
      </aside>
    );
  }
  return null;
}

function Results({
  bp,
  sim,
  scale,
  onScale,
  onPick,
}: {
  bp: Blueprint;
  sim: NonNullable<ReturnType<typeof simulate>>;
  scale: number;
  onScale: (n: number) => void;
  onPick: (id: string) => void;
}) {
  const label = (id: string) => bp.parts.find((p) => p.id === id)?.label ?? id;
  return (
    <section className="bp-results" aria-label="Simulation results">
      <div className="bp-results-bar">
        <strong>Simulation</strong>
        <label className="bp-scale">
          Load ×{scale < 1 ? scale.toFixed(2) : scale.toFixed(1)}
          {/* Logarithmic, 0.1× to 50×. */}
          <input
            type="range"
            min={-1}
            max={Math.log10(50)}
            step={0.01}
            value={Math.log10(scale)}
            onChange={(e) => onScale(10 ** Number(e.target.value))}
            aria-label="Load multiplier"
          />
        </label>
        <button onClick={() => onScale(1)} disabled={scale === 1}>
          ×1
        </button>
      </div>
      <ul className="bp-entries">
        {sim.entries.map((e) => (
          <li key={e.part}>
            From <strong>{label(e.part)}</strong>: {rate(e.rps)} req/s · {ms(e.latency)} each ·{" "}
            <span className={e.ok < 0.99 ? "failure" : "ok"}>{(e.ok * 100).toFixed(1)}% succeed</span>
          </li>
        ))}
      </ul>
      {sim.findings.length === 0 ? (
        <p className="muted">No problems found.</p>
      ) : (
        <ul className="bp-findings">
          {sim.findings.map((f, i) => (
            <li key={i} className={f.level}>
              <button className="link-button" onClick={() => f.parts[0] && onPick(f.parts[0])} disabled={!f.parts.length}>
                <span className="bp-tag">{f.area === "bug" ? "Bug" : f.area === "scale" ? "Scaling" : "Deploy"}</span>
                {f.text}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Lines of text, one item each: kept as typed until it is used, so Enter works. */
function LinesField({ label, value, placeholder, onChange }: { label: string; value: string[]; placeholder: string; onChange: (lines: string[]) => void }) {
  const [raw, setRaw] = useState(value.join("\n"));
  return (
    <label className="bp-field">
      <span>{label}</span>
      <textarea
        rows={2}
        value={raw}
        placeholder={placeholder}
        onChange={(e) => {
          setRaw(e.target.value);
          onChange(e.target.value.split("\n").map((l) => l.trim()).filter(Boolean));
        }}
      />
    </label>
  );
}

/** What a part does: the operations others call, the data it keeps, its rules. */
function ContractEditor({ legend, contract, onChange }: { legend: string; contract?: Contract; onChange: (c: Contract) => void }) {
  const c = contract ?? emptyContract();
  const editOp = (i: number, patch: Partial<Contract["operations"][number]>) =>
    onChange({ ...c, operations: c.operations.map((o, j) => (j === i ? { ...o, ...patch } : o)) });
  return (
    <fieldset className="bp-list bp-contract">
      <legend>{legend}</legend>
      {c.operations.map((o, i) => (
        <div key={i} className="bp-op">
          <input value={o.name} onChange={(e) => editOp(i, { name: e.target.value })} placeholder="createOrder" aria-label="Operation name" />
          <button type="button" className="icon-button" onClick={() => onChange({ ...c, operations: c.operations.filter((_, j) => j !== i) })} aria-label={`Remove ${o.name || "this operation"}`}>
            ✕
          </button>
          <input value={o.takes} onChange={(e) => editOp(i, { takes: e.target.value })} placeholder="takes: items, customerId" aria-label="What it takes" />
          <input value={o.returns} onChange={(e) => editOp(i, { returns: e.target.value })} placeholder="returns: orderId" aria-label="What it returns" />
        </div>
      ))}
      <button type="button" className="bp-new" onClick={() => onChange({ ...c, operations: [...c.operations, { name: "", takes: "", returns: "" }] })}>
        + Add operation
      </button>
      <LinesField label="Data it keeps (one per line)" value={c.data} placeholder="orders(id, items, total, status)" onChange={(data) => onChange({ ...c, data })} />
      <LinesField label="Rules (one per line)" value={c.rules} placeholder="Reject an order if an item is out of stock" onChange={(rules) => onChange({ ...c, rules })} />
    </fieldset>
  );
}

/** The sandbox's progress, then each part as measured against the drawing. */
function SandboxPanel({
  bp,
  sim,
  state,
  onStop,
  onClose,
  onApply,
}: {
  bp: Blueprint;
  sim: NonNullable<ReturnType<typeof simulate>>;
  state: { running: boolean; status: string; report?: SandboxReport; error?: string };
  onStop: () => void;
  onClose: () => void;
  onApply: (numbers: Record<string, { latency: number; errorRate: number }>) => void;
}) {
  const label = (id: string) => bp.parts.find((p) => p.id === id)?.label ?? id;
  const measured = Object.fromEntries((state.report?.parts ?? []).map((m) => [m.part, m]));
  const [applied, setApplied] = useState(false);
  return (
    <section className="bp-results bp-sandbox" aria-label="Sandbox">
      <div className="bp-results-bar">
        <strong>Sandbox</strong>
        <span className="muted">{state.status}</span>
        {state.running ? (
          <button onClick={onStop}>Stop</button>
        ) : (
          <button onClick={onClose} aria-label="Close the sandbox results">
            Close
          </button>
        )}
      </div>
      {state.error && <pre className="failure bp-sandbox-error">{state.error}</pre>}
      {state.report && (
        <>
          <table className="bp-sandbox-table">
            <thead>
              <tr>
                <th>Part</th>
                <th>Drawing expects</th>
                <th>Measured (typical / slow)</th>
                <th>Failed</th>
                <th>Answered</th>
              </tr>
            </thead>
            <tbody>
              {state.report.parts.map((m) => {
                const s = sim.parts[m.part];
                const failed = m.requests ? m.failures / m.requests : 0;
                // 8 callers at once is a floor on what it handles, not its limit.
                const short = s && s.load > 0 && m.throughput < s.load;
                return (
                  <tr key={m.part} title={m.error ?? undefined}>
                    <td>{label(m.part)}</td>
                    <td>{s ? ms(s.total) : "—"}</td>
                    <td>
                      {ms(m.p50)} / {ms(m.p95)}
                    </td>
                    <td className={failed > 0.01 ? "failure" : ""}>
                      {(failed * 100).toFixed(1)}%{m.error && failed > 0 ? ` (${m.error})` : ""}
                    </td>
                    <td className={short ? "failure" : ""}>
                      {rate(m.throughput)}/s{short ? ` (needs ${rate(s.load)}/s)` : ""}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="row">
            <button
              className="primary"
              disabled={applied || !state.report.parts.length}
              onClick={() => {
                onApply(calibrate(bp, measured));
                setApplied(true);
              }}
              title="Each measured part's own time and failure rate go into the drawing (what it waits on is taken off), so Run works from real numbers"
            >
              {applied ? "Measured numbers in use" : "Use measured numbers"}
            </button>
            <span className="muted bp-note">Answered is with 8 requests at a time: a floor on what it handles, not its limit.</span>
          </div>
          {state.report.output.trim() && (
            <details>
              <summary className="muted">What the system printed</summary>
              <pre className="bp-sandbox-output">{state.report.output}</pre>
            </details>
          )}
        </>
      )}
    </section>
  );
}

/** A setting's field: empty means its default. */
function SettingField({ setting, value, onChange }: { setting: Setting; value?: string; onChange: (v: string | undefined) => void }) {
  return (
    <label className="bp-field">
      <span>{setting.label}</span>
      {setting.type === "choice" ? (
        <select value={value ?? setting.default} onChange={(e) => onChange(e.target.value === setting.default ? undefined : e.target.value)}>
          {(setting.options ?? []).map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
      ) : (
        <input
          type={setting.type === "number" ? "number" : "text"}
          value={value ?? ""}
          placeholder={setting.default}
          onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
        />
      )}
    </label>
  );
}

/** The parts builder: a description in, the form below filled in. */
function Builder({ bp, kind, onBuilt }: { bp: Blueprint; kind: TypeEdit["kind"]; onBuilt: (v: PartType | ConnectorType) => void }) {
  const [desc, setDesc] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [models, setModels] = useState<ModelOption[]>([]);
  useEffect(() => {
    api
      .listModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);
  // The model last picked in a chat, else a Claude.
  const model = preferredModel(models, claudeFirst);
  const connectors = connectorTypes(bp);
  const free = () => {
    setError(null);
    onBuilt(kind === "part" ? buildPartFree(desc) : buildConnectorFree(desc));
  };
  const ai = async () => {
    if (!model) return;
    setBusy(true);
    setError(null);
    try {
      const reply = await api.askModel(model.backend, model.model_id, partPrompt(kind, connectors), desc);
      onBuilt(kind === "part" ? parseAiPart(reply, desc, connectors) : parseAiConnector(reply, desc));
    } catch (e) {
      setError(`${e}. The free builder works without AI.`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <fieldset className="bp-builder">
      <legend>Build it from a description</legend>
      <textarea
        rows={2}
        value={desc}
        placeholder={kind === "part" ? "e.g. A Postgres database for orders, 2 replicas, 5 ms per query" : "e.g. gRPC between internal services, 3 ms"}
        onChange={(e) => setDesc(e.target.value)}
        aria-label={`Describe the ${kind}`}
      />
      <div className="row">
        <button type="button" onClick={free} disabled={!desc.trim() || busy} title="Reads the description itself: no AI, no cost, instant">
          Build (free)
        </button>
        <button
          type="button"
          onClick={() => void ai()}
          disabled={!desc.trim() || busy || !model}
          title={model ? `Asks ${model.display_name}; costs a little` : "Add a model in Settings first"}
        >
          {busy ? "Building…" : "Build with AI"}
        </button>
        <span className="muted bp-note">{model ? `AI: ${model.display_name}` : "AI needs a model (Settings)"}</span>
      </div>
      {error && <p className="failure">{error}</p>}
      <p className="muted bp-note">It fills in the form below. Check it, change what you like, then Save.</p>
    </fieldset>
  );
}

function PortsEditor({ bp, ports, onChange }: { bp: Blueprint; ports: Port[]; onChange: (ports: Port[]) => void }) {
  const edit = (i: number, patch: Partial<Port>) => onChange(ports.map((p, j) => (j === i ? { ...p, ...patch } : p)));
  return (
    <fieldset className="bp-list">
      <legend>Ports: ways in (how others call it) and out (what it calls)</legend>
      {ports.map((p, i) => (
        <div key={p.id} className="bp-list-row">
          <input value={p.name} onChange={(e) => edit(i, { name: e.target.value })} aria-label="Port name" />
          <select value={p.dir} onChange={(e) => edit(i, { dir: e.target.value as Port["dir"] })} aria-label="Direction">
            <option value="in">in</option>
            <option value="out">out</option>
          </select>
          <select value={p.kind} onChange={(e) => edit(i, { kind: e.target.value })} aria-label="Connector">
            <option value="*">any connector</option>
            {connectorTypes(bp).map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
          <button type="button" className="icon-button" onClick={() => onChange(ports.filter((_, j) => j !== i))} aria-label={`Remove port ${p.name}`}>
            ✕
          </button>
        </div>
      ))}
      <button type="button" className="bp-new" onClick={() => onChange([...ports, { id: `port_${uid().slice(0, 4)}`, name: "port", dir: "in", kind: "*" }])}>
        + Add port
      </button>
    </fieldset>
  );
}

function SettingsEditor({ settings, onChange }: { settings: Setting[]; onChange: (settings: Setting[]) => void }) {
  const edit = (i: number, patch: Partial<Setting>) => onChange(settings.map((s, j) => (j === i ? { ...s, ...patch } : s)));
  return (
    <fieldset className="bp-list">
      <legend>Settings it is built with (each placed part can change them)</legend>
      {settings.map((s, i) => (
        <div key={s.key} className="bp-list-row">
          <input value={s.label} onChange={(e) => edit(i, { label: e.target.value })} aria-label="Setting name" placeholder="Name" />
          <select value={s.type} onChange={(e) => edit(i, { type: e.target.value as Setting["type"] })} aria-label="Kind of value">
            <option value="text">text</option>
            <option value="number">number</option>
            <option value="choice">choice</option>
          </select>
          <input value={s.default} onChange={(e) => edit(i, { default: e.target.value })} aria-label="Default" placeholder="Default" />
          {s.type === "choice" && (
            <input
              value={(s.options ?? []).join(", ")}
              onChange={(e) => edit(i, { options: e.target.value.split(",").map((o) => o.trim()).filter(Boolean) })}
              aria-label="Choices, comma separated"
              placeholder="Choices, a, b, c"
            />
          )}
          <button type="button" className="icon-button" onClick={() => onChange(settings.filter((_, j) => j !== i))} aria-label={`Remove setting ${s.label}`}>
            ✕
          </button>
        </div>
      ))}
      <button
        type="button"
        className="bp-new"
        onClick={() => onChange([...settings, { key: `setting_${uid().slice(0, 4)}`, label: "", type: "text", default: "" }])}
      >
        + Add setting
      </button>
    </fieldset>
  );
}

function TypeEditor({
  bp,
  edit,
  stored,
  onClose,
  onSave,
  onDelete,
}: {
  bp: Blueprint;
  edit: TypeEdit;
  /** Kept in the inventory, rather than only in this blueprint. */
  stored: boolean;
  onClose: () => void;
  onSave: (t: TypeEdit) => void;
  onDelete: (t: TypeEdit) => void;
}) {
  // A part edits its ports as a list: the plain in/out of an older part become real ports.
  const [t, setT] = useState<TypeEdit>(edit.kind === "part" ? { ...edit, value: { ...edit.value, ports: portsOf(edit.value) } } : edit);
  const used =
    t.kind === "part" ? bp.parts.filter((p) => p.type === t.value.id).length : bp.wires.filter((w) => w.kind === t.value.id).length;
  const set = (patch: Partial<PartType & ConnectorType>) => setT((x) => ({ ...x, value: { ...x.value, ...patch } }) as TypeEdit);
  const num = (key: string, label: string, k = 1) => (
    <label className="bp-field">
      <span>{label}</span>
      <input
        type="number"
        min={0}
        step="any"
        value={+(((t.value as unknown as Record<string, number>)[key] ?? 0) * k).toFixed(4)}
        onChange={(e) => set({ [key]: Math.max(0, Number(e.target.value) || 0) / k })}
      />
    </label>
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <form
        className="graph-dialog bp-type-editor"
        role="dialog"
        aria-modal="true"
        aria-label={t.kind === "part" ? "Part type" : "Connector type"}
        onSubmit={(e) => {
          e.preventDefault();
          if (!t.value.name.trim()) return;
          // The same checks a built part gets: unique port ids, sane numbers, `accepts` from the ports.
          onSave(
            t.kind === "part"
              ? { ...t, value: normalisePart({ ...t.value, settings: (t.value.settings ?? []).filter((s) => s.label.trim()) }, connectorTypes(bp)) }
              : { ...t, value: normaliseConnector(t.value) },
          );
        }}
      >
        <header>
          <h2>
            {t.isNew ? "New" : "Edit"} {t.kind}
          </h2>
          <button type="button" onClick={onClose} aria-label="Close">
            ✕
          </button>
        </header>
        <Builder
          bp={bp}
          kind={t.kind}
          // The built part replaces the form, keeping the id so an edit stays the same part.
          onBuilt={(v) => setT((x) => ({ ...x, value: { ...v, id: x.value.id } }) as TypeEdit)}
        />
        <div className="bp-type-grid">
          <label className="bp-field">
            <span>Name</span>
            <input required value={t.value.name} onChange={(e) => set({ name: e.target.value })} />
          </label>
          <label className="bp-field">
            <span>Category</span>
            <input
              list="bp-categories"
              value={t.value.category ?? ""}
              placeholder="General"
              onChange={(e) => set({ category: e.target.value })}
            />
            <datalist id="bp-categories">
              {[...new Set([...bp.customParts, ...bp.customConnectors].map(categoryOf))].map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>
          </label>
          <label className="bp-field">
            <span>Colour</span>
            <input type="color" value={t.value.color} onChange={(e) => set({ color: e.target.value })} />
          </label>
        </div>
        <label className="bp-field">
          <span>What it is (the agent builds from this)</span>
          <textarea rows={3} value={t.value.description} onChange={(e) => set({ description: e.target.value })} />
        </label>
        {t.kind === "part" ? (
          <>
            <div className="bp-type-grid">
              {num("source", "Starts requests / s")}
              {num("capacity", "Handles requests / s per copy (0 = no limit)")}
              {num("latency", "Time per request (ms)")}
              {num("errorRate", "Fails on its own (%)", 100)}
              {num("replicas", "Copies running")}
            </div>
            <label className="bp-check">
              <input type="checkbox" checked={t.value.stateful} onChange={(e) => set({ stateful: e.target.checked })} />
              Holds data (more copies need replication)
            </label>
            <PortsEditor bp={bp} ports={t.value.ports ?? []} onChange={(ports) => set({ ports })} />
            <SettingsEditor settings={t.value.settings ?? []} onChange={(settings) => set({ settings })} />
            <ContractEditor
              legend="Contract: what every part of this kind does (a placed one can change its own)"
              contract={t.value.contract}
              onChange={(contract) => set({ contract })}
            />
          </>
        ) : (
          <>
            {num("latency", "Network time per call (ms)")}
            <label className="bp-check">
              <input type="checkbox" checked={t.value.sync} onChange={(e) => set({ sync: e.target.checked })} />
              The caller waits for the answer (off: fire and forget, like a queue)
            </label>
          </>
        )}
        <p className="muted bp-note">
          Saved to the inventory, for every blueprint.{used > 0 && ` Used ${used} time${used > 1 ? "s" : ""} here.`}
        </p>
        <div className="row">
          <button className="primary" type="submit">
            Save
          </button>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          {stored && (
            <button type="button" className="danger" onClick={() => onDelete(t)}>
              Remove from inventory
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
