// The project's files, one folder at a time.
//
// Folders load when opened, so a huge repository costs only what is looked at.

import { useEffect, useState } from "react";
import { api, pickFile, pickFolder, type FileEntry } from "../api";

interface Props {
  project: string;
  onProjectChange: (project: string) => void;
  onOpenFile: (path: string) => void;
  openPath: string | null;
  /** Bumped when files may have changed on disk; open folders re-list. */
  version: number;
}

function Folder({
  project,
  path,
  depth,
  onOpenFile,
  openPath,
  version,
}: {
  project: string;
  path: string;
  depth: number;
  onOpenFile: (path: string) => void;
  openPath: string | null;
  version: number;
}) {
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Record<string, boolean>>({});

  useEffect(() => {
    setError(null);
    api
      .listDir(project, path)
      .then(setEntries)
      .catch((e) => setError(String(e)));
  }, [project, path, version]);

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
              version={version}
            />
          )}
        </li>
      ))}
    </ul>
  );
}

export function Explorer({ project, onProjectChange, onOpenFile, openPath, version }: Props) {
  const [draft, setDraft] = useState(project);
  // The path being typed for a new file, while that box is open.
  const [newFile, setNewFile] = useState<string | null>(null);
  const [created, setCreated] = useState(0);
  const [error, setError] = useState<string | null>(null);

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
          title="Choose a folder to open on the left"
        >
          Browse…
        </button>
      </form>
      <div className="explorer-actions">
        {newFile === null ? (
          <div className="row">
            <button
              onClick={async () => {
                const file = await pickFile();
                if (!file) return;
                // Inside the open folder: just open it. Elsewhere: open its folder too.
                const root = project.replace(/[\\/]+$/, "");
                const inside =
                  root && file.toLowerCase().startsWith(root.toLowerCase() + "\\");
                if (inside) {
                  onOpenFile(file.slice(root.length + 1).replace(/\\/g, "/"));
                } else {
                  const cut = file.lastIndexOf("\\");
                  setDraft(file.slice(0, cut));
                  onProjectChange(file.slice(0, cut));
                  onOpenFile(file.slice(cut + 1));
                }
              }}
              title="Choose any file to open"
            >
              Open file…
            </button>
            {project && (
              <button onClick={() => setNewFile("")} title="Create a file in this folder">
                New file
              </button>
            )}
          </div>
        ) : (
            <form
              onSubmit={async (e) => {
                e.preventDefault();
                const path = newFile.trim().replace(/\\/g, "/");
                if (!path) return;
                try {
                  await api.createFile(project, path);
                  setNewFile(null);
                  setError(null);
                  setCreated((n) => n + 1);
                  onOpenFile(path);
                } catch (err) {
                  setError(String(err));
                }
              }}
            >
              <input
                autoFocus
                type="text"
                value={newFile}
                placeholder="src/main.c"
                onChange={(e) => setNewFile(e.target.value)}
                onKeyDown={(e) => e.key === "Escape" && setNewFile(null)}
                spellCheck={false}
                aria-label="New file path"
              />
              <button type="submit" disabled={!newFile.trim()}>
                Create
              </button>
            </form>
          )}
        {error && <p className="failure tree-note">{error}</p>}
      </div>
      {project ? (
        <Folder
          project={project}
          path=""
          depth={0}
          onOpenFile={onOpenFile}
          openPath={openPath}
          version={version + created}
        />
      ) : (
        <p className="muted tree-note">Open a project folder to browse it.</p>
      )}
    </aside>
  );
}
