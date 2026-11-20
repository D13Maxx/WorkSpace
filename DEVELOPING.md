# Developing Workspace

Building, testing and releasing the app, and how the code is laid out.

## Build from source

Rust stable (1.85+) and Node 20+. On Windows, Visual Studio Build Tools with the
C++ workload and the WebView2 runtime (already on Windows 11). On Linux:

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

Then:

```bash
cd ws/app
npm install
npm run tauri dev
```

An installer (`.exe` on Windows, `.deb` on Linux), with the Understand-Anything
pipeline and Reverse Engineer Anything bundled first:

```bash
cd ws/app
node scripts/build-ua-pipeline.mjs
node scripts/build-rea.mjs
npm run tauri build -- --bundles nsis   # or: --bundles deb
```

On Windows, `powershell -ExecutionPolicy Bypass -File ws/app/scripts/install.ps1`
builds and installs a copy into `%LOCALAPPDATA%\Programs\Workspace` with a
desktop shortcut, for working on the app itself.

### Releasing

Push a tag such as `v0.2.0` (after setting the same version in
`ws/app/src-tauri/tauri.conf.json`). GitHub Actions builds the Windows installer
and the `.deb` and attaches them to a draft release; check it and publish.

## Test

```bash
cd ws
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Two suites are gated behind features because they touch real systems and must
never run in CI:

```bash
# Writes to the real credential store.
cargo test -p ws-secrets --features live-tests

# Talks to real provider APIs; needs a key configured in the app first.
cargo test -p ws-providers --features live-tests
```

Test the model and the parts builder with
`node ws/app/scripts/test-blueprint.mjs` and `node ws/app/scripts/test-builder.mjs`;
the Markdown preview's sanitizer with `node ws/app/scripts/test-markdown.mjs`.

## The local engine on its own

The engine also runs without the app, which is the easier place to diagnose a
model that will not load:

```bash
cargo run -p ws-engine-cli -- inspect path/to/model.gguf
```

```bash
cargo run -p ws-engine-cli -- chat path/to/model.gguf --system "Be brief."
```

`chat` reports prefill and decode rates per turn, and how much of the prompt
came from the KV cache rather than being read again.

## The Understand-Anything pipeline

The pipeline lives in `ws/app/src-tauri/ua-pipeline` (gitignored, 38 MB). It
is built by `node ws/app/scripts/build-ua-pipeline.mjs`, which clones UA
v2.9.0, installs with scripts disabled, and bundles each script with esbuild;
`install.ps1` builds it when missing and copies it next to `Workspace.exe`;
the release installers carry it as a bundled resource.

## Reverse Engineer Anything

`node ws/app/scripts/build-rea.mjs` installs the pinned `rea-agents` npm
package (about 200 MB, scripts disabled, the optional `frida` add-on left out)
into `ws/app/src-tauri/rea` (gitignored). The installers carry it as a
resource and `install.ps1` mirrors it next to `Workspace.exe`. The app starts
it with the system's Node.js only when a team's lead calls
`start_reverse_engineering` (`app/src-tauri/src/rea.rs`). To try the real
thing end to end, with a supported Node.js:

```bash
cd ws
cargo test -p ws-app rea_starts -- --ignored --nocapture
```

## Layout

```
ws/
  crates/
    ws-context/     canonical conversation format + context builder (no I/O)
    ws-store/       SQLite, blobs, migrations
    ws-secrets/     OS credential store
    ws-providers/   API adapters (Anthropic, OpenAI, Gemini, compatible)
    ws-engine/      local inference engine: GGUF, tokenizer, template, sampler
    ws-engine-cli/  standalone engine harness
    ws-tools/       the agent's file, search, git and command tools
    ws-mcp/         tool servers (MCP)
    ws-core/        turn orchestration, agent teams, skills
    ws-cli/         the same turns from a terminal
  app/
    src-tauri/      Tauri backend and commands
    src/            React + TypeScript frontend
  fixtures/sse/     recorded provider streams used by tests
```

`ws-context` performs no I/O, which is why the rules that matter most — what a
model is allowed to see, and what must never be replayed to it — are tested
without a database, a network, or a model.

## A note on antivirus

On Windows, McAfee's real-time scanner (and others like it) can hold freshly linked binaries in `ws/target`, which
makes `cargo test --workspace` fail intermittently with
`LNK1104: cannot open file ...exe`. Running per package works:

```bash
cd ws
for p in ws-context ws-store ws-secrets ws-providers ws-core; do cargo test -p $p; done
```

Excluding `ws/target` in the scanner's settings resolves it.
