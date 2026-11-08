// A Debug Adapter Protocol client: just what the debug panel needs. The
// backend builds the program and starts the adapter (gdb, debugpy); this side
// sets breakpoints, runs, steps and reads the stack.

import { api } from "./api";
import { repeatStop, type LastStop } from "./debugStops";
import { allBreakpoints, onBreakpoints } from "./breakpoints";

export interface Frame {
  id: number;
  name: string;
  line: number;
  /** Absolute, when the adapter knows the file. */
  path?: string;
}

export interface Variable {
  name: string;
  value: string;
  type?: string;
  /** Non-zero when it has children to ask for. */
  ref: number;
}

interface Events {
  output: (text: string) => void;
  stopped: (frames: Frame[], why: string) => void;
  running: () => void;
  ended: (why?: string) => void;
}

/** `C:\proj\src\a.c` for root `C:\proj` and path `src/a.c`. */
export const absolute = (root: string, path: string) =>
  `${root.replace(/[\\/]+$/, "")}\\${path.replace(/\//g, "\\")}`;

export class DebugSession {
  private id = 0;
  private seq = 1;
  private pending = new Map<number, (response: any) => void>();
  private initialized: () => void = () => {};
  private thread = 1;
  /** How the program last went on: a continue may skip repeat stops, a step never. */
  private resumedBy: "continue" | "step" | null = null;
  /** Where it last stopped, for skipping lldb's repeat stops on one line. */
  private lastStop: LastStop | null = null;
  private unlisten: () => void = () => {};
  private done = false;

  constructor(
    private root: string,
    private on: Events,
  ) {}

  /** Build, start the adapter, set breakpoints, run. Errors end the session. */
  async start(path: string) {
    let launch: unknown;
    try {
      const started = await api.dapStart(this.root, path, (e) => {
        if (e.type === "message") this.receive(JSON.parse(e.text));
        else this.end(e.stderr.trim() ? `The debugger stopped: ${e.stderr.trim().split("\n").slice(-2).join(" ")}` : undefined);
      });
      this.id = started.id;
      launch = started.launch;
    } catch (e) {
      this.end(String(e));
      return;
    }
    const ready = new Promise<void>((r) => (this.initialized = r));
    await this.request("initialize", {
      clientID: "workspace",
      adapterID: "workspace",
      linesStartAt1: true,
      columnsStartAt1: true,
      pathFormat: "path",
    });
    // Answered only once the program runs, with some adapters: not awaited.
    void this.request("launch", launch).then((r) => {
      if (!r.success) this.end(r.message ?? "The program could not be started.");
    });
    await ready;
    await this.sendBreakpoints();
    this.unlisten = onBreakpoints(() => void this.sendBreakpoints());
    await this.request("configurationDone", {});
  }

  private async sendBreakpoints() {
    const files = allBreakpoints().filter((f) => f.root === this.root);
    await Promise.all(
      files.map((f) =>
        this.request("setBreakpoints", {
          source: { path: absolute(f.root, f.path) },
          breakpoints: f.lines.map((line) => ({ line })),
        }),
      ),
    );
  }

  private request(command: string, args: unknown): Promise<any> {
    if (this.done) return Promise.resolve({ success: false });
    const seq = this.seq++;
    return new Promise((resolve) => {
      this.pending.set(seq, resolve);
      api.bridgeSend(this.id, JSON.stringify({ seq, type: "request", command, arguments: args })).catch((e) => {
        this.pending.delete(seq);
        resolve({ success: false, message: String(e) });
      });
    });
  }

  private receive(msg: any) {
    if (msg.type === "response") {
      this.pending.get(msg.request_seq)?.(msg);
      this.pending.delete(msg.request_seq);
      return;
    }
    if (msg.type !== "event") return;
    const body = msg.body ?? {};
    switch (msg.event) {
      case "initialized":
        this.initialized();
        break;
      case "output":
        if (body.category !== "telemetry") this.on.output(body.output ?? "");
        break;
      case "continued":
        this.on.running();
        break;
      case "stopped": {
        if (body.threadId) this.thread = body.threadId;
        const resumedBy = this.resumedBy;
        this.resumedBy = null;
        void this.stack().then((frames) => {
          const top = frames[0];
          const { skip, last } = repeatStop(this.lastStop, {
            resumedBy,
            reason: body.reason,
            where: top ? `${top.path}:${top.line}` : "",
            description: body.description,
          });
          this.lastStop = last;
          if (skip) {
            this.resumedBy = "continue";
            void this.request("continue", { threadId: this.thread });
            return;
          }
          this.on.stopped(frames, body.description ?? body.reason ?? "paused");
        });
        break;
      }
      case "terminated":
      case "exited":
        this.end();
        break;
    }
  }

  private async stack(): Promise<Frame[]> {
    const r = await this.request("stackTrace", { threadId: this.thread, levels: 50 });
    return (r.body?.stackFrames ?? []).map((f: any) => ({ id: f.id, name: f.name, line: f.line, path: f.source?.path }));
  }

  /** The variables of `frame`'s first scopes (locals, usually). */
  async locals(frame: number): Promise<Variable[]> {
    const r = await this.request("scopes", { frameId: frame });
    const scopes: any[] = (r.body?.scopes ?? []).filter((s: any) => !s.expensive);
    const lists = await Promise.all(scopes.map((s) => this.children(s.variablesReference)));
    return lists.flat();
  }

  async children(ref: number): Promise<Variable[]> {
    const r = await this.request("variables", { variablesReference: ref });
    return (r.body?.variables ?? []).map((v: any) => ({ name: v.name, value: v.value, type: v.type, ref: v.variablesReference ?? 0 }));
  }

  resume() {
    this.on.running();
    this.resumedBy = "continue";
    void this.request("continue", { threadId: this.thread });
  }
  step(kind: "next" | "stepIn" | "stepOut") {
    this.on.running();
    this.resumedBy = "step";
    void this.request(kind, { threadId: this.thread });
  }
  pause() {
    void this.request("pause", { threadId: this.thread });
  }

  /** Stop the program and the adapter. */
  async stop() {
    if (this.done) return;
    await Promise.race([this.request("disconnect", { terminateDebuggee: true }), new Promise((r) => setTimeout(r, 1500))]);
    this.end();
  }

  private end(why?: string) {
    if (this.done) return;
    this.done = true;
    this.unlisten();
    for (const resolve of this.pending.values()) resolve({ success: false });
    this.pending.clear();
    if (this.id) void api.bridgeStop(this.id);
    this.on.ended(why);
  }
}
