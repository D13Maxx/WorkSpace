// A blueprint file (.blueprint): software drawn as parts wired with
// connectors, run in a simulation, then handed to the agent to build.
//
// The file is plain JSON (see blueprint/model.ts), so the agent can read and
// change it too; the view reloads when it does.

import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
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
  type Connection,
  type Edge,
  type EdgeChange,
  type Node,
  type NodeChange,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { api } from "../api";
import {
  CONNECTORS,
  PARTS,
  accepts,
  buildPrompt,
  connectorTypes,
  ms,
  parseBlueprint,
  partTypes,
  rate,
  simulate,
  typeOf,
  type Blueprint,
  type ConnectorType,
  type Finding,
  type Part,
  type PartStats,
  type PartType,
  type Tunable,
  type Wire,
} from "../blueprint/model";

interface Props {
  project: string;
  path: string;
  version: number;
  header: (actions: ReactNode) => ReactNode;
  /** Hand the finished blueprint to the agent, as this request. */
  onBuild: (prompt: string) => void;
}

const uid = () => crypto.randomUUID().slice(0, 8);

type PartData = { part: Part; type: PartType; stats?: PartStats; down: boolean; level?: Finding["level"] };
type PartNodeT = Node<PartData, "part">;

function PartNode({ data, selected }: NodeProps<PartNodeT>) {
  const { part, type, stats, down, level } = data;
  return (
    <div
      className={`bp-part ${selected ? "selected" : ""} ${level ?? ""} ${down ? "down" : ""}`}
      style={{ "--part": type.color } as CSSProperties}
      title={type.description}
    >
      <Handle type="target" position={Position.Left} />
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
      <Handle type="source" position={Position.Right} />
    </div>
  );
}
const NODE_TYPES = { part: PartNode };

export function BlueprintFile(props: Props) {
  return (
    <ReactFlowProvider>
      <Editor {...props} />
    </ReactFlowProvider>
  );
}

type Selection = { kind: "part" | "wire"; id: string } | null;
type TypeEdit = { kind: "part"; value: PartType; isNew: boolean } | { kind: "connector"; value: ConnectorType; isNew: boolean };

