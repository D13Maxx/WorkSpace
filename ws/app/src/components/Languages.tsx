import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type LanguagePacks } from "../api";

/**
 * Language packs: what is on this computer, and installing what is missing
 * with winget. In Settings, and alone in the window the installer opens.
 */
export function Languages({ setup = false }: { setup?: boolean }) {
  const [packs, setPacks] = useState<LanguagePacks | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [lines, setLines] = useState<Record<string, string>>({});
  const [failed, setFailed] = useState<Record<string, string>>({});
  const [installed, setInstalled] = useState(false);

  const refresh = () =>
    api.languagePacks().then((p) => {
      setPacks(p);
      return p;
    });

  useEffect(() => {
    refresh().then((p) => {
      // Core is needed by the rest: chosen from the start when it is missing.
      const core = p.packs.find((k) => k.id === "core");
      if (core && core.parts.some((x) => !x.present)) setChosen(new Set(["core"]));
    });
    const stop = listen<{ pack: string; line: string }>("language-pack-progress", ({ payload }) =>
      setLines((l) => ({ ...l, [payload.pack]: payload.line })),
    );
    return () => {
      stop.then((f) => f());
    };
  }, []);

  if (!packs) return <p className="muted">Checking what is installed…</p>;

  const complete = (id: string) => packs.packs.find((p) => p.id === id)?.parts.every((x) => x.present) ?? true;
  const toggle = (id: string, on: boolean) =>
    setChosen((c) => {
      const next = new Set(c);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const install = async () => {
    // In the packs' order, so Core's Node.js is there before the npm-based parts.
    const order = packs.packs.map((p) => p.id).filter((id) => chosen.has(id));
    setFailed({});
    for (const id of order) {
      setBusy(id);
      try {
        await api.installLanguagePack(id);
      } catch (e) {
        setFailed((f) => ({ ...f, [id]: String(e) }));
      }
    }
    setBusy(null);
    setChosen(new Set());
    setInstalled(true);
    await refresh();
  };

  return (
    <div className="languages">
      {!packs.can_install && (
        <p className="muted">
          Workspace installs these with winget, on Windows. Here, install the programs listed with your
          package manager.
        </p>
      )}
      {packs.packs.map((pack) => {
        const done = complete(pack.id);
        return (
          <div key={pack.id} className="language-pack">
            <label className="language-head">
              <input
                type="checkbox"
                checked={done || chosen.has(pack.id)}
                disabled={done || busy !== null || !packs.can_install}
                onChange={(e) => toggle(pack.id, e.target.checked)}
              />
              <span className="language-name">{pack.name}</span>
              {done ? <span className="ok">installed</span> : busy === pack.id && <span className="muted">installing…</span>}
            </label>
            <p className="muted">{pack.about}</p>
            <ul className="language-parts">
              {pack.parts.map((part) => (
                <li key={part.name} className={part.present ? "ok" : "muted"}>
                  {part.present ? "✓" : "○"} {part.name}
                </li>
              ))}
            </ul>
            {busy === pack.id && lines[pack.id] && <p className="language-line">{lines[pack.id]}</p>}
            {failed[pack.id] && <p className="failure">{failed[pack.id]}</p>}
          </div>
        );
      })}
      {packs.can_install && (
        <div className="row">
          <button className="primary" disabled={busy !== null || chosen.size === 0} onClick={() => void install()}>
            {busy ? "Installing…" : "Install selected"}
          </button>
          {setup && (
            <button disabled={busy !== null} onClick={() => window.close()}>
              {installed || chosen.size === 0 ? "Done" : "Skip"}
            </button>
          )}
        </div>
      )}
      {packs.can_install && (
        <p className="muted">
          Some installers ask for admin rights. Workspace finds newly installed tools after you restart it.
        </p>
      )}
    </div>
  );
}

/** The window the installer opens to choose languages; closing it ends setup. */
export function LanguagesWindow() {
  return (
    <main className="languages-window">
      <h2>Choose your languages</h2>
      <p className="muted">
        Workspace is installed. Pick the languages you will use and it installs what they need: compilers,
        debuggers and language servers. You can change this later in Settings → Languages.
      </p>
      <Languages setup />
    </main>
  );
}
