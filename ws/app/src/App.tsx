import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, ask, claudeFirst, pickFolder, preferredModel, rememberModel, type ContextView, type HelperLog, type ModelOption, type Thread, type TurnEvent } from "./api";
import { Sidebar } from "./components/Sidebar";
import { Composer } from "./components/Composer";
import { MessageList } from "./components/MessageList";
import { Inspector } from "./components/Inspector";
import { Settings } from "./components/Settings";
import { PrefillProgress } from "./components/PrefillProgress";
import { Explorer, baseName, type OpenFile } from "./components/Explorer";
import { GraphDialog } from "./components/GraphDialog";
import { FileViewer, moveDrafts } from "./components/FileViewer";
import { SidePanel } from "./components/SidePanel";
import { TabBar, tabKey } from "./components/TabBar";
import { RunPanel, type RunTask } from "./components/RunPanel";
import { DebugPanel } from "./components/DebugPanel";
import { setFileOpener, stopLanguageServers } from "./lsp";
import { Finder, type Command } from "./components/Finder";
import { TerminalPanel } from "./components/TerminalPanel";
import { lineBlameEnabled, setLineBlameEnabled } from "./git";
import { setVimEnabled, setWindowCommands, vimEnabled } from "./vim";

/** The chat pane's width is the user's; these only keep the panes usable. */
const MIN_CHAT = 300;
const MIN_REST = 580;
const MIN_SIDE = 160;
/** More splits than this leave each too narrow to use. */
const MAX_SPLITS = 4;

interface Group {
  /** Stable, so closing one split does not rebuild the others' editors. */
  id: number;
  tabs: OpenFile[];
  open: OpenFile | null;
}
/** The editor keeps at least this much between the side panel and the chat. */
const MIN_EDITOR = 300;

/** Per-viewer UI preferences. Storage can be unavailable; the app works without it. */
function readPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not remembered, which is fine.
  }
}

