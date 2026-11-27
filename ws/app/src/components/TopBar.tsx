// The thin bar along the top: the folder in use, its git branch, and a box
// that opens the finder (files, symbols, text, commands).

import { Icon } from "./Explorer";

const BRANCH = "M4.5 2.5v8M4.5 10.5a1.8 1.8 0 1 0 0 3.6 1.8 1.8 0 1 0 0-3.6M11.5 5.5a1.8 1.8 0 1 0 0-3.6 1.8 1.8 0 1 0 0 3.6M11.5 5.5c0 3-7 2-7 5";

interface Props {
  /** The folder in use; empty with none open. */
  project: string;
  /** Its checked-out branch; empty when it is not a repo or no branch is out. */
  branch: string;
  onBranch: () => void;
  onFind: () => void;
}

export function TopBar({ project, branch, onBranch, onFind }: Props) {
  const name = project.split(/[\\/]/).filter(Boolean).pop() ?? "";
  return (
    <header className="top-bar">
      <span className="top-project" title={project || undefined}>
        {name || "Workspace"}
      </span>
      {branch && (
        <button className="top-branch" onClick={onBranch} title="Branch: open Changes to push, pull or switch">
          <Icon d={BRANCH} />
          {branch}
        </button>
      )}
      <button className="top-find" onClick={onFind} title="Find a file, a symbol (@), text (/) or a command (>)">
        <span>Search files, symbols, commands…</span>
        <kbd>Ctrl+P</kbd>
      </button>
    </header>
  );
}
