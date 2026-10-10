// The project's files, one folder at a time (read-only).
//
// Folders load when opened, so a huge repository costs only what is looked at.

import { useEffect, useState } from "react";
import { api, pickFolder, type FileEntry } from "../api";

interface Props {
  project: string;
  onProjectChange: (project: string) => void;
  onOpenFile: (path: string) => void;
  openPath: string | null;
}

function Folder({
  project,
  path,
  depth,
  onOpenFile,
  openPath,
}: {
  project: string;
  path: string;
  depth: number;
  onOpenFile: (path: string) => void;
  openPath: string | null;
}) {
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Record<string, boolean>>({});

  useEffect(() => {
    setEntries(null);
    setError(null);
    api
      .listDir(project, path)
      .then(setEntries)
      .catch((e) => setError(String(e)));
  }, [project, path]);

  if (error) return <p className="failure tree-note">{error}</p>;
  if (!entries) return <p className="muted tree-note">Loading…</p>;

  return (
    <ul className="tree" role={depth === 0 ? "tree" : "group"}>
      {entries.map((e) => (
        <li key={e.path} role="treeitem" aria-expanded={e.is_dir ? !!open[e.path] : undefined}>
          <button
            className={`tree-item ${e.path === openPath ? "selected" : ""}`}
            style={{ paddingLeft: `${depth * 12 + 6}px` }}
            onClick={() =>
              e.is_dir
                ? setOpen((o) => ({ ...o, [e.path]: !o[e.path] }))
                : onOpenFile(e.path)
            }
            title={e.path}
          >
            <span className="tree-icon" aria-hidden="true">
              {e.is_dir ? (open[e.path] ? "▾" : "▸") : " "}
            </span>
            {e.name}
          </button>
          {e.is_dir && open[e.path] && (
            <Folder
              project={project}
              path={e.path}
              depth={depth + 1}
              onOpenFile={onOpenFile}
              openPath={openPath}
            />
          )}
        </li>
      ))}
    </ul>
  );
}

export function Explorer({ project, onProjectChange, onOpenFile, openPath }: Props) {
  const [draft, setDraft] = useState(project);

  return (
    <aside className="explorer" aria-label="Files">
      <form
        className="explorer-project"
        onSubmit={(e) => {
          e.preventDefault();
          onProjectChange(draft.trim());
        }}
      >
        <input
          type="text"
          value={draft}
          placeholder={"C:\\path\\to\\your\\repo"}
          onChange={(e) => setDraft(e.target.value)}
          spellCheck={false}
          aria-label="Project folder"
        />
        <button type="submit" disabled={!draft.trim()}>
          Open
        </button>
        <button
          type="button"
          onClick={async () => {
            const folder = await pickFolder();
            if (folder) {
              setDraft(folder);
              onProjectChange(folder);
            }
          }}
          title="Choose a folder"
        >
          Browse…
        </button>
      </form>
      {project ? (
        <Folder project={project} path="" depth={0} onOpenFile={onOpenFile} openPath={openPath} />
      ) : (
        <p className="muted tree-note">Open a project folder to browse it.</p>
      )}
    </aside>
  );
}
