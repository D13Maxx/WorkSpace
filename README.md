# Workspace

A desktop AI workspace with its own inference engine.

One conversation that moves across API models and a local model, in any order,
without losing context. When you switch to a model with a smaller window, the
app writes a summary of the older messages and keeps the recent ones verbatim —
and shows you exactly what the model received.

Windows first. Rust throughout, React + TypeScript inside Tauri 2.

Full design: [`WORKSPACE_M1_MASTER_SPEC.md`](WORKSPACE_M1_MASTER_SPEC.md).
Progress against milestone 1: [`docs/checkpoints/C1.md`](docs/checkpoints/C1.md).

## Status

Milestone 1, checkpoint C2. All four provider adapters work end to end
(Anthropic, OpenAI Responses, Gemini, and any number of OpenAI-compatible
endpoints). The local engine (`ws-engine`) is not built yet -- that is C3.

## Requirements

- Rust stable (1.85+) with the `x86_64-pc-windows-msvc` toolchain
- Visual Studio Build Tools with the C++ workload (for the MSVC linker)
- Node 20+ and npm
- WebView2 runtime (present on current Windows 11 by default)

## Build and run

```bash
cd ws/app
npm install
npm run tauri dev
```

For a release build and the Windows installer:

```bash
cd ws/app
npm run tauri build
```

## Test

```bash
cd ws
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

Two suites are gated behind features because they touch real systems and must
never run in CI (master spec §7.6):

```bash
# Writes to the real Windows Credential Manager.
cargo test -p ws-secrets --features live-tests

# Talks to real provider APIs; needs a key configured in the app first.
cargo test -p ws-providers --features live-tests
```

## Add a provider

API keys live in Windows Credential Manager and nowhere else — not in the
database, not in logs, not in exports. The app can tell you whether a key is
set, but cannot read one back.

1. Launch the app and open **Settings**.
2. Paste a key next to `anthropic`, `openai` or `gemini` and choose **Save**.
3. The model list is fetched from each provider that has a key.

To add an OpenAI-compatible endpoint (a local llama.cpp, vLLM, a gateway), use
**OpenAI-compatible endpoints** in the same panel: give it an id, a display
name and a base URL. It is wired up on the next launch. Reasoning from such a
server is shown but never replayed -- the protocol has nothing signed to send
back.

Model ids are never hardcoded. Anything the provider does not report a context
window for is marked *unprofiled* and uses conservative defaults until you set
real values.

## Layout

```
ws/
  crates/
    ws-context/     canonical conversation format + context builder (no I/O)
    ws-store/       SQLite, blobs, migrations
    ws-secrets/     OS credential store
    ws-providers/   API adapters (Anthropic, OpenAI, Gemini, compatible)
    ws-engine/      local inference engine (not yet built)
    ws-engine-cli/  standalone engine harness (not yet built)
    ws-core/        turn orchestration
  app/
    src-tauri/      Tauri backend and commands
    src/            React + TypeScript frontend
  fixtures/sse/     recorded provider streams used by tests
docs/
  adr/              decisions that deviate from or extend the spec
  checkpoints/      acceptance evidence per checkpoint
```

`ws-context` performs no I/O, which is why the rules that matter most — what a
model is allowed to see, and what must never be replayed to it — are tested
without a database, a network, or a model.

## A note on antivirus

McAfee's real-time scanner holds freshly linked binaries in `ws/target`, which
makes `cargo test --workspace` fail intermittently with
`LNK1104: cannot open file ...exe`. Running per package works:

```bash
cd ws
for p in ws-context ws-store ws-secrets ws-providers ws-core; do cargo test -p $p; done
```

Excluding `ws/target` in McAfee's settings resolves it.