function Editor({ project, path, version, header, onBuild }: Props) {
  const name = (path.split("/").pop() ?? "blueprint").replace(/\.blueprint$/i, "");
  const [bp, setBp] = useState<Blueprint | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [sel, setSel] = useState<Selection>(null);
  // Run: the simulation drawn over the parts, at `scale` times the load, with `down` failed.
  const [running, setRunning] = useState(false);
  const [scale, setScale] = useState(1);
  const [down, setDown] = useState<Set<string>>(new Set());
  // The connector new wires use; null picks one the target takes.
  const [pen, setPen] = useState<string | null>(null);
  const [typeEdit, setTypeEdit] = useState<TypeEdit | null>(null);
  // The blueprint's own details (name, purpose, stack), shown when nothing is selected.
  const [info, setInfo] = useState(false);
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const flow = useReactFlow();
  const canvas = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    setDirty(false);
    api
      .readTextFile(project, path)
      .then((text) => {
        if (!live) return;
        const next = parseBlueprint(text, name);
        setBp(next);
        // A new blueprint starts by saying what it is for.
        setInfo(!next.parts.length && !next.description);
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [project, path, name, nonce]);

  // Pick up the agent's edits, but never over unsaved work.
  const seen = useRef(version);
  useEffect(() => {
    if (version === seen.current) return;
    seen.current = version;
    if (!dirtyRef.current) setNonce((n) => n + 1);
  }, [version]);

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
  const edges: Edge[] = (bp?.wires ?? []).map((w) => {
    const c = kinds.get(w.kind);
    const load = live?.wires[w.id] ?? 0;
    const color = c?.color ?? "#888";
    return {
      id: w.id,
      source: w.from,
      target: w.to,
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
    const takes = target ? typeOf(bp, target).accepts.filter((k) => kinds.has(k)) : [];
    const kind = pen ?? takes[0] ?? "http";
    const id = uid();
    change((b) => ({ ...b, wires: [...b.wires, { id, from: c.source, to: c.target, kind, calls: 1, notes: "" }] }));
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
    }
    const same = bp.parts.filter((p) => p.type === typeId).length;
    const id = uid();
    const p: Part = {
      id,
      type: typeId,
      label: same ? `${t.name} ${same + 1}` : t.name,
      x: Math.round(where.x - 70),
      y: Math.round(where.y - 25),
      notes: "",
      set: {},
    };
    change((b) => ({ ...b, parts: [...b.parts, p] }));
    setSel({ kind: "part", id });
  };

  // Parts are dragged from the palette with pointer events: the webview's own
  // file drop takes over HTML drag and drop.
  const [ghost, setGhost] = useState<{ type: PartType; x: number; y: number; moved: boolean } | null>(null);
  useEffect(() => {
    if (!ghost) return;
    const move = (e: PointerEvent) =>
      setGhost((g) => g && { ...g, x: e.clientX, y: e.clientY, moved: g.moved || Math.hypot(e.clientX - g.x, e.clientY - g.y) > 4 });
    const up = (e: PointerEvent) => {
      const over = document.elementFromPoint(e.clientX, e.clientY);
      if (!ghost.moved) addPart(ghost.type.id);
      else if (over && canvas.current?.contains(over)) addPart(ghost.type.id, flow.screenToFlowPosition({ x: e.clientX, y: e.clientY }));
      setGhost(null);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
  });

  const save = async (): Promise<boolean> => {
    if (!bp) return false;
    setSaving(true);
    try {
      await api.writeTextFile(project, path, JSON.stringify(bp, null, 2) + "\n");
      setDirty(false);
      setError(null);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (dirtyRef.current) void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const build = async () => {
    if (!bp || !base || (dirtyRef.current && !(await save()))) return;
    const dir = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
    onBuild(buildPrompt(bp, path, dir, base));
  };

  const selPart = sel?.kind === "part" ? bp?.parts.find((p) => p.id === sel.id) : undefined;
  const selWire = sel?.kind === "wire" ? bp?.wires.find((w) => w.id === sel.id) : undefined;

  const actions = (
    <>
      {dirty && <span className="muted unsaved">unsaved</span>}
      <button onClick={() => (setSel(null), setInfo(!info))} disabled={!bp} title="Name, purpose and stack of this blueprint">
        Describe
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
      <button onClick={() => void save()} disabled={!dirty || saving} title="Ctrl+S">
        {saving ? "Saving…" : "Save"}
      </button>
      <button
        onClick={() => setNonce((n) => n + 1)}
        title={dirty ? "Throw away your changes and reload from disk" : "Reload from disk"}
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
    <section className="file-viewer" aria-label={path}>
      {header(actions)}
      {error && <p className="failure">{error}</p>}
      {!bp && !error && <p className="muted">Loading…</p>}
      {bp && (
        <div className="bp">
          <Palette
            bp={bp}
            pen={pen}
            onPen={setPen}
            onGrab={(type, e) => {
              e.preventDefault();
              setGhost({ type, x: e.clientX, y: e.clientY, moved: false });
            }}
            onEdit={setTypeEdit}
          />
          <div className="bp-canvas" ref={canvas}>
            <ReactFlow
              key={nonce}
              nodes={nodes}
              edges={edges}
              nodeTypes={NODE_TYPES}
              onNodesChange={onNodesChange}
              onEdgesChange={onEdgesChange}
              onConnect={onConnect}
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
                another part&apos;s left dot to connect them, and press Run.
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
                  void flow.fitView({ nodes: [{ id }], duration: 300, maxZoom: 1.2 });
                }}
              />
            )}
            {(selPart || selWire || info) && (
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
                }}
                onRemoveWire={(id) => {
                  removeWires([id]);
                  setSel(null);
                }}
                onEditType={setTypeEdit}
                onClose={() => (setSel(null), setInfo(false))}
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
          onClose={() => setTypeEdit(null)}
          onSave={(t) => {
            change((b) =>
              t.kind === "part"
                ? { ...b, customParts: t.isNew ? [...b.customParts, t.value] : b.customParts.map((x) => (x.id === t.value.id ? t.value : x)) }
                : {
                    ...b,
                    customConnectors: t.isNew
                      ? [...b.customConnectors, t.value]
                      : b.customConnectors.map((x) => (x.id === t.value.id ? t.value : x)),
                  },
            );
            setTypeEdit(null);
          }}
          onDelete={(t) => {
            change((b) =>
              t.kind === "part"
                ? { ...b, customParts: b.customParts.filter((x) => x.id !== t.value.id) }
                : { ...b, customConnectors: b.customConnectors.filter((x) => x.id !== t.value.id) },
            );
            setTypeEdit(null);
          }}
        />
      )}
    </section>
  );
}

function Palette({
  bp,
  pen,
  onPen,
  onGrab,
  onEdit,
}: {
  bp: Blueprint;
  pen: string | null;
  onPen: (id: string | null) => void;
  onGrab: (t: PartType, e: React.PointerEvent) => void;
  onEdit: (t: TypeEdit) => void;
}) {
  const item = (t: PartType, custom: boolean) => (
    <div key={t.id} className="bp-item">
      <button
        className="bp-item-grab"
        style={{ "--part": t.color } as CSSProperties}
        onPointerDown={(e) => onGrab(t, e)}
        title={`${t.description || t.name} — drag onto the canvas, or click to add`}
      >
        <span className="bp-swatch" />
        {t.name}
      </button>
      {custom && (
        <button className="icon-button" onClick={() => onEdit({ kind: "part", value: t, isNew: false })} aria-label={`Edit ${t.name}`} title="Edit this part">
          ✎
        </button>
      )}
    </div>
  );
  const connectors = connectorTypes(bp);
  return (
    <aside className="bp-palette" aria-label="Parts and connectors">
      <h3>Parts</h3>
      {PARTS.map((t) => item(t, false))}
      <h3>Your parts</h3>
      {bp.customParts.map((t) => item(t, true))}
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
      <p className="muted bp-note">The one picked is used for new wires.</p>
      <button className={`bp-conn ${pen === null ? "picked" : ""}`} onClick={() => onPen(null)}>
        <span className="bp-line" style={{ borderColor: "var(--muted)" }} />
        Automatic
      </button>
      {connectors.map((c) => (
        <div key={c.id} className="bp-item">
          <button className={`bp-conn ${pen === c.id ? "picked" : ""}`} onClick={() => onPen(c.id)} title={c.description}>
            <span className="bp-line" style={{ borderColor: c.color, borderTopStyle: c.sync ? "solid" : "dashed" }} />
            {c.name}
          </button>
          {!CONNECTORS.includes(c) && (
            <button className="icon-button" onClick={() => onEdit({ kind: "connector", value: c, isNew: false })} aria-label={`Edit ${c.name}`} title="Edit this connector">
              ✎
            </button>
          )}
        </div>
      ))}
      <button
        className="bp-new"
        onClick={() =>
          onEdit({ kind: "connector", isNew: true, value: { id: `custom-${uid()}`, name: "", color: "#d9822b", sync: true, latency: 5, description: "" } })
        }
      >
        + New connector
      </button>
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
          {from?.label} → {to?.label}
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
  return (
    <aside className="bp-details" aria-label="Blueprint details">
        {close}
      <h3>Blueprint</h3>
      <label className="bp-field">
        <span>Name</span>
        <input value={bp.name} onChange={(e) => change((b) => ({ ...b, name: e.target.value }))} />
      </label>
      <label className="bp-field">
        <span>What the system is for</span>
        <textarea rows={5} value={bp.description} onChange={(e) => change((b) => ({ ...b, description: e.target.value }))} />
      </label>
      <label className="bp-field">
        <span>Stack (languages, frameworks, hosting)</span>
        <input
          value={bp.stack}
          placeholder="e.g. TypeScript, Postgres, Docker"
          onChange={(e) => change((b) => ({ ...b, stack: e.target.value }))}
        />
      </label>
      <p className="muted bp-note">
        Click a part or a wire to set its details. Delete removes what is selected.
      </p>
    </aside>
  );
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

function TypeEditor({
  bp,
  edit,
  onClose,
  onSave,
  onDelete,
}: {
  bp: Blueprint;
  edit: TypeEdit;
  onClose: () => void;
  onSave: (t: TypeEdit) => void;
  onDelete: (t: TypeEdit) => void;
}) {
  const [t, setT] = useState(edit);
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
          if (t.value.name.trim()) onSave({ ...t, value: { ...t.value, name: t.value.name.trim() } } as TypeEdit);
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
        <div className="bp-type-grid">
          <label className="bp-field">
            <span>Name</span>
            <input autoFocus required value={t.value.name} onChange={(e) => set({ name: e.target.value })} />
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
            <fieldset className="bp-accepts">
              <legend>Can be called over</legend>
              <label className="bp-check">
                <input
                  type="checkbox"
                  checked={t.value.accepts.includes("*")}
                  onChange={(e) => set({ accepts: e.target.checked ? ["*"] : [] })}
                />
                Any connector
              </label>
              {!t.value.accepts.includes("*") &&
                connectorTypes(bp).map((c) => (
                  <label key={c.id} className="bp-check">
                    <input
                      type="checkbox"
                      checked={(t.value as PartType).accepts.includes(c.id)}
                      onChange={(e) => {
                        const a = (t.value as PartType).accepts.filter((k) => k !== c.id);
                        set({ accepts: e.target.checked ? [...a, c.id] : a });
                      }}
                    />
                    {c.name}
                  </label>
                ))}
            </fieldset>
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
        <div className="row">
          <button className="primary" type="submit">
            Save
          </button>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          {!t.isNew && (
            <button
              type="button"
              className="danger"
              disabled={used > 0}
              title={used ? `Used ${used} time${used > 1 ? "s" : ""} in this blueprint` : undefined}
              onClick={() => onDelete(t)}
            >
              Delete
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
