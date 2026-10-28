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
  /** A folder was picked to make or view a knowledge graph of. */
  onGraph: (folder: string) => void;
  /** Open an untitled blueprint in its own window. */
  onNewBlueprint: () => void;
  /** Open a terminal in the current folder; absent with no folder open. */
  onTerminal?: () => void;
  /** `from` in `root` is now called `to` (relative paths). */
  onRenamed: (root: string, from: string, to: string) => void;
  /** `path` in `root` went to the Recycle Bin. */
  onDeleted: (root: string, path: string) => void;
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
const NEW_FOLDER = "M1.5 3.5h4l1.5 1.5h7.5v8h-13zM8 6.8v4.4M5.8 9h4.4";

const BLUEPRINT = "M1.5 2.5h5v4h-5zM9.5 9.5h5v4h-5zM6.5 4.5h2.5v7h.5";
const TERMINAL ="M1.5 2.5h13v11h-13zM4 6l2.5 2L4 10M8 10.5h4";
const RENAME = "M10.5 2.5l3 3-8 8h-3v-3zM9 4l3 3";
const TRASH = "M2.5 4.5h11M6 4.5v-2h4v2M4 4.5l.8 9h6.4l.8-9M6.8 7v4.5M9.2 7v4.5";

/** Rename and delete, for any row of the tree. */
interface RowEdit {
  editing: { root: string; path: string; mode: "rename" | "delete" } | null;
  begin: (root: string, path: string, mode: "rename" | "delete") => void;
  cancel: () => void;
  rename: (name: string) => void;
  remove: () => void;
}

/** A folder inside an open folder: where new files and folders go. */
interface Place {
  root: string;
  /** Relative to `root`; empty for the top. */
  dir: string;
}
// Three linked nodes.
const GRAPH = "M4 4.5a1.5 1.5 0 1 1 0 .01zM12 3.5a1.5 1.5 0 1 1 0 .01zM8 12a1.5 1.5 0 1 1 0 .01zM5.3 5.3l2 5M10.6 4.4l-2 6M5.5 4.3h5";

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
  place,
  onPlace,
  isOpen,
  onToggle,
  edit,
  version,
}: {
  root: string;
  path: string;
  depth: number;
  onOpenFile: (file: OpenFile) => void;
  open: OpenFile | null;
  place: Place | null;
  onPlace: (place: Place) => void;
  isOpen: (path: string) => boolean;
  onToggle: (path: string) => void;
  edit: RowEdit;
  version: number;
}) {
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

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
        <li key={e.path} role="treeitem" aria-expanded={e.is_dir ? isOpen(e.path) : undefined}>
          {edit.editing?.root === root && edit.editing.path === e.path && edit.editing.mode === "rename" ? (
            <form
              className="tree-rename"
              style={{ paddingLeft: `${depth * 12 + 6}px` }}
              onSubmit={(ev) => {
                ev.preventDefault();
                const input = ev.currentTarget.elements.namedItem("name") as HTMLInputElement;
                edit.rename(input.value);
              }}
            >
              <input
                name="name"
                autoFocus
                defaultValue={e.name}
                // The name without its extension, ready to retype.
                onFocus={(ev) => {
                  const dot = e.is_dir ? -1 : e.name.lastIndexOf(".");
                  ev.currentTarget.setSelectionRange(0, dot > 0 ? dot : e.name.length);
                }}
                onKeyDown={(ev) => ev.key === "Escape" && edit.cancel()}
                onBlur={edit.cancel}
                spellCheck={false}
                aria-label={`New name for ${e.name}`}
              />
            </form>
          ) : (
            <div className="tree-row">
            <button
              className={`tree-item ${
                (e.is_dir ? place?.root === root && place.dir === e.path : open?.root === root && e.path === open.path)
                  ? "selected"
                  : ""
              }`}
              style={{ paddingLeft: `${depth * 12 + 6}px` }}
              onClick={() => {
                if (e.is_dir) {
                  onToggle(e.path);
                  onPlace({ root, dir: e.path });
                } else {
                  onOpenFile({ root, path: e.path });
                }
              }}
              title={e.path}
            >
              <span className="tree-icon" aria-hidden="true">
                {e.is_dir ? (isOpen(e.path) ? "▾" : "▸") : " "}
              </span>
              {e.name}
            </button>
              <span className="tree-actions">
                <button
                  className="icon-button"
                  onClick={() => edit.begin(root, e.path, "rename")}
                  title={`Rename ${e.name}`}
                  aria-label={`Rename ${e.name}`}
                >
                  <Icon d={RENAME} />
                </button>
                <button
                  className="icon-button"
                  onClick={() => edit.begin(root, e.path, "delete")}
                  title={`Delete ${e.name}`}
                  aria-label={`Delete ${e.name}`}
                >
                  <Icon d={TRASH} />
                </button>
              </span>
            </div>
          )}
          {edit.editing?.root === root && edit.editing.path === e.path && edit.editing.mode === "delete" && (
            <div className="tree-confirm" role="alertdialog" aria-label={`Delete ${e.name}`}>
              <span>
                Move <strong>{e.name}</strong>
                {e.is_dir ? " and everything in it" : ""} to the Recycle Bin?
              </span>
              <span className="row">
                <button className="danger" autoFocus onClick={edit.remove}>
                  Delete
                </button>
                <button onClick={edit.cancel}>Cancel</button>
              </span>
            </div>
          )}
          {e.is_dir && isOpen(e.path) && (
            <Folder
              root={root}
              path={e.path}
              depth={depth + 1}
              onOpenFile={onOpenFile}
              open={open}
              place={place}
              onPlace={onPlace}
              isOpen={isOpen}
              onToggle={onToggle}
              edit={edit}
              version={version}
            />
          )}
        </li>
      ))}
    </ul>
  );
}

