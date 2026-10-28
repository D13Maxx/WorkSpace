import { useState } from "react";
import { ask, type ConversationSummary } from "../api";

interface Props {
  conversations: ConversationSummary[];
  active: string | null;
  onSelect: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}

export function Sidebar(props: Props) {
  const [query, setQuery] = useState("");
  const shown = props.conversations.filter((c) =>
    c.title.toLowerCase().includes(query.toLowerCase()),
  );

  return (
    <aside className="sidebar">
      <div className="sidebar-top">
        <button className="primary" onClick={props.onNew}>
          New conversation
        </button>
        <input
          placeholder="Search…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="Search conversations"
        />
      </div>

      <ul className="conversation-list">
        {shown.map((c) => (
          <li key={c.id} className={c.id === props.active ? "active" : ""}>
            <button className="conversation" onClick={() => props.onSelect(c.id)}>
              <span className="title">{c.title}</span>
              <span className="model">{c.model_id}</span>
            </button>
            <div className="row-actions">
              <button
                title="Rename"
                onClick={() => {
                  const next = window.prompt("Rename conversation", c.title);
                  if (next) props.onRename(c.id, next);
                }}
              >
                ✎
              </button>
              <button
                title="Delete"
                onClick={async () => {
                  // Deleting a conversation is the only destructive operation
                  // in the product, so it always confirms (master spec s5.1).
                  if (await ask(`Delete "${c.title}"? This cannot be undone.`)) {
                    props.onDelete(c.id);
                  }
                }}
              >
                ✕
              </button>
            </div>
          </li>
        ))}
        {shown.length === 0 && <li className="muted">No conversations</li>}
      </ul>

    </aside>
  );
}
