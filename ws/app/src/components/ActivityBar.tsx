// The strip of icons down the left edge: which view fills the left pane
// (clicking the open one hides the pane), the agents, and Settings.

import { Icon } from "./Explorer";

export type LeftView = "files" | "search" | "changes";

const FILES = "M2.5 1.5h6l3 3v6h-9zM8.5 1.5v3h3M5 13.5h8.5v-8";
const SEARCH = "M7 2.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 1 1 0-9M10.3 10.3l3.7 3.7";
const CHANGES = "M4.5 2.5v8M4.5 10.5a1.8 1.8 0 1 0 0 3.6 1.8 1.8 0 1 0 0-3.6M11.5 5.5a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 1 0 0 3.6M11.5 5.5c0 3-7 2-7 5";
const AGENTS = "M5.5 2.5a2 2 0 1 1 0 4 2 2 0 1 1 0-4M1.5 13v-1.5a3 3 0 0 1 3-3h2a3 3 0 0 1 3 3V13M11 3a2 2 0 0 1 0 4M12.5 9a2.5 2.5 0 0 1 2 2.5V13";
const SETTINGS =
  "M8 5.6a2.4 2.4 0 1 1 0 4.8 2.4 2.4 0 1 1 0-4.8M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M3.4 12.6l1.1-1.1M11.5 4.5l1.1-1.1";

interface Props {
  view: LeftView | null;
  onView: (v: LeftView | null) => void;
  /** Files changed since the last commit. */
  changes: number;
  agentsOpen: boolean;
  /** Null hides the button: no agents have run in this chat. */
  onAgents: (() => void) | null;
  /** Agents waiting on an approval or an answer. */
  needYou: number;
  onSettings: () => void;
}

export function ActivityBar({ view, onView, changes, agentsOpen, onAgents, needYou, onSettings }: Props) {
  const item = (v: LeftView, d: string, label: string, badge = 0) => (
    <button
      className={`activity-item${view === v ? " selected" : ""}`}
      onClick={() => onView(view === v ? null : v)}
      aria-pressed={view === v}
      aria-label={badge ? `${label} (${badge})` : label}
      title={view === v ? `Hide ${label.toLowerCase()}` : label}
    >
      <Icon d={d} />
      {badge > 0 && <span className="activity-badge">{badge > 99 ? "99+" : badge}</span>}
    </button>
  );
  return (
    <nav className="activity-bar" aria-label="Views">
      {item("files", FILES, "Files")}
      {item("search", SEARCH, "Search")}
      {item("changes", CHANGES, "Changes", changes)}
      {onAgents && (
        <button
          className={`activity-item${agentsOpen ? " selected" : ""}`}
          onClick={onAgents}
          aria-pressed={agentsOpen}
          aria-label={needYou ? `Agents (${needYou} need you)` : "Agents"}
          title={needYou ? `${needYou} waiting for you` : "See each agent's work side by side"}
        >
          <Icon d={AGENTS} />
          {needYou > 0 && <span className="activity-badge warn">{needYou}</span>}
        </button>
      )}
      <span className="activity-spacer" />
      <button className="activity-item" onClick={onSettings} aria-label="Settings" title="Settings">
        <Icon d={SETTINGS} />
      </button>
    </nav>
  );
}
