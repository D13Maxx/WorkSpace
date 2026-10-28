import { useCallback, useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, ask, claudeFirst, preferredModel, rememberModel, type ContextView, type ModelOption, type Thread, type TurnEvent } from "./api";
import { Sidebar } from "./components/Sidebar";
import { Composer } from "./components/Composer";
import { MessageList } from "./components/MessageList";
import { Inspector } from "./components/Inspector";
import { Settings } from "./components/Settings";
import { PrefillProgress } from "./components/PrefillProgress";
import { Explorer, baseName, type OpenFile } from "./components/Explorer";
import { GraphDialog } from "./components/GraphDialog";
import { FileViewer, moveDrafts } from "./components/FileViewer";
import { RunPanel, type RunTask } from "./components/RunPanel";

/** The chat pane's width is the user's; these only keep the panes usable. */
const MIN_CHAT = 300;
const MIN_REST = 580;

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
  // The chat pane shows either the open conversation or the list of them.
  const [showChats, setShowChats] = useState(false);
  // The folders open on the left. Every launch starts with none: folders and
  // files are not remembered, chats are (they live in the database).
  const [folders, setFolders] = useState<string[]>([]);
  const [openFile, setOpenFile] = useState<OpenFile | null>(null);
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
  const [chatWidth, setChatWidth] = useState(() => Number(readPref("ws.chatWidth")) || 420);

  useEffect(() => writePref("ws.chatWidth", String(chatWidth)), [chatWidth]);

  // The agent's file tools act on whatever is open here.
  useEffect(() => {
    void api.setWorkspace(project || null, openFile?.path ?? null);
  }, [project, openFile]);

  // Blueprint windows reload what the agent or a run changed, as this window does.
  useEffect(() => {
    if (fileVersion) void emit("files-changed");
  }, [fileVersion]);

  /** Files open in the middle; a blueprint opens in its own window. */
  const openAny = (f: OpenFile) => {
    if (!f.path.toLowerCase().endsWith(".blueprint")) return setOpenFile(f);
    api.openBlueprintWindow(f.root, f.path).catch((e) => setError(String(e)));
  };
  const openAnyRef = useRef(openAny);
  openAnyRef.current = openAny;

  const addFolder = (f: string) => {
    setFolders((fs) => (fs.includes(f) ? fs : [...fs, f]));
    setFocus(f);
  };

  const busy = streaming !== null;
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

      let text = "";
      let reasoned = "";
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
              break;
            case "approval_required":
              if (allowAll.current) {
                void api.resolveApproval(event.call_id, true);
              } else {
                const asked = { callId: event.call_id, name: event.name, args: event.args };
                setApprovals((q) => [...q, asked]);
              }
              break;
            case "tool_result":
              setStatus(null);
              setFileVersion((v) => v + 1);
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
    async (text: string) => {
      if (!active) return;
      // The model a chat is used with is the one new work should start with.
      if (thread) rememberModel(thread.backend, thread.model_id);
      await consume(active, (onEvent) => api.sendMessage(active, text, onEvent));
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
      <div className="pane-left">
        <Explorer
          folders={folders}
          onAddFolder={addFolder}
          onRemoveFolder={(f) => {
            setFolders((fs) => fs.filter((x) => x !== f));
            if (openFile?.root === f) setOpenFile(null);
            if (focus === f) setFocus(null);
          }}
          onGraph={setGraphFor}
          onNewBlueprint={() => api.openBlueprintWindow(null, null, project || null).catch((e) => setError(String(e)))}
          onTerminal={project ? () => startTask({ kind: "terminal", root: project }) : undefined}
          onRenamed={(root, from, to) => {
            moveDrafts(root, from, to);
            if (openFile?.root === root && (openFile.path === from || openFile.path.startsWith(`${from}/`))) {
              setOpenFile({ root, path: to + openFile.path.slice(from.length) });
            }
          }}
          onDeleted={(root, path) => {
            moveDrafts(root, path, null);
            if (openFile?.root === root && (openFile.path === path || openFile.path.startsWith(`${path}/`))) {
              setOpenFile(null);
            }
          }}
          onOpenFile={openAny}
          open={openFile}
          version={fileVersion}
        />
        <button className="settings" onClick={() => setShowSettings(true)}>
          Settings
        </button>
      </div>

      {/* Middle: the file being viewed. */}
      <section className="pane-middle">
        {openFile ? (
          <FileViewer
            // One editor per file, so unsaved text stays with its file.
            key={`${openFile.root}|${openFile.path}`}
            project={openFile.root}
            path={openFile.path}
            version={fileVersion}
            onClose={() => setOpenFile(null)}
            onRun={(path) => startTask({ kind: "file", root: openFile.root, path })}
          />
        ) : (
          <div className="empty">
            <p>{folders.length ? "Choose a file on the left, or make a new one." : "Open a folder on the left to start."}</p>
          </div>
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
