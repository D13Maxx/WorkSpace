// Build the Understand-Anything /understand pipeline that ships with Workspace.
//
//   node ws/app/scripts/build-ua-pipeline.mjs [existing-UA-checkout]
//
// Output: ws/app/src-tauri/ua-pipeline (gitignored; install.ps1 copies it
// next to Workspace.exe). It holds UA's /understand skill, its agents, and its
// scripts, each bundled by esbuild into one file with UA's core library
// inside, plus the tree-sitter grammars as .wasm. Running it needs Node and
// Python only; nothing is installed on the user's machine.
//
// Packages are installed with --ignore-scripts, so no package code runs here;
// the grammars ship prebuilt .wasm files and need no build step.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const UA_TAG = "v2.9.0";
const UA_REPO = "https://github.com/Egonex-AI/Understand-Anything.git";
const PNPM = "pnpm@10.6.2";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(here, "../src-tauri/ua-pipeline");
const npx = process.platform === "win32" ? "npx.cmd" : "npx";
const run = (cmd, args, cwd) =>
  execFileSync(cmd, args, { cwd, stdio: "inherit", shell: process.platform === "win32" });

// 1. A built checkout of UA.
let repo = process.argv[2];
if (!repo) {
  repo = fs.mkdtempSync(path.join(os.tmpdir(), "ua-src-"));
  run("git", ["clone", "--depth", "1", "--branch", UA_TAG, UA_REPO, repo]);
}
const plugin = path.join(repo, "understand-anything-plugin");
const core = path.join(plugin, "packages/core");
if (!fs.existsSync(path.join(core, "dist/index.js"))) {
  run(npx, ["--yes", PNPM, "install", "--frozen-lockfile", "--ignore-scripts"], repo);
  run(npx, ["--yes", PNPM, "--filter", "@understand-anything/core", "build"], plugin);
}

fs.rmSync(out, { recursive: true, force: true });
const skillSrc = path.join(plugin, "skills/understand");
const skillOut = path.join(out, "skills/understand");
fs.mkdirSync(skillOut, { recursive: true });

// 2. The skill's own files (instructions, language notes, Python merges) and
// the agents it dispatches, as UA ships them.
for (const name of fs.readdirSync(skillSrc)) {
  if (!name.endsWith(".mjs")) {
    fs.cpSync(path.join(skillSrc, name), path.join(skillOut, name), { recursive: true });
  }
}
fs.cpSync(path.join(plugin, "agents"), path.join(out, "agents"), { recursive: true });
fs.copyFileSync(path.join(repo, "LICENSE"), path.join(out, "LICENSE"));
// The skill's pre-flight looks for these to recognise a plugin root, and for
// core's build output; the core code itself is inside each bundled script.
fs.copyFileSync(path.join(plugin, "package.json"), path.join(out, "package.json"));
fs.copyFileSync(path.join(plugin, "pnpm-workspace.yaml"), path.join(out, "pnpm-workspace.yaml"));
fs.mkdirSync(path.join(out, "packages/core/dist"), { recursive: true });
fs.writeFileSync(
  path.join(out, "packages/core/dist/index.js"),
  "// Bundled into skills/understand/*.mjs by Workspace's build-ua-pipeline.mjs.\nexport {};\n",
);

// 3. Each script, bundled. The scripts load core with a computed dynamic
// import, which a bundler cannot follow; it becomes a static import first.
const loader = /let core;\s*try \{[\s\S]*?\n\} catch \{[\s\S]*?\n\}/;
// Inside the plugin, so bare imports (graphology) resolve from its packages.
const staging = path.join(plugin, "skills", ".friday-stage");
fs.rmSync(staging, { recursive: true, force: true });
fs.mkdirSync(staging);
for (const name of fs.readdirSync(skillSrc).filter((n) => n.endsWith(".mjs"))) {
  let code = fs.readFileSync(path.join(skillSrc, name), "utf8");
  if (loader.test(code)) {
    code = code.replace(loader, "import * as core from '@understand-anything/core';");
  }
  // The banner below defines `require` for bundled CommonJS packages.
  code = code.replace(/\bconst require = createRequire\(/g, "const pluginRequire = createRequire(");
  code = code.replace(/\brequire\.resolve\(/g, "pluginRequire.resolve(");
  fs.writeFileSync(path.join(staging, name), code);
}
const entries = fs.readdirSync(staging).filter((n) => n.endsWith(".mjs"));
// UA's own lockfile pins esbuild; use that copy's API.
const pnpmStore = path.join(repo, "node_modules/.pnpm");
const esbuildDir = fs.readdirSync(pnpmStore).filter((d) => d.startsWith("esbuild@")).sort()[0];
const esbuild = createRequire(import.meta.url)(path.join(pnpmStore, esbuildDir, "node_modules/esbuild"));
await esbuild.build({
  entryPoints: entries.map((n) => path.join(staging, n)),
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node20",
  outdir: skillOut,
  outExtension: { ".js": ".mjs" },
  alias: { "@understand-anything/core": path.join(core, "dist/index.js") },
  // Bundled CommonJS packages call require(); ESM has none of its own.
  banner: {
    js: "import { createRequire as __fridayCreateRequire } from 'node:module'; const require = __fridayCreateRequire(import.meta.url);",
  },
  logLevel: "warning",
});

// 4. The grammars. Core finds each with require.resolve('<pkg>/<file>.wasm')
// from the bundle's location, so a node_modules with just package.json and
// the .wasm files is enough. web-tree-sitter looks for its own .wasm next to
// the code that loads it: the bundles.
const deps = path.join(core, "node_modules");
const grammarPackages = Object.keys(JSON.parse(fs.readFileSync(path.join(core, "package.json"))).dependencies)
  .filter((p) => p.includes("tree-sitter") && p !== "web-tree-sitter");
for (const pkg of grammarPackages) {
  const src = fs.realpathSync(path.join(deps, pkg));
  const dst = path.join(out, "node_modules", pkg);
  fs.mkdirSync(dst, { recursive: true });
  fs.copyFileSync(path.join(src, "package.json"), path.join(dst, "package.json"));
  for (const f of fs.readdirSync(src).filter((f) => f.endsWith(".wasm"))) {
    fs.copyFileSync(path.join(src, f), path.join(dst, f));
  }
}
const wts = fs.realpathSync(path.join(deps, "web-tree-sitter"));
fs.copyFileSync(path.join(wts, "web-tree-sitter.wasm"), path.join(skillOut, "web-tree-sitter.wasm"));

fs.writeFileSync(
  path.join(out, "SOURCE.txt"),
  `Understand-Anything ${UA_TAG} /understand pipeline, built by ws/app/scripts/build-ua-pipeline.mjs\nfrom ${UA_REPO}. MIT, see LICENSE.\n`,
);
fs.rmSync(staging, { recursive: true, force: true });
console.log(`built ${out}`);
