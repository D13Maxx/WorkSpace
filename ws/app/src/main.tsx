import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { BlueprintWindow } from "./components/BlueprintFile";
import { LanguagesWindow } from "./components/Languages";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

// A blueprint window is told its file before the page loads (blueprint.rs).
const blueprint = (window as unknown as { __WORKSPACE_BLUEPRINT__?: { root: string | null; path: string | null; dir: string | null } }).__WORKSPACE_BLUEPRINT__;

// The installer's chooser window (packs.rs).
const languages = (window as unknown as { __WORKSPACE_LANGUAGES__?: boolean }).__WORKSPACE_LANGUAGES__;

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    {languages ? <LanguagesWindow /> : blueprint ? <BlueprintWindow {...blueprint} /> : <App />}
  </React.StrictMode>,
);
