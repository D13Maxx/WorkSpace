// The terminal panel: shells in tabs, apart from the output panel, so running
// a file never touches them. Hidden shells keep running with their history:
// every terminal stays mounted and only the active one shows.

import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";
import { baseName } from "./Explorer";

interface Session {
  key: number;
  root: string;
  exited: boolean;
}

/** One shell: an xterm on a pseudo-terminal in the backend. */
function Shell({ root, active, onExit }: { root: string; active: boolean; onExit: () => void }) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const exitRef = useRef(onExit);
  exitRef.current = onExit;

  useEffect(() => {
    if (!host.current) return;
    const css = getComputedStyle(document.documentElement);
    const t = new Terminal({
      fontFamily: 'ui-monospace, "Cascadia Mono", Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: { background: css.getPropertyValue("--bg").trim(), foreground: css.getPropertyValue("--text").trim() },
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(host.current);
    term.current = t;
    let id: number | null = null;
    let live = true;
    const fitNow = () => {
      // A hidden terminal has no size; it fits when it shows again.
      if (!host.current?.offsetWidth) return;
      fit.fit();
      if (id !== null) void api.terminalResize(id, t.cols, t.rows);
    };
    fitNow();
    const resize = new ResizeObserver(fitNow);
    resize.observe(host.current);
    t.onData((data) => {
      if (id !== null) void api.terminalInput(id, data);
    });
    api
      .terminalOpen(root, (e) => {
        if (!live) return;
        if (e.type === "output") t.write(e.data);
        else {
          t.write(`\r\n\x1b[2m${e.stopped ? "Closed" : `Shell exited${e.code === null ? "" : ` with code ${e.code}`}`}\x1b[0m\r\n`);
          exitRef.current();
        }
      })
      .then((opened) => {
        if (!live) return void api.terminalClose(opened);
        id = opened;
        fitNow();
      })
      .catch((err) => t.write(`\x1b[31m${String(err).replace(/\r?\n/g, "\r\n")}\x1b[0m\r\n`));
    return () => {
      live = false;
      resize.disconnect();
      if (id !== null) void api.terminalClose(id);
      t.dispose();
      term.current = null;
    };
  }, [root]);

  useEffect(() => {
    if (active) term.current?.focus();
  }, [active]);

  return <div className="terminal" ref={host} hidden={!active} />;
}

interface Props {
  /** Whether the panel shows; hidden shells keep running. */
  open: boolean;
  /** Where a new shell starts: the folder in use. */
  root: string;
  /** A new value opens another shell. */
  newKey: number;
  height: number;
  onResize: (height: number) => void;
  onHide: () => void;
}

export function TerminalPanel({ open, root, newKey, height, onResize, onHide }: Props) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [active, setActive] = useState<number | null>(null);
  const next = useRef(1);

  const add = () => {
    if (!root) return;
    const key = next.current++;
    setSessions((s) => [...s, { key, root, exited: false }]);
    setActive(key);
  };
  // Showing an empty panel starts a shell, and so does each new `newKey`;
  // one effect, so asking for a new shell on a hidden panel starts just one.
  const handled = useRef(newKey);
  useEffect(() => {
    if (!open) return;
    if (newKey !== handled.current) {
      handled.current = newKey;
      add();
    } else if (sessions.length === 0) add();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, newKey]);

  const close = (key: number) => {
    const rest = sessions.filter((s) => s.key !== key);
    setSessions(rest);
    if (active === key) setActive(rest[rest.length - 1]?.key ?? null);
    if (!rest.length) onHide();
  };

  if (!sessions.length) return null;
  return (
    <section className="terminal-panel" aria-label="Terminal" style={{ height }} hidden={!open}>
      <div
        className="splitter-h"
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize the terminal"
        aria-valuenow={height}
        tabIndex={0}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          document.body.classList.add("resizing-v");
        }}
        onPointerMove={(e) => {
          if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
          const bottom = e.currentTarget.parentElement?.getBoundingClientRect().bottom ?? window.innerHeight;
          onResize(Math.round(bottom - e.clientY));
        }}
        onLostPointerCapture={() => document.body.classList.remove("resizing-v")}
        onKeyDown={(e) => {
          const step = e.key === "ArrowUp" ? 24 : e.key === "ArrowDown" ? -24 : 0;
          if (step) onResize(height + step);
        }}
      />
      <header>
        <div className="terminal-tabs" role="tablist" aria-label="Shells">
          {sessions.map((s, i) => (
            <span key={s.key} className={`terminal-tab${s.key === active ? " active" : ""}`}>
              <button role="tab" aria-selected={s.key === active} onClick={() => setActive(s.key)}>
                {i + 1}: {baseName(s.root)}
                {s.exited ? " (exited)" : ""}
              </button>
              <button aria-label={`Close shell ${i + 1}`} onClick={() => close(s.key)}>
                ✕
              </button>
            </span>
          ))}
        </div>
        <span className="actions">
          <button onClick={add} disabled={!root} title="New shell (Ctrl+Shift+`)">
            +
          </button>
          <button onClick={onHide} title="Hide; shells keep running (Ctrl+`)" aria-label="Hide terminal">
            ▾
          </button>
        </span>
      </header>
      <div className="terminal-body">
        {sessions.map((s) => (
          <Shell
            key={s.key}
            root={s.root}
            active={s.key === active}
            onExit={() => setSessions((all) => all.map((x) => (x.key === s.key ? { ...x, exited: true } : x)))}
          />
        ))}
      </div>
    </section>
  );
}