/** Tool arguments as readable fields: code in blocks with real line breaks. */
function ToolArgs({ args }: { args: string }) {
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

const ANTHROPIC = "anthropic";

export default function App() {
  const [conversations, setConversations] = useState<Awaited<ReturnType<typeof api.listConversations>>>([]);
  const [active, setActive] = useState<string | null>(null);
  const [thread, setThread] = useState<Thread | null>(null);
  const [models, setModels] = useState<ModelOption[]>([]);

  // While a turn streams, the reply lives here rather than in `thread`: the
  // message row does not exist until it completes (master spec s10).
  const [streaming, setStreaming] = useState<string | null>(null);
  const [reasoning, setReasoning] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  // Helpers' work as it streams in; the saved logs replace it once they finish.
  const [liveHelpers, setLiveHelpers] = useState<Record<string, HelperLog>>({});
  const helperLogs = useMemo(() => {
    const logs = new Map(Object.entries(liveHelpers));
    for (const log of thread?.helpers ?? []) logs.set(log.dispatch, log);
    return logs;
  }, [liveHelpers, thread]);
  // Local engine only: how much of the prompt has been read so far.
  const [prefill, setPrefill] = useState<{
    reused: number;
    done: number;
    total: number;
    startedAt: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A tool call waiting for the user. The turn is paused until it is answered.
  // Helper agents run side by side, so several can wait at once: a queue.
  const [approvals, setApprovals] = useState<{ callId: string; name: string; args: string }[]>(
    [],
  );
  // "Allow all for this reply": later requests in the same reply are allowed unasked.
  const allowAll = useRef(false);
  const approval = approvals[0] ?? null;
  const answer = (callId: string, approved: boolean) => {
    void api.resolveApproval(callId, approved);
    setApprovals((q) => q.filter((a) => a.callId !== callId));
  };

  const [inspecting, setInspecting] = useState<string | null>(null);
  const [inspectorView, setInspectorView] = useState<ContextView | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  // The finder, open with what its box starts with ("", "@", "/" or ">").
  const [finder, setFinder] = useState<string | null>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey) return;
      // By key position: Shift turns ` into ~ on most layouts.
      if (e.code === "Backquote") {
        e.preventDefault();
        if (e.shiftKey) terminalRef.current.newTerminal();
        else terminalRef.current.toggleTerminal();
        return;
      }
      const k = e.key.toLowerCase();
      if (k === "\\" && !e.shiftKey) {
        e.preventDefault();
        splitsRef.current.splitEditor("row");
        return;
      }
      const start =
        k === "p" ? (e.shiftKey ? ">" : "") : k === "t" && !e.shiftKey ? "@" : k === "f" && e.shiftKey ? "/" : null;
      if (start === null) return;
      // Before the editor: Vim's own Ctrl+P must not swallow it.
      e.preventDefault();
      e.stopPropagation();
      setFinder(start);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);
  // The chat pane shows either the open conversation or the list of them.
  const [showChats, setShowChats] = useState(false);
  // The folders open on the left. Every launch starts with none: folders and
  // files are not remembered, chats are (they live in the database).
  const [folders, setFolders] = useState<string[]>([]);
  // The editor splits, each with its tabs and the one showing. A file is open
  // in one split at most: two editors on it would each keep unsaved text.
  const [groups, setGroups] = useState<Group[]>(() => [{ id: 0, tabs: [], open: null }]);
  const [focused, setFocused] = useState(0);
  const [splitDir, setSplitDir] = useState<"row" | "column">("row");
  const nextGroupId = useRef(1);
  const focusedRef = useRef(focused);
  focusedRef.current = focused;
  const group = groups[Math.min(focused, groups.length - 1)];
  const openFile = group.open;
  // Set by keyboard moves between splits, which also move the typing focus.
  const focusEditorNext = useRef(false);
  useEffect(() => {
    if (!focusEditorNext.current) return;
    focusEditorNext.current = false;
    document.querySelectorAll<HTMLElement>(".editor-group")[focused]?.querySelector<HTMLElement>(".cm-content")?.focus();
  }, [focused, groups]);
  // A tab's unsaved dot follows its editor, which App cannot see.
  const [, redrawTabs] = useReducer((n: number) => n + 1, 0);
  // The folder last opened, when no file says otherwise.
  const [focus, setFocus] = useState<string | null>(null);
  // The folder the agent and Run work in.
  const project = openFile?.root ?? focus ?? folders[0] ?? "";
  // The folder whose knowledge-graph choices are showing.
  const [graphFor, setGraphFor] = useState<string | null>(null);
  // Bumped when the agent or a run may have changed files, so views reload.
  const [fileVersion, setFileVersion] = useState(0);
  // What the output panel is running: a file, a terminal or a pip install.
  const [run, setRun] = useState<{ task: RunTask; key: number } | null>(null);
  const startTask = (task: RunTask) => setRun((r) => ({ task, key: (r?.key ?? 0) + 1 }));
  // The file being debugged; a new key starts a new session.
  const [debug, setDebug] = useState<{ root: string; path: string; key: number } | null>(null);
  const [chatWidth, setChatWidth] = useState(() => Number(readPref("ws.chatWidth")) || 420);

  useEffect(() => writePref("ws.chatWidth", String(chatWidth)), [chatWidth]);
  // The terminal panel: shown or not (its shells run either way), a counter
  // that asks it for another shell, and its height.
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [newShell, setNewShell] = useState(0);
  const [termHeight, setTermHeight] = useState(() => Number(readPref("ws.termHeight")) || 240);
  useEffect(() => writePref("ws.termHeight", String(termHeight)), [termHeight]);
  const toggleTerminal = () => setTerminalOpen((o) => !o);
  const newTerminal = () => {
    setTerminalOpen(true);
    setNewShell((n) => n + 1);
  };
  const terminalRef = useRef({ toggleTerminal, newTerminal });
  terminalRef.current = { toggleTerminal, newTerminal };
  const [sideWidth, setSideWidth] = useState(() => Number(readPref("ws.sideWidth")) || 260);
  useEffect(() => writePref("ws.sideWidth", String(sideWidth)), [sideWidth]);
  const clampSide = (w: number) =>
    Math.round(Math.max(MIN_SIDE, Math.min(window.innerWidth - chatWidth - MIN_EDITOR, w)));

  // The agent's file tools act on whatever is open here.
  useEffect(() => {
    void api.setWorkspace(project || null, openFile?.path ?? null);
  }, [project, openFile]);

  // Blueprint windows reload what the agent or a run changed, as this window does.
  useEffect(() => {
    if (fileVersion) void emit("files-changed");
  }, [fileVersion]);

  /** Change every split: renames, deletes and closed folders reach them all. */
  const eachGroup = (fn: (g: Group) => Group) => setGroups((gs) => gs.map(fn));
  /** Show `f` (or nothing) in the focused split, without adding a tab. */
  const setOpenFile = (f: OpenFile | null) =>
    setGroups((gs) => gs.map((g, i) => (i === Math.min(focusedRef.current, gs.length - 1) ? { ...g, open: f } : g)));
  /** Files open in the middle, in the split that has them, else the focused
   *  one; a blueprint opens in its own window. */
  const show = (f: OpenFile) => {
    const at = groups.findIndex((g) => g.tabs.some((t) => tabKey(t) === tabKey(f)));
    const gi = at >= 0 ? at : Math.min(focused, groups.length - 1);
    setFocused(gi);
    setGroups((gs) =>
      gs.map((g, i) =>
        i !== gi ? g : { ...g, tabs: g.tabs.some((t) => tabKey(t) === tabKey(f)) ? g.tabs : [...g.tabs, f], open: f },
      ),
    );
  };
  /** Close a tab; the one beside it shows if it was the one showing. */
  const closeTab = (f: OpenFile, gi = focused) =>
    setGroups((gs) =>
      gs.map((g, j) => {
        if (j !== gi) return g;
        const i = g.tabs.findIndex((t) => tabKey(t) === tabKey(f));
        const rest = g.tabs.filter((t) => tabKey(t) !== tabKey(f));
        const open = g.open && tabKey(g.open) === tabKey(f) ? (rest[Math.min(i, rest.length - 1)] ?? null) : g.open;
        return { ...g, tabs: rest, open };
      }),
    );
  /** A new, empty split beside (row) or below (column) the focused one. */
  const splitEditor = (dir: "row" | "column") => {
    if (groups.length >= MAX_SPLITS) return;
    const at = Math.min(focused, groups.length - 1) + 1;
    setSplitDir(dir);
    setGroups((gs) => [...gs.slice(0, at), { id: nextGroupId.current++, tabs: [], open: null }, ...gs.slice(at)]);
    setFocused(at);
    focusEditorNext.current = true;
  };
  /** Close a split; its tabs move to the split next to it, so nothing is lost. */
  const closeSplit = (gi = focused) => {
    if (groups.length < 2) return;
    const into = gi > 0 ? gi - 1 : 1;
    setGroups((gs) =>
      gs
        .map((g, j) =>
          j !== into
            ? g
            : { ...g, tabs: [...g.tabs, ...gs[gi].tabs.filter((t) => !g.tabs.some((u) => tabKey(u) === tabKey(t)))], open: g.open ?? gs[gi].open },
        )
        .filter((_, j) => j !== gi),
    );
    setFocused(into > gi ? into - 1 : into);
    focusEditorNext.current = true;
  };
  /** Move to the next (1) or previous (-1) split. */
  const focusSplit = (by: number) => {
    setFocused((f) => (f + by + groups.length) % groups.length);
    focusEditorNext.current = true;
  };
  /** Keep only the focused split, its neighbours' tabs joining it. */
  const onlySplit = () => {
    const keep = Math.min(focused, groups.length - 1);
    setGroups((gs) => {
      const mine = gs[keep];
      const others = gs.flatMap((g, j) => (j === keep ? [] : g.tabs));
      const tabs = [...mine.tabs, ...others.filter((t, i) => !mine.tabs.some((u) => tabKey(u) === tabKey(t)) && others.findIndex((u) => tabKey(u) === tabKey(t)) === i)];
      return [{ ...mine, tabs }];
    });
    setFocused(0);
  };
  const splitsRef = useRef({ splitEditor, closeSplit, focusSplit, onlySplit });
  splitsRef.current = { splitEditor, closeSplit, focusSplit, onlySplit };
  useEffect(
    () =>
      setWindowCommands({
        split: (dir) => splitsRef.current.splitEditor(dir),
        close: () => splitsRef.current.closeSplit(),
        focus: (by) => splitsRef.current.focusSplit(by),
        only: () => splitsRef.current.onlySplit(),
        terminal: () => terminalRef.current.newTerminal(),
      }),
    [],
  );
  const openAny = (f: OpenFile) => {
    if (!f.path.toLowerCase().endsWith(".blueprint")) return show(f);
    api.openBlueprintWindow(f.root, f.path).catch((e) => setError(String(e)));
  };
  const openAnyRef = useRef(openAny);
  openAnyRef.current = openAny;
  // Go to definition and references open other files in tabs.
  useEffect(() => setFileOpener((root, path) => openAnyRef.current({ root, path })), []);

  const addFolder = (f: string) => {
    setFolders((fs) => (fs.includes(f) ? fs : [...fs, f]));
    setFocus(f);
  };

  const busy = streaming !== null;

  const commands: Command[] = [
    { label: "Open folder", run: () => void pickFolder().then((f) => f && addFolder(f)) },
    { label: "New chat", run: () => void onNewConversation() },
    { label: "Settings", run: () => setShowSettings(true) },
    { label: "Find file", run: () => setFinder("") },
    { label: "Go to symbol", run: () => setFinder("@") },
    { label: "Search in files", run: () => setFinder("/") },
    { label: vimEnabled() ? "Turn off Vim keys" : "Turn on Vim keys", run: () => setVimEnabled(!vimEnabled()) },
    {
      label: lineBlameEnabled() ? "Hide line blame" : "Show line blame",
      run: () => setLineBlameEnabled(!lineBlameEnabled()),
    },
    ...(openFile ? [{ label: "Close tab", run: () => closeTab(openFile) }] : []),
    { label: terminalOpen ? "Hide terminal" : "Show terminal", run: toggleTerminal },
    ...(project ? [{ label: "New terminal", run: newTerminal }] : []),
    { label: "Split editor right", run: () => splitEditor("row") },
    { label: "Split editor down", run: () => splitEditor("column") },
    ...(groups.length > 1
      ? [
          { label: "Close split", run: () => closeSplit() },
          { label: "Focus next split", run: () => focusSplit(1) },
          { label: "Keep only this split", run: () => onlySplit() },
        ]
      : []),
  ];
  const bottomRef = useRef<HTMLDivElement>(null);

  const refreshModels = useCallback(() => {
    api
      .listModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  const refreshConversations = useCallback(async () => {
    try {
      setConversations(await api.listConversations());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const refreshThread = useCallback(async (id: string) => {
    try {
      setThread(await api.loadThread(id));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refreshConversations();
    refreshModels();
  }, [refreshConversations, refreshModels]);

  useEffect(() => {
    if (active) void refreshThread(active);
    else setThread(null);
  }, [active, refreshThread]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [thread?.messages.length, streaming]);

  useEffect(() => {
    if (!inspecting) {
      setInspectorView(null);
      return;
    }
    api.getContextView(inspecting).then(setInspectorView).catch(() => setInspectorView(null));
  }, [inspecting]);

  /** Shared handler for both send and regenerate. */
  const consume = useCallback(
    async (conversation: string, run: (onEvent: (e: TurnEvent) => void) => Promise<void>) => {
      setStreaming("");
      setReasoning("");
      setError(null);
      setStatus(null);
      setPrefill(null);
      setLiveHelpers({});

      let text = "";
      let reasoned = "";
      // choose_location calls, so the folder picked for the agent shows in the explorer.
      const choosing = new Set<string>();
      try {
        await run((event) => {
          switch (event.type) {
            case "preparing_context":
              setStatus(`Preparing context for ${event.target.model_id}…`);
              break;
            case "summarising":
              setStatus(
                `Summarising ${event.covering} earlier messages with ${event.writer.model_id}…`,
              );
              break;
            case "context_ready":
              setStatus(null);
              break;
            case "prefill_progress":
              setStatus(null);
              setPrefill((previous) => ({
                reused: event.reused_tokens,
                done: event.done_tokens,
                total: event.total_tokens,
                // Keep the original start, or the estimate restarts from zero
                // on every update and never settles.
                startedAt: previous?.startedAt ?? performance.now(),
              }));
              break;
            case "text_delta":
              // The first word means prefill is over.
              setPrefill(null);
              text += event.text;
              setStreaming(text);
              break;
            case "reasoning_delta":
              reasoned += event.text;
              setReasoning(reasoned);
              break;
            case "tool_call":
              setStatus(`Running ${event.name}…`);
              if (event.name.endsWith("choose_location")) choosing.add(event.call_id);
              break;
            case "approval_required":
              if (allowAll.current) {
                void api.resolveApproval(event.call_id, true);
              } else {
                const asked = { callId: event.call_id, name: event.name, args: event.args };
                setApprovals((q) => [...q, asked]);
              }
              break;
            case "helper_step":
              setLiveHelpers((all) => {
                const log = all[event.dispatch] ?? { dispatch: event.dispatch, agent: event.agent, entries: [] };
                return { ...all, [event.dispatch]: { ...log, entries: [...log.entries, event.entry] } };
              });
              break;
            case "tool_result":
              setStatus(null);
              setFileVersion((v) => v + 1);
              if (choosing.has(event.call_id) && !event.is_error) {
                void api.workspaceFolder().then((f) => f && addFolder(f));
              }
              break;
            case "completed":
              // One user message can produce several model replies with tool
              // calls between them; show each as it lands.
              text = "";
              setStreaming("");
              void refreshThread(conversation);
              break;
            case "failed":
              setError(event.error);
              break;
            default:
              break;
          }
        });
      } catch (e) {
        setError(String(e));
      } finally {
        setStreaming(null);
        setReasoning("");
        setStatus(null);
        setPrefill(null);
        setApprovals([]);
        allowAll.current = false;
        await refreshThread(conversation);
        await refreshConversations();
      }
    },
    [refreshThread, refreshConversations],
  );

  const onSend = useCallback(
    async (text: string, coordinate: boolean) => {
      if (!active) return;
      // The model a chat is used with is the one new work should start with.
      if (thread) rememberModel(thread.backend, thread.model_id);
      await consume(active, (onEvent) => api.sendMessage(active, text, onEvent, coordinate));
    },
    [active, consume, thread],
  );

  const onRegenerate = useCallback(
    async (message: string) => {
      if (!active) return;
      await consume(active, (onEvent) => api.regenerate(active, message, onEvent));
    },
    [active, consume],
  );

  const onStop = useCallback(() => {
    if (active) void api.cancelTurn(active);
  }, [active]);

  const onNewConversation = useCallback(async () => {
    const model = preferredModel(models, (m) => m[0]);
    const id = await api.createConversation(
      "New conversation",
      model?.backend ?? ANTHROPIC,
      model?.model_id ?? "claude-sonnet-4-5",
    );
    await refreshConversations();
    setActive(id);
  }, [models, refreshConversations]);

  /** Send `message` to the agent in a new chat working in `folder`, with the
   *  model last picked (or a Claude when `claude`, which /understand needs). */
  const startAgent = async (folder: string, file: string | null, title: string, message: string, claude = false) => {
    const model = claude ? claudeFirst(models) : preferredModel(models, claudeFirst);
    try {
      const id = await api.createConversation(
        title,
        model?.backend ?? ANTHROPIC,
        model?.model_id ?? "claude-sonnet-4-5",
      );
      await refreshConversations();
      setActive(id);
      setShowChats(false);
      // Before the turn starts: the effect that tracks the open folder runs later.
      await api.setWorkspace(folder, file);
      await consume(id, (onEvent) => api.sendMessage(id, message, onEvent));
    } catch (e) {
      setError(String(e));
    }
  };

  // Closing this window closes Workspace, blueprint windows too: ask about unsaved ones first.
  useEffect(() => {
    const off = getCurrentWindow().onCloseRequested(async (e) => {
      const n = await api.unsavedBlueprints().catch(() => 0);
      const what = n === 1 ? "A blueprint has" : `${n} blueprints have`;
      if (n && !(await ask(`${what} unsaved changes. Close Workspace anyway?`))) e.preventDefault();
    });
    return () => void off.then((f) => f());
  }, []);

  // Show `root` on the left: in the open folder holding it, or as a folder of its own.
  const showFolder = (root: string) => {
    const at = root.replace(/[\\/]+$/, "").toLowerCase();
    setFolders((fs) =>
      fs.some((f) => {
        const r = f.replace(/[\\/]+$/, "").toLowerCase();
        return at === r || at.startsWith(r + "\\");
      })
        ? fs
        : [...fs, root],
    );
  };

  // Blueprint windows: a saved file (new ones show on the left, and an open
  // source rereads), a file's source, and a part's code.
  useEffect(() => {
    const offs = [
      listen<{ root: string; path: string }>("blueprint-saved", ({ payload }) => {
        showFolder(payload.root);
        setFileVersion((v) => v + 1);
      }),
      listen<{ root: string; path: string }>("blueprint-source", ({ payload }) => {
        showFolder(payload.root);
        setOpenFile(payload);
        void getCurrentWindow().setFocus();
      }),
      listen<{ root: string; path: string }>("open-code", ({ payload }) => {
        void getCurrentWindow().setFocus();
        // A folder opens on the left; a file opens here.
        // shortcut: a folder already inside an open one shows again as its own root; expand it in place once the tree can be driven from here.
        api
          .listDir(payload.root, payload.path)
          .then(() => addFolder(`${payload.root.replace(/[\\/]+$/, "")}\\${payload.path.replace(/\//g, "\\")}`))
          .catch(() => {
            showFolder(payload.root);
            openAnyRef.current(payload);
          });
      }),
    ];
    return () => offs.forEach((off) => void off.then((f) => f()));
    // Once: the handlers read what changes through state setters and openAnyRef.
  }, []);

  // A blueprint window's Build with agent: the chat is here.
  const startAgentRef = useRef(startAgent);
  startAgentRef.current = startAgent;
  useEffect(() => {
    const off = listen<{ root: string; path: string; prompt: string }>("blueprint-build", ({ payload }) => {
      void getCurrentWindow().setFocus();
      void startAgentRef.current(payload.root, payload.path, `Build: ${baseName(payload.path)}`, payload.prompt);
    });
    return () => void off.then((f) => f());
  }, []);

  /** Build `folder`'s AI graph: open it, then run /understand in a new chat
   *  with a Claude model, which is what the pipeline needs. */
  const startUnderstand = (folder: string) => {
    setGraphFor(null);
    addFolder(folder);
    setOpenFile(null);
    void startAgent(folder, null, `Knowledge graph: ${baseName(folder)}`, "/understand", true);
  };

  const onSelectBranch = useCallback(
    async (message: string) => {
      if (!active) return;
      await api.selectBranch(active, message);
      await refreshThread(active);
    },
    [active, refreshThread],
  );

  const onSwitchModel = useCallback(
    async (backend: string, modelId: string) => {
      if (!active) return;
      await api.switchModel(active, backend, modelId);
      rememberModel(backend, modelId);
      await refreshThread(active);
    },
    [active, refreshThread],
  );

  return (
    <div className="app">
      {/* Left: the open folder. */}
      <div className="pane-left" style={{ width: sideWidth, flexBasis: sideWidth }}>
        <Explorer
          folders={folders}
          onAddFolder={addFolder}
          onRemoveFolder={(f) => {
            setFolders((fs) => fs.filter((x) => x !== f));
            stopLanguageServers(f);
            eachGroup((g) => ({ ...g, tabs: g.tabs.filter((t) => t.root !== f), open: g.open?.root === f ? null : g.open }));
            if (focus === f) setFocus(null);
          }}
          onGraph={setGraphFor}
          onNewBlueprint={() => api.openBlueprintWindow(null, null, project || null).catch((e) => setError(String(e)))}
          onTerminal={project ? () => (terminalOpen ? newTerminal() : setTerminalOpen(true)) : undefined}
          onRenamed={(root, from, to) => {
            moveDrafts(root, from, to);
            const moved = (t: OpenFile) =>
              t.root === root && (t.path === from || t.path.startsWith(`${from}/`)) ? { ...t, path: to + t.path.slice(from.length) } : t;
            eachGroup((g) => ({ ...g, tabs: g.tabs.map(moved), open: g.open && moved(g.open) }));
          }}
          onDeleted={(root, path) => {
            moveDrafts(root, path, null);
            const gone = (t: OpenFile) => t.root === root && (t.path === path || t.path.startsWith(`${path}/`));
            eachGroup((g) => ({ ...g, tabs: g.tabs.filter((t) => !gone(t)), open: g.open && gone(g.open) ? null : g.open }));
          }}
          onOpenFile={openAny}
          open={openFile}
          version={fileVersion}
        />
        <SidePanel root={project} version={fileVersion} onOpen={openAny} />
        <button className="settings" onClick={() => setShowSettings(true)}>
          Settings
        </button>
      </div>

      {/* Middle: the file being viewed. */}
      <div
        className="splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the side panel"
        aria-valuenow={sideWidth}
        tabIndex={0}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          document.body.classList.add("resizing");
        }}
        onPointerMove={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId)) setSideWidth(clampSide(e.clientX));
        }}
        onLostPointerCapture={() => document.body.classList.remove("resizing")}
        onKeyDown={(e) => {
          const step = e.key === "ArrowLeft" ? -24 : e.key === "ArrowRight" ? 24 : 0;
          if (step) setSideWidth((w) => clampSide(w + step));
        }}
      />

      <section className="pane-middle">
        <div className={`editor-groups ${splitDir}`}>
          {groups.map((g, gi) => (
            <div
              key={g.id}
              className={`editor-group${groups.length > 1 && gi === focused ? " focused" : ""}`}
              onMouseDownCapture={() => setFocused(gi)}
            >
              <div className="group-bar">
                <TabBar
                  tabs={g.tabs}
                  active={g.open}
                  onSelect={(f) => {
                    setFocused(gi);
                    setGroups((gs) => gs.map((x, j) => (j === gi ? { ...x, open: f } : x)));
                  }}
                  onClose={(f) => closeTab(f, gi)}
                />
                <span className="group-actions">
                  <button onClick={() => splitEditor("row")} disabled={groups.length >= MAX_SPLITS} title="Split right (Ctrl+\)" aria-label="Split right">
                    ◫
                  </button>
                  <button onClick={() => splitEditor("column")} disabled={groups.length >= MAX_SPLITS} title="Split down" aria-label="Split down">
                    ⊟
                  </button>
                  {groups.length > 1 && (
                    <button onClick={() => closeSplit(gi)} title="Close this split; its tabs move next door" aria-label="Close split">
                      ✕
                    </button>
                  )}
                </span>
              </div>
              {g.open ? (
                <FileViewer
                  // One editor per file, so unsaved text stays with its file.
                  key={tabKey(g.open)}
                  project={g.open.root}
                  path={g.open.path}
                  diff={g.open.diff}
                  line={g.open.line}
                  lineKey={g.open}
                  onDirtyChange={redrawTabs}
                  version={fileVersion}
                  onClose={() => g.open && closeTab(g.open, gi)}
                  onRun={(path) => g.open && startTask({ kind: "file", root: g.open.root, path })}
                  onDebug={(path) => g.open && setDebug((d) => ({ root: g.open!.root, path, key: (d?.key ?? 0) + 1 }))}
                />
              ) : (
                <div className="empty">
                  <p>
                    {!folders.length
                      ? "Open a folder on the left to start."
                      : groups.length > 1
                        ? "Open a file here: Ctrl+P, or choose one on the left."
                        : "Choose a file on the left, or make a new one."}
                  </p>
                </div>
              )}
            </div>
          ))}
        </div>
        {debug && (
          <DebugPanel
            root={debug.root}
            path={debug.path}
            runKey={debug.key}
            onOpen={openAny}
            onClose={() => setDebug(null)}
          />
        )}
        {run && (
          <RunPanel
            task={run.task}
            runKey={run.key}
            // A build leaves new files (a program, a PDF) for the tree to show.
            onExit={() => setFileVersion((v) => v + 1)}
            onInstall={(pkg) => startTask({ kind: "pip", root: run.task.root, package: pkg })}
            onClose={() => setRun(null)}
          />
        )}
        <TerminalPanel
          open={terminalOpen}
          root={project}
          newKey={newShell}
          height={termHeight}
          onResize={(h) => setTermHeight(Math.max(120, Math.min(window.innerHeight - 200, h)))}
          onHide={() => setTerminalOpen(false)}
        />
      </section>

      <div
        className="splitter"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the chat"
        aria-valuenow={chatWidth}
        tabIndex={0}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          // No text gets selected while dragging.
          document.body.classList.add("resizing");
        }}
        onPointerMove={(e) => {
          if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
          const max = window.innerWidth - MIN_REST;
          setChatWidth(Math.round(Math.max(MIN_CHAT, Math.min(max, window.innerWidth - e.clientX))));
        }}
        onLostPointerCapture={() => document.body.classList.remove("resizing")}
        onKeyDown={(e) => {
          const step = e.key === "ArrowLeft" ? 24 : e.key === "ArrowRight" ? -24 : 0;
          if (step) {
            setChatWidth((w) => Math.max(MIN_CHAT, Math.min(window.innerWidth - MIN_REST, w + step)));
          }
        }}
      />

      {/* Right: the agent. */}
      <main className="main pane-right" style={{ width: chatWidth }}>
        <div className="chat-bar">
          <button onClick={() => setShowChats(!showChats)}>
            {showChats ? "Back to chat" : "Chats"}
          </button>
          {/* The chat list has its own New button. */}
          {!showChats && (
            <button onClick={() => void onNewConversation()}>New chat</button>
          )}
        </div>
        {showChats ? (
          <Sidebar
            conversations={conversations}
            active={active}
            onSelect={(id) => {
              setActive(id);
              setShowChats(false);
            }}
            onNew={() => {
              setShowChats(false);
              void onNewConversation();
            }}
            onDelete={async (id) => {
              await api.deleteConversation(id);
              if (active === id) setActive(null);
              await refreshConversations();
            }}
            onRename={async (id, title) => {
              await api.renameConversation(id, title);
              await refreshConversations();
            }}
          />
        ) : thread ? (
          <>
            <header className="thread-header">
              <h1>{thread.title}</h1>
              <select
                value={`${thread.backend}/${thread.model_id}`}
                disabled={busy}
                onChange={(e) => {
                  const [backend, ...rest] = e.target.value.split("/");
                  void onSwitchModel(backend, rest.join("/"));
                }}
              >
                <option value={`${thread.backend}/${thread.model_id}`}>
                  {thread.backend} / {thread.model_id}
                </option>
                {Object.entries(
                  models
                    .filter(
                      (m) =>
                        `${m.backend}/${m.model_id}` !==
                        `${thread.backend}/${thread.model_id}`,
                    )
                    .reduce<Record<string, ModelOption[]>>((groups, m) => {
                      (groups[m.backend] ??= []).push(m);
                      return groups;
                    }, {}),
                ).map(([backend, group]) => (
                  <optgroup key={backend} label={backend}>
                    {group.map((m) => (
                      <option
                        key={`${m.backend}/${m.model_id}`}
                        value={`${m.backend}/${m.model_id}`}
                      >
                        {m.display_name}
                        {m.unprofiled ? " (unprofiled)" : ""}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
            </header>

            <MessageList
              messages={thread.messages}
              helpers={helperLogs}
              streaming={streaming}
              streamingReasoning={reasoning}
              onRegenerate={onRegenerate}
              onInspect={setInspecting}
              onSelectBranch={onSelectBranch}
              onUndo={async (callId) => {
                try {
                  await api.undoEdit(callId);
                } catch (e) {
                  // A refused undo explains itself: the file changed since.
                  setError(String(e));
                }
                if (active) await refreshThread(active);
              }}
              busy={busy}
            />
            <div ref={bottomRef} />

            {prefill && (
              <PrefillProgress
                reusedTokens={prefill.reused}
                doneTokens={prefill.done}
                totalTokens={prefill.total}
                startedAt={prefill.startedAt}
              />
            )}
            {status && <div className="status-banner">{status}</div>}
            {approval && (
              <div className="approval" role="alertdialog" aria-label="Approve tool call">
                <p>
                  The model wants to run <code>{approval.name}</code>:
                  {approvals.length > 1 && (
                    <span className="muted"> ({approvals.length - 1} more waiting)</span>
                  )}
                </p>
                <ToolArgs args={approval.args} />
                <div className="row">
                  <button className="primary" onClick={() => answer(approval.callId, true)}>
                    Allow
                  </button>
                  <button onClick={() => answer(approval.callId, false)}>Deny</button>
                  <button
                    title="Allow this and every other tool call until this reply finishes"
                    onClick={() => {
                      allowAll.current = true;
                      for (const a of approvals) void api.resolveApproval(a.callId, true);
                      setApprovals([]);
                    }}
                  >
                    Allow all for this reply
                  </button>
                </div>
              </div>
            )}
            {error && (
              <div className="error-banner" role="alert">
                {error}
              </div>
            )}

            <Composer onSend={onSend} onStop={onStop} busy={busy} />
          </>
        ) : (
          <div className="empty">
            <p>No conversation selected.</p>
            <button onClick={onNewConversation}>New conversation</button>
          </div>
        )}
      </main>

      {graphFor && (
        <GraphDialog
          folder={graphFor}
          onClose={() => setGraphFor(null)}
          onOpenSettings={() => setShowSettings(true)}
          onAddFolder={addFolder}
          onStartAi={startUnderstand}
        />
      )}
      {inspecting && (
        <Inspector
          view={inspectorView}
          onClose={() => setInspecting(null)}
        />
      )}
      {finder !== null && (
        <Finder
          folders={folders}
          initial={finder}
          commands={commands}
          onOpen={openAny}
          onClose={() => setFinder(null)}
        />
      )}

      {showSettings && (
        <Settings
          onClose={() => setShowSettings(false)}
          models={models}
          onModelsChanged={refreshModels}
        />
      )}
    </div>
  );
}