export function Explorer({
  folders,
  onAddFolder,
  onRemoveFolder,
  onOpenFile,
  onGraph,
  onNewBlueprint,
  onTerminal,
  onRenamed,
  onDeleted,
  open,
  version,
}: Props) {
  // Collapsed roots; a newly opened folder starts expanded.
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  // Expanded folders inside the roots, by root and path.
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const key = (root: string, path: string) => `${root}\n${path}`;
  /** Show `p`: its root and every folder on the way to it. */
  const expandTo = (p: Place) => {
    setCollapsed((c) => ({ ...c, [p.root]: false }));
    const parts = p.dir ? p.dir.split("/") : [];
    setExpanded((x) => {
      const next = { ...x };
      parts.forEach((_, i) => {
        next[key(p.root, parts.slice(0, i + 1).join("/"))] = true;
      });
      return next;
    });
  };
  // What is being created, and its name as typed, while that box is open.
  const [creating, setCreating] = useState<null | "file" | "folder">(null);
  const [name, setName] = useState("");
  const [created, setCreated] = useState(0);
  const [error, setError] = useState<string | null>(null);
  // The folder last clicked. New things go there, else beside the file in
  // view, else at the top of the first folder.
  const [picked, setPicked] = useState<Place | null>(null);
  const place: Place | null =
    picked && folders.includes(picked.root)
      ? picked
      : open
        ? { root: open.root, dir: open.path.includes("/") ? open.path.slice(0, open.path.lastIndexOf("/")) : "" }
        : folders[0]
          ? { root: folders[0], dir: "" }
          : null;
  const where = place ? (place.dir ? `${baseName(place.root)}/${place.dir}` : baseName(place.root)) : "";

  /** The open folder holding `folder`, or `folder` itself as a new root. */
  const locate = (folder: string): Place => {
    const lower = folder.replace(/[\\/]+$/, "").toLowerCase();
    for (const root of folders) {
      const r = root.replace(/[\\/]+$/, "");
      if (lower === r.toLowerCase()) return { root, dir: "" };
      if (lower.startsWith(r.toLowerCase() + "\\")) {
        return { root, dir: folder.slice(r.length + 1).replace(/[\\/]+$/, "").replace(/\\/g, "/") };
      }
    }
    return { root: folder, dir: "" };
  };

  /** Ask where, in Windows' folder picker, then ask for the name. */
  const startCreate = async (kind: "file" | "folder") => {
    const start = place
      ? place.root.replace(/[\\/]+$/, "") + (place.dir ? "\\" + place.dir.replace(/\//g, "\\") : "")
      : undefined;
    const folder = await pickFolder(start, `Where should the new ${kind} go?`);
    if (!folder) return;
    const p = locate(folder);
    if (!folders.includes(p.root)) onAddFolder(p.root);
    setPicked(p);
    expandTo(p);
    setName("");
    setError(null);
    setCreating(kind);
  };

  const [editing, setEditing] = useState<RowEdit["editing"]>(null);
  const edit: RowEdit = {
    editing,
    begin: (root, path, mode) => {
      setError(null);
      setEditing({ root, path, mode });
    },
    cancel: () => setEditing(null),
    rename: async (name) => {
      if (!editing) return;
      const { root, path } = editing;
      const old = path.split("/").pop();
      setEditing(null);
      if (!name.trim() || name.trim() === old) return;
      try {
        const to = await api.renamePath(root, path, name);
        onRenamed(root, path, to);
        setPicked(null);
        setCreated((n) => n + 1);
      } catch (err) {
        setError(String(err));
      }
    },
    remove: async () => {
      if (!editing) return;
      const { root, path } = editing;
      setEditing(null);
      try {
        await api.deletePath(root, path);
        onDeleted(root, path);
        setPicked(null);
        setCreated((n) => n + 1);
      } catch (err) {
        setError(String(err));
      }
    },
  };

  // Opening a file moves the place for new things to beside it.
  const openFromTree = (file: OpenFile) => {
    setPicked(null);
    onOpenFile(file);
  };

  const create = async () => {
    const typed = name.trim().replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
    if (!place || !typed) return;
    const path = place.dir ? `${place.dir}/${typed}` : typed;
    try {
      if (creating === "folder") {
        await api.createDir(place.root, path);
        setPicked({ root: place.root, dir: path });
        expandTo({ root: place.root, dir: path });
      } else {
        await api.createFile(place.root, path);
        onOpenFile({ root: place.root, path });
      }
      setCreating(null);
      setName("");
      setError(null);
      setCreated((n) => n + 1);
    } catch (err) {
      setError(String(err));
    }
  };

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
          onClick={() => void startCreate("file")}
          title="New file (asks where)"
          aria-label="New file"
        >
          <Icon d={NEW_FILE} />
        </button>
        <button
          className="icon-button"
          onClick={() => void startCreate("folder")}
          title="New folder (asks where)"
          aria-label="New folder"
        >
          <Icon d={NEW_FOLDER} />
        </button>
        <button
          className="icon-button"
          onClick={onNewBlueprint}
          title="New blueprint: draw a system in the Workshop, simulate it, have the agent build it (saved only when you save it)"
          aria-label="New blueprint"
        >
          <Icon d={BLUEPRINT} />
        </button>
        <button
          className="icon-button"
          onClick={async () => {
            const folder = await pickFolder();
            if (folder) onGraph(folder);
          }}
          title="Knowledge graph of a folder"
          aria-label="Knowledge graph"
        >
          <Icon d={GRAPH} />
        </button>
        <button
          className="icon-button"
          onClick={onTerminal}
          disabled={!onTerminal}
          title={onTerminal ? "Terminal (pip, npm, cargo…)" : "Open a folder first"}
          aria-label="Terminal"
        >
          <Icon d={TERMINAL} />
        </button>
      </div>

      {creating && place && (
        <form
          className="explorer-new"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <label className="muted">
            New {creating} in <strong>{where}</strong>
          </label>
          <input
            autoFocus
            type="text"
            value={name}
            placeholder={creating === "file" ? "main.c" : "src"}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setCreating(null);
                setName("");
              }
            }}
            spellCheck={false}
            aria-label={`New ${creating} name`}
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
                className={`tree-item root-name ${place?.root === root && place.dir === "" && picked ? "selected" : ""}`}
                onClick={() => {
                  setCollapsed((c) => ({ ...c, [root]: !c[root] }));
                  setPicked({ root, dir: "" });
                }}
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
                onOpenFile={openFromTree}
                open={open}
                place={picked ? place : null}
                onPlace={setPicked}
                edit={edit}
                isOpen={(path) => !!expanded[key(root, path)]}
                onToggle={(path) => setExpanded((x) => ({ ...x, [key(root, path)]: !x[key(root, path)] }))}
                version={version + created}
              />
            )}
          </div>
        ))}
      </div>
    </aside>
  );
}
