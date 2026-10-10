// The output panel: a terminal for the program being built and run.
//
// It is a real terminal, not a log: what the user types here goes to the
// program, so `input()` and `scanf` work.

import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";

interface Props {
  project: string;
  path: string;
  /** A new value starts a new run. */
  runKey: number;
  onExit: (path: string, code: number | null) => void;
  onClose: () => void;
}

export function RunPanel({ project, path, runKey, onExit, onClose }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const [running, setRunning] = useState(false);
  const exitRef = useRef(onExit);
  exitRef.current = onExit;

  useEffect(() => {
    if (!host.current) return;
    const css = getComputedStyle(document.documentElement);
    const t = new Terminal({
      fontFamily: 'ui-monospace, "Cascadia Mono", Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: {
        background: css.getPropertyValue("--bg").trim(),
        foreground: css.getPropertyValue("--text").trim(),
      },
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(host.current);
    fit.fit();
    t.onData((data) => void api.runInput(data));
    const resize = new ResizeObserver(() => {
      fit.fit();
      void api.runResize(t.cols, t.rows);
    });
    resize.observe(host.current);
    term.current = t;
    return () => {
      resize.disconnect();
      t.dispose();
      term.current = null;
    };
  }, []);

  useEffect(() => {
    const t = term.current;
    if (!t) return;
    let live = true;
    t.reset();
    setRunning(true);
    api
      .runFile(project, path, (e) => {
        if (!live) return;
        if (e.type === "output") {
          t.write(e.data);
          return;
        }
        setRunning(false);
        const how = e.stopped
          ? "Stopped"
          : e.code === null
            ? "Did not finish"
            : `Exited with code ${e.code}`;
        t.write(`\r\n\x1b[2m${how}\x1b[0m\r\n`);
        exitRef.current(path, e.code);
      })
      .then(() => api.runResize(t.cols, t.rows))
      .catch((err) => {
        if (!live) return;
        setRunning(false);
        t.write(`\x1b[31m${String(err).replace(/\r?\n/g, "\r\n")}\x1b[0m\r\n`);
      });
    t.focus();
    return () => {
      live = false;
    };
  }, [project, path, runKey]);

  return (
    <section className="run-panel" aria-label="Output">
      <header>
        <span>
          Output <code>{path}</code>
        </span>
        <span className="actions">
          <button onClick={() => void api.stopRun()} disabled={!running}>
            Stop
          </button>
          <button
            onClick={() => {
              void api.stopRun();
              onClose();
            }}
            aria-label="Close output"
          >
            ✕
          </button>
        </span>
      </header>
      <div className="terminal" ref={host} />
    </section>
  );
}
