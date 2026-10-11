// The output panel: a real terminal for a program being built and run, a
// shell, or a pip install.
//
// It is a terminal, not a log: what the user types here goes to the program,
// so `input()`, `scanf` and an interactive shell all work.

import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api, type RunEvent } from "../api";
import { baseName } from "./Explorer";

export type RunTask =
  | { kind: "file"; root: string; path: string }
  | { kind: "terminal"; root: string }
  | { kind: "pip"; root: string; package: string };

/** Import names whose pip package is called something else. */
const PIP_NAMES: Record<string, string> = {
  cv2: "opencv-python",
  PIL: "pillow",
  sklearn: "scikit-learn",
  yaml: "pyyaml",
  bs4: "beautifulsoup4",
  dotenv: "python-dotenv",
  docx: "python-docx",
  fitz: "pymupdf",
  serial: "pyserial",
  Crypto: "pycryptodome",
};

/** The package a failed Python run is missing, from its output. */
export function missingPackage(output: string): string | null {
  const m = /ModuleNotFoundError: No module named '([A-Za-z0-9_]+)/.exec(output);
  return m ? (PIP_NAMES[m[1]] ?? m[1]) : null;
}

function label(task: RunTask): string {
  switch (task.kind) {
    case "file":
      return task.path;
    case "terminal":
      return `Terminal in ${baseName(task.root)}`;
    case "pip":
      return `pip install ${task.package}`;
  }
}

function start(task: RunTask, onEvent: (e: RunEvent) => void): Promise<void> {
  switch (task.kind) {
    case "file":
      return api.runFile(task.root, task.path, onEvent);
    case "terminal":
      return api.openTerminal(task.root, onEvent);
    case "pip":
      return api.pipInstall(task.root, task.package, onEvent);
  }
}

interface Props {
  task: RunTask;
  /** A new value starts a new run. */
  runKey: number;
  onExit: (task: RunTask, code: number | null) => void;
  /** Install a package the last Python run was missing. */
  onInstall: (pkg: string) => void;
  onClose: () => void;
}

export function RunPanel({ task, runKey, onExit, onInstall, onClose }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const [running, setRunning] = useState(false);
  const [missing, setMissing] = useState<string | null>(null);
  const latest = useRef({ task, onExit });
  latest.current = { task, onExit };

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
    const current = latest.current.task;
    let live = true;
    // The tail of the output, for spotting a missing Python package.
    let tail = "";
    t.reset();
    setRunning(true);
    setMissing(null);
    start(current, (e) => {
      if (!live) return;
      if (e.type === "output") {
        t.write(e.data);
        tail = (tail + e.data).slice(-8000);
        return;
      }
      setRunning(false);
      const how = e.stopped
        ? "Stopped"
        : e.code === null
          ? "Did not finish"
          : `Exited with code ${e.code}`;
      t.write(`\r\n\x1b[2m${how}\x1b[0m\r\n`);
      if (current.kind === "file" && current.path.endsWith(".py") && e.code !== 0) {
        // eslint-disable-next-line no-control-regex
        setMissing(missingPackage(tail.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "")));
      }
      latest.current.onExit(current, e.code);
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
  }, [runKey]);

  return (
    <section className="run-panel" aria-label="Output">
      <header>
        <span>
          Output <code>{label(task)}</code>
        </span>
        <span className="actions">
          {missing && (
            <button
              className="primary"
              onClick={() => onInstall(missing)}
              title={`Run pip install ${missing}`}
            >
              Install {missing}
            </button>
          )}
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
