import { useCallback, useEffect, useRef, useState } from "react";
import { api, type ContextView, type ModelOption, type Thread, type TurnEvent } from "./api";
import { Sidebar } from "./components/Sidebar";
import { Composer } from "./components/Composer";
import { MessageList } from "./components/MessageList";
import { Inspector } from "./components/Inspector";
import { Settings } from "./components/Settings";

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
  const [error, setError] = useState<string | null>(null);

  const [inspecting, setInspecting] = useState<string | null>(null);
  const [inspectorView, setInspectorView] = useState<ContextView | null>(null);
  const [showSettings, setShowSettings] = useState(false);

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
              setStatus(`Reading prompt: ${event.done_tokens} / ${event.total_tokens} tokens`);
              break;
            case "text_delta":
              text += event.text;
              setStreaming(text);
              break;
            case "reasoning_delta":
              reasoned += event.text;
              setReasoning(reasoned);
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
        await refreshThread(conversation);
        await refreshConversations();
      }
    },
    [refreshThread, refreshConversations],
  );

  const onSend = useCallback(
    async (text: string) => {
      if (!active) return;
      await consume(active, (onEvent) => api.sendMessage(active, text, onEvent));
    },
    [active, consume],
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
    const model = models[0];
    const id = await api.createConversation(
      "New conversation",
      model?.backend ?? ANTHROPIC,
      model?.model_id ?? "claude-sonnet-4-5",
    );
    await refreshConversations();
    setActive(id);
  }, [models, refreshConversations]);

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
      await refreshThread(active);
    },
    [active, refreshThread],
  );

  return (
    <div className="app">
      <Sidebar
        conversations={conversations}
        active={active}
        onSelect={setActive}
        onNew={onNewConversation}
        onDelete={async (id) => {
          await api.deleteConversation(id);
          if (active === id) setActive(null);
          await refreshConversations();
        }}
        onRename={async (id, title) => {
          await api.renameConversation(id, title);
          await refreshConversations();
        }}
        onOpenSettings={() => setShowSettings(true)}
      />

      <main className="main">
        {thread ? (
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
              busy={busy}
            />
            <div ref={bottomRef} />

            {status && <div className="status-banner">{status}</div>}
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
