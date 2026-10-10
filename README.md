# F.R.I.D.A.Y

A desktop AI workspace with its own inference engine.

One conversation that moves across API models and a local model, in any order,
without losing context. When you switch to a model with a smaller window, the
app writes a summary of the older messages and keeps the recent ones verbatim —
and shows you exactly what the model received.

Windows first. Rust throughout, React + TypeScript inside Tauri 2.

Full design: [`WORKSPACE_M1_MASTER_SPEC.md`](WORKSPACE_M1_MASTER_SPEC.md).
Progress against milestone 1: [C1](docs/checkpoints/C1.md), [C2](docs/checkpoints/C2.md),
[C3](docs/checkpoints/C3.md).

## Status

Milestone 1, checkpoint C3. All four provider adapters work end to end
(Anthropic, OpenAI Responses, Gemini, and any number of OpenAI-compatible
endpoints), and so does the local engine: a GGUF model loads, renders its own
chat template, tokenizes with its own vocabulary, and streams a reply, reusing
its KV cache across turns.

The forward pass is still `candle-transformers`' reference implementation
behind `ws-engine`'s own wrapper. Everything around it -- validation, the
tokenizer, the chat template, the sampler, the cache policy, the threading --
is hand-written and stays when the model is replaced. Two limits come from that
choice and are reported rather than hidden: a 4,096-token context cap, and a KV
cache that cannot be partially rewound.
[`docs/checkpoints/C3.md`](docs/checkpoints/C3.md) has the details.

## Install a desktop shortcut

```bash
powershell -ExecutionPolicy Bypass -File ws/app/scripts/install.ps1
```

Builds a release copy into `%LOCALAPPDATA%\Programs\F.R.I.D.A.Y` and puts an
F.R.I.D.A.Y shortcut on the desktop. Run it again after changing the app to
update what the shortcut opens (close the app first).

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

## Run a local model

Open **Settings**, paste the path to a `.gguf` file under **Local model** and
choose **Read file**. That reads the header only -- architecture, quantisation,
and whether it will fit in memory -- so nothing is committed before you can see
what the file is. **Load model** then makes it available in the model picker
like any other.

Llama-architecture GGUFs only, and the file must carry its own chat template:
guessing one produces fluent text from a model that no longer believes it is in
a conversation, so this build refuses instead.

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

## Connect tool servers (Serena)

Models can call tools from any MCP server. List them in `mcp.json` in the app
data folder (Settings → Tool servers shows the exact path), in the same
`mcpServers` format Claude Code uses. For [Serena](https://github.com/oraios/serena):

```json
{
  "mcpServers": {
    "serena": {
      "command": "serena",
      "args": ["start-mcp-server", "--context=ide", "--project", "C:/path/to/your/repo"]
    }
  }
}
```

Install Serena first with `uv tool install -p 3.13 serena-agent`. Servers start
in the background at launch; changes to `mcp.json` apply on the next launch.

Read-only tools run straight away. Anything that can edit files or run
commands stops and asks you first. Once an approved edit runs, the thread
shows its diff with an **Undo** button that puts the file back byte for byte.
Undo refuses if the file has changed again since, rather than discard that
change. Diffs need `--project` in the server's args, as above; with
`--project-from-cwd` the app cannot tell which file was edited. Tool calling works with Anthropic models
today; the other adapters still flatten tool history to text.

## Skills

Skills are `SKILL.md` folders in the Claude Code format, such as
[Ponytail](https://github.com/DietrichGebert/ponytail). Put them anywhere under
`%APPDATA%\dev.debargha.friday\skills` (a whole cloned repo is fine) and
restart; Settings → Skills lists what was found.

- Type `/name request` to run one. Its instructions travel with your message,
  so this works with every model, local ones included.
- Claude also sees the list and loads a skill on its own when a request fits.

## Knowledge graphs (Understand-Anything)

[Understand-Anything](https://github.com/Egonex-AI/Understand-Anything) turns
a codebase into a knowledge graph in the project's `.ua` folder.

- **Explore a graph:** Settings → Knowledge graph, give the project folder,
  choose **Open graph**. It opens UA's own dashboard in a window; no model and
  no tokens are involved. The first time downloads UA's viewer (needs Node.js).
- **Ask about it:** clone the UA repo into the skills folder, connect Serena,
  then use `/understand-chat`, `/understand-explain`, `/understand-diff` or
  `/understand-onboard`. They read the graph through Serena's search and read
  tools.
- **Build a graph:** not yet in this app. `/understand` dispatches subagents,
  which MyAgent does not have; build the graph with Claude Code for now.

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
    ws-engine/      local inference engine: GGUF, tokenizer, template, sampler
    ws-engine-cli/  standalone engine harness
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
