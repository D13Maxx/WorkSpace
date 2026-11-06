// The debugger, under the editor: run controls, the call stack and the
// variables of the selected frame. Breakpoints are set in the editor's gutter.

import { useEffect, useRef, useState } from "react";
import { DebugSession, type Frame, type Variable } from "../dap";
import type { OpenFile } from "./Explorer";

interface Props {
  root: string;
  path: string;
  /** A new value starts a new session. */
  runKey: number;
  onOpen: (file: OpenFile) => void;
  onClose: () => void;
}

/** `src/a.c` for `C:\proj\src\a.c` in root `C:\proj`; null outside it. */
function relative(root: string, abs: string): string | null {
  const r = root.replace(/[\\/]+$/, "").replace(/\\/g, "/").toLowerCase();
  const a = abs.replace(/\\/g, "/");
  return a.toLowerCase().startsWith(`${r}/`) ? a.slice(r.length + 1) : null;
}

export function DebugPanel({ root, path, runKey, onOpen, onClose }: Props) {
  const session = useRef<DebugSession | null>(null);
  const [state, setState] = useState<"starting" | "running" | "paused" | "ended">("starting");
  const [why, setWhy] = useState<string | null>(null);
  const [frames, setFrames] = useState<Frame[]>([]);
  const [frame, setFrame] = useState<number | null>(null);
  const [vars, setVars] = useState<Variable[]>([]);
  const [output, setOutput] = useState("");
  const latest = useRef({ root, onOpen });
  latest.current = { root, onOpen };

  const show = (f: Frame) => {
    setFrame(f.id);
    const rel = f.path ? relative(latest.current.root, f.path) : null;
    if (rel) latest.current.onOpen({ root: latest.current.root, path: rel, line: f.line });
    session.current
      ?.locals(f.id)
      .then(setVars)
      .catch(() => setVars([]));
  };
  const showRef = useRef(show);
  showRef.current = show;

  useEffect(() => {
    setState("starting");
    setWhy(null);
    setFrames([]);
    setVars([]);
    setOutput("");
    const s = new DebugSession(root, {
      output: (text) => setOutput((o) => (o + text).slice(-20_000)),
      running: () => setState("running"),
      stopped: (fs, reason) => {
        setState("paused");
        setWhy(reason);
        setFrames(fs);
        if (fs[0]) showRef.current(fs[0]);
      },
      ended: (reason) => {
        setState("ended");
        setWhy(reason ?? "The program finished.");
        setFrames([]);
        setVars([]);
      },
    });
    session.current = s;
    setState("running");
    void s.start(path);
    return () => void s.stop();
  }, [root, path, runKey]);

  const s = session.current;
  const paused = state === "paused";
  return (
    <section className="run-panel debug-panel" aria-label="Debugger">
      <header>
        <span>
          Debugging <code>{path}</code> <span className="muted">· {state === "paused" ? `paused: ${why}` : state}</span>
        </span>
        <span className="actions">
          <button className="primary" disabled={!paused} onClick={() => s?.resume()}>
            Continue
          </button>
          <button disabled={!paused} onClick={() => s?.step("next")} title="Step over">
            Over
          </button>
          <button disabled={!paused} onClick={() => s?.step("stepIn")} title="Step into">
            Into
          </button>
          <button disabled={!paused} onClick={() => s?.step("stepOut")} title="Step out">
            Out
          </button>
          <button disabled={state !== "running"} onClick={() => s?.pause()}>
            Pause
          </button>
          {state === "ended" ? (
            <button onClick={onClose} aria-label="Close the debugger">
              ✕
            </button>
          ) : (
            <button className="danger" onClick={() => void s?.stop()}>
              Stop
            </button>
          )}
        </span>
      </header>
      {state === "ended" && why && <p className={why === "The program finished." ? "muted debug-note" : "failure debug-note"}>{why}</p>}
      <div className="debug-body">
        <div className="debug-col">
          <h3>Call stack</h3>
          <ul className="side-list">
            {frames.map((f) => (
              <li key={f.id}>
                <button className={f.id === frame ? "selected" : ""} onClick={() => show(f)} title={f.path}>
                  <span className="side-text">{f.name}</span>
                  <span className="side-where">{f.line}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
        <div className="debug-col">
          <h3>Variables</h3>
          <Vars vars={vars} session={s} />
        </div>
        <pre className="debug-output" aria-label="Program output">
          {output || <span className="muted">Output from the debugger and the program shows here. A C or C++ program runs in its own console window.</span>}
        </pre>
      </div>
    </section>
  );
}

function Vars({ vars, session }: { vars: Variable[]; session: DebugSession | null }) {
  return (
    <ul className="debug-vars">
      {vars.map((v, i) => (
        <Var key={`${v.name}:${i}`} v={v} session={session} />
      ))}
    </ul>
  );
}

function Var({ v, session }: { v: Variable; session: DebugSession | null }) {
  const [kids, setKids] = useState<Variable[] | null>(null);
  return (
    <li>
      <button
        disabled={!v.ref}
        onClick={() => (kids ? setKids(null) : session?.children(v.ref).then(setKids))}
        title={v.type}
        aria-expanded={v.ref ? kids !== null : undefined}
      >
        <span className="tree-icon">{v.ref ? (kids ? "▾" : "▸") : " "}</span>
        <span className="var-name">{v.name}</span> = <span className="var-value">{v.value}</span>
      </button>
      {kids && <Vars vars={kids} session={session} />}
    </li>
  );
}
