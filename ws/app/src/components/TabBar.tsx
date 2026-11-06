// The open files, one tab each. Unsaved text stays with its file (FileViewer's
// drafts), so switching tabs loses nothing.

import { baseName, type OpenFile } from "./Explorer";
import { hasDraft } from "./FileViewer";

/** Two tabs are the same tab when they show the same file the same way. */
export const tabKey = (f: OpenFile) => `${f.root}|${f.path}|${f.diff ? "diff" : "file"}`;

interface Props {
  tabs: OpenFile[];
  active: OpenFile | null;
  onSelect: (f: OpenFile) => void;
  onClose: (f: OpenFile) => void;
}

export function TabBar({ tabs, active, onSelect, onClose }: Props) {
  if (tabs.length === 0) return null;
  return (
    <div className="tab-bar" role="tablist" aria-label="Open files">
      {tabs.map((t) => {
        const selected = active !== null && tabKey(active) === tabKey(t);
        return (
          <div key={tabKey(t)} className={`tab${selected ? " selected" : ""}`}>
            <button
              role="tab"
              aria-selected={selected}
              className="tab-name"
              title={t.path}
              onClick={() => onSelect(t)}
              // Middle click closes, as in a browser.
              onAuxClick={(e) => e.button === 1 && onClose(t)}
            >
              {baseName(t.path)}
              {t.diff ? " (changes)" : ""}
              {!t.diff && hasDraft(t.root, t.path) && <span className="tab-dot" title="Unsaved changes"> ●</span>}
            </button>
            <button className="tab-close" aria-label={`Close ${baseName(t.path)}`} onClick={() => onClose(t)}>
              ✕
            </button>
          </div>
        );
      })}
    </div>
  );
}
