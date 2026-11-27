// Bundle Reverse Engineer Anything (REA) with Workspace, for agent teams.
//
//   node ws/app/scripts/build-rea.mjs
//
// Output: ws/app/src-tauri/rea (gitignored; about 200 MB): the rea-agents npm
// package with its own dependencies, which it ships inside itself. Workspace
// starts it with the system's Node.js as `node rea/scripts/rea.mjs mcp`.
//
// Installed with --ignore-scripts, so no package code runs here, and without
// optional dependencies: frida (live process instrumentation) is a native
// add-on that downloads its own binary when installed, so it is left out.
// Also without node_modules/.bin links: on Linux they are symlinks into the
// temporary install folder, left dangling once it is deleted, and Tauri fails
// on them. Workspace starts REA with `node`, so nothing needs them.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Pinned: a new REA release is taken on purpose, after trying it.
const REA = "rea-agents@6.10.1";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(here, "../src-tauri/rea");
const npm = process.platform === "win32" ? "npm.cmd" : "npm";

const prefix = fs.mkdtempSync(path.join(os.tmpdir(), "rea-"));
execFileSync(
  npm,
  ["install", REA, "--prefix", prefix, "--omit=dev", "--omit=optional", "--ignore-scripts",
   "--no-bin-links", "--no-audit", "--no-fund", "--engine-strict=false"],
  { stdio: "inherit", shell: process.platform === "win32" },
);
const pkg = path.join(prefix, "node_modules", "rea-agents");
if (!fs.existsSync(path.join(pkg, "scripts", "rea.mjs"))) {
  throw new Error(`${REA} did not install as expected: no scripts/rea.mjs`);
}
if (!fs.existsSync(path.join(pkg, "skills", "reverse-engineer-anything", "SKILL.md"))) {
  throw new Error(`${REA} has no reverse-engineer-anything skill`);
}

fs.rmSync(out, { recursive: true, force: true });
fs.cpSync(pkg, out, { recursive: true });
fs.rmSync(prefix, { recursive: true, force: true });
const files = fs.readdirSync(out, { recursive: true }).length;
console.log(`REA (${REA}) bundled into ${out}: ${files} entries`);
