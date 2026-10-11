import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { BlueprintWindow } from "./components/BlueprintFile";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

// A blueprint window is told its file before the page loads (blueprint.rs).
const blueprint = (window as unknown as { __FRIDAY_BLUEPRINT__?: { root: string; path: string } }).__FRIDAY_BLUEPRINT__;

ReactDOM.createRoot(root).render(
  <React.StrictMode>{blueprint ? <BlueprintWindow {...blueprint} /> : <App />}</React.StrictMode>,
);
