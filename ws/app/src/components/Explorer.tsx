// The open folders, each a tree that collapses, one level loaded at a time.
//
// Folders load when opened, so a huge repository costs only what is looked at.

import { useEffect, useState } from "react";
import { api, pickFile, pickFolder, type FileEntry } from "../api";

export interface OpenFile {
  root: string;
  path: string;
}

interface Props {
  folders: string[];
  onAddFolder: (folder: string) => void;
  onRemoveFolder: (folder: string) => void;
  onOpenFile: (file: OpenFile) => void;
  open: OpenFile | null;
  /** Bumped when files may have changed on disk; open folders re-list. */
  version: number;
}

const Icon = ({ d }: { d: string }) => (
  <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
    <path d={d} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" />
  </svg>
);
const FOLDER = "M1.5 3.5h4l1.5 1.5h7.5v8h-13z";
const FILE = "M3.5 1.5h6l3 3v10h-9zM9.5 1.5v3h3";
const NEW_FILE = "M3.5 1.5h6l3 3v10h-9zM9.5 1.5v3h3M8 7.5v5M5.5 10h5";

/** The last part of a path, as a folder's name. */
export function baseName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

function Folder({
  root,
  path,
  depth,
  onOpenFile,
  open,
  version,
}: {
  root: string;
  path: string;
  depth: number;
  onOpenFile: (file: OpenFile) => void;
  open: OpenFile | null;
  version: number;
}) {
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  useEffect(() => {
    setError(null);
    api
      .listDir(root, path)
      .then(setEntries)
      .catch((e) => setError(String(e)));
  }, [root, path, version]);

  if (error) return <p className="failure tree-note">{error}</p>;
  if (!entries) return <p className="muted tree-note">Loading…</p>;
  if (entries.length === 0 && depth === 1) return <p className="muted tree-note">Empty folder.</p>;

  return (
    <ul className="tree" role="group">
      {entries.map((e) => (
        <li key={e.path} role="treeitem" aria-expanded={e.is_dir ? !!expanded[e.path] : undefined}>
          <button
            className={`tree-item ${open?.root === root && e.path === open.path ? "selected" : ""}`}
            style={{ paddingLeft: `${depth * 12 + 6}px` }}
            onClick={() =>
              e.is_dir
                ? setExpanded((x) => ({ ...x, [e.path]: !x[e.path] }))
                : onOpenFile({ root, path: e.path })
            }
            title={e.path}
          >
            <span className="tree-icon" aria-hidden="true">
              {e.is_dir ? (expanded[e.path] ? "▾" : "▸") : " "}
            </span>
            {e.name}
          </button>
          {e.is_dir && expanded[e.path] && (
            <Folder
              root={root}
              path={e.path}
              depth={depth + 1}
              onOpenFile={onOpenFile}
              open={open}
              version={version}
            />
          )}
        </li>
      ))}
    </ul>
  );
}

export function Explorer({ folders, onAddFolder, onRemoveFolder, onOpenFile, open, version }: Props) {
  // Collapsed roots; a newly opened folder starts expanded.
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  // The path being typed for a new file, while that box is open.
  const [newFile, setNewFile] = useState<string | null>(null);
  const [created, setCreated] = useState(0);
  const [error, setError] = useState<string | null>(null);
  // New files go in the folder of the file in view, or the first folder.
  const target = open?.root ?? folders[0];

  const openPicked = async () => {
    const file = await pickFile();
    if (!file) return;
    const lower = file.toLowerCase();
    const root = folders.find((f) => lower.startsWith(f.replace(/[\\/]+$/, "").toLowerCase() + "\\"));
    if (root) {
      const rel = file.slice(root.replace(/[\\/]+$/, "").length + 1).replace(/\\/g, "/");
      onOpenFile({ root, path: rel });
    } else {
      // Outside every open folder: open its folder too.
      const cut = file.lastIndexOf("\\");
      onAddFolder(file.slice(0, cut));
      onOpenFile({ root: file.slice(0, cut), path: file.slice(cut + 1) });
    }
  };

  return (
    <aside className="explorer" aria-label="Files">
      <div className="explorer-bar">
        <span className="explorer-title">Folders</span>
        <button
          className="icon-button"
          onClick={async () => {
            const folder = await pickFolder();
            if (folder) onAddFolder(folder);
          }}
          title="Open a folder"
          aria-label="Open a folder"
        >
          <Icon d={FOLDER} />
        </button>
        <button className="icon-button" onClick={() => void openPicked()} title="Open a file" aria-label="Open a file">
          <Icon d={FILE} />
        </button>
        <button
          className="icon-button"
          onClick={() => setNewFile("")}
          disabled={!target}
          title={target ? `New file in ${baseName(target)}` : "Open a folder first"}
          aria-label="New file"
        >
          <Icon d={NEW_FILE} />
        </button>
      </div>

      {newFile !== null && target && (
        <form
          className="explorer-new"
          onSubmit={async (e) => {
            e.preventDefault();
            const path = newFile.trim().replace(/\\/g, "/");
            if (!path) return;
            try {
              await api.createFile(target, path);
              setNewFile(null);
              setError(null);
              setCreated((n) => n + 1);
              onOpenFile({ root: target, path });
            } catch (err) {
              setError(String(err));
            }
          }}
        >
          <input
            autoFocus
            type="text"
            value={newFile}
            placeholder={`src/main.c in ${baseName(target)}`}
            onChange={(e) => setNewFile(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setNewFile(null)}
            spellCheck={false}
            aria-label="New file path"
          />
        </form>
      )}
      {error && <p className="failure tree-note">{error}</p>}

      <div className="explorer-roots" role="tree">
        {folders.length === 0 && (
          <p className="muted tree-note">Open a folder with the folder button above.</p>
        )}
        {folders.map((root) => (
          <div key={root} className="explorer-root" role="treeitem" aria-expanded={!collapsed[root]}>
            <div className="root-row">
              <button
                className="tree-item root-name"
                onClick={() => setCollapsed((c) => ({ ...c, [root]: !c[root] }))}
                title={root}
              >
                <span className="tree-icon" aria-hidden="true">
                  {collapsed[root] ? "▸" : "▾"}
                </span>
                {baseName(root)}
              </button>
              <button
                className="icon-button root-close"
                onClick={() => onRemoveFolder(root)}
                title={`Close ${baseName(root)}`}
                aria-label={`Close ${baseName(root)}`}
              >
                ✕
              </button>
            </div>
            {!collapsed[root] && (
              <Folder
                root={root}
                path=""
                depth={1}
                onOpenFile={onOpenFile}
                open={open}
                version={version + created}
              />
            )}
          </div>
        ))}
      </div>
    </aside>
  );
}
