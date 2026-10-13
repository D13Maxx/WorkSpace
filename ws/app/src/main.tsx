import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { BlueprintWindow } from "./components/BlueprintFile";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

// A blueprint window is told its file before the page loads (blueprint.rs).
const blueprint = (window as unknown as { __WORKSPACE_BLUEPRINT__?: { root: string | null; path: string | null; dir: string | null } }).__WORKSPACE_BLUEPRINT__;

ReactDOM.createRoot(root).render(
  <React.StrictMode>{blueprint ? <BlueprintWindow {...blueprint} /> : <App />}</React.StrictMode>,
);
