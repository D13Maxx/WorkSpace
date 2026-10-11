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

## Write and run code

Open a file from the left pane and edit it in the middle. **Run** saves the
file, compiles it and runs it in the output panel below. The panel is a real
terminal, so programs can ask for input.

| File | How it runs |
| --- | --- |
| `.py` | `python` |
| `.js` / `.ts` | `node` (TypeScript without enums or namespaces) |
| `.c` / `.cpp` | `gcc` / `g++` (or clang), then the program |
| `.rs` | `cargo run` inside a Cargo project, otherwise `rustc` then the program |
| `.tex` | `tectonic`, or `pdflatex` twice; the PDF opens when it builds |

FRIDAY ships no compilers; it uses whatever is on PATH. For C/C++ and LaTeX:

```powershell
winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT
winget install -e --id MiKTeX.MiKTeX
```

then restart FRIDAY so it sees the new PATH.

### Packages (numpy and friends)

- When a Python run fails with `No module named …`, the output panel offers
  **Install …**, which runs `python -m pip install` for the Python that runs
  your files (common import names map to their package: `cv2` →
  `opencv-python`, `PIL` → `pillow`, `sklearn` → `scikit-learn`, …).
- The terminal button in the left toolbar opens PowerShell in the current
  folder, inside the output panel, for anything else: `pip install`,
  `npm install`, `cargo add`, virtual environments.

## Files and folders

Hover a file or folder on the left for rename (pencil) and delete (bin).
Delete asks first and moves it to the Recycle Bin. New file and New folder
ask where, in the folder picker, then for a name.

## PDFs, Word and text files

Everything opens editable in the middle pane, and the agent can change the
same files (the view reloads when it does, unless you have unsaved changes).

- **Text and code:** the editor; Ctrl+S saves.
- **Word (.docx):** [SuperDoc](https://www.superdoc.dev), a .docx editor with
  a Word-style toolbar (fonts, sizes, colours, lists, tables, alignment).
  Save writes the .docx back with its formatting. SuperDoc is AGPL-3.0:
  free for personal use; sharing FRIDAY would mean publishing its source.
- **PDFs:** pdf.js. **Edit text** changes words already on a page: click a
  line, retype it, Enter, then Save. The old line is covered with white and
  the new words are written over it, in Times, Helvetica or Courier at the
  same size; the old text is still in the file underneath, so do not use it
  to hide anything. **Text**, **Draw** and **Highlight** add annotations.
  Encrypted PDFs can be annotated but not text-edited.

The agent has its own tools for the open folder: `list_dir`, `search_files`, `read_file`,
`write_file`, `edit_file`, `run_command`, and `get_open_file` (so "fix this
file" works). Writes show as diffs with Undo. Commands always ask first, and
are how it edits PDFs and Word files (with Python's `pypdf` and
`python-docx`). Changes made by commands cannot be undone from the chat.

## Blueprints: draw a system, simulate it, have the agent build it

The **New blueprint** button on the left (two boxes joined by a wire) makes a
`.blueprint` file. It opens as a canvas, a bit like LTspice or Multisim but
for software:

- **Parts:** drag them from the left onto the canvas, or click one to add it:
  users, web front end, API service, worker, queue, database, cache, LLM, AI
  agent, vector database, third-party API, and more. **+ New part** defines
  your own, with its numbers and which connectors it takes.
- **Connectors:** drag from a part's right dot to another part's left dot.
  HTTP, function call, SQL, cache, LLM request, queue message, event, or your
  own (**+ New connector**). The connector picked on the left is used for
  new wires; **Automatic** picks one the target takes. Dashed connectors do
  not wait for an answer.
- **Details:** click a part or a wire to set what it should do (the agent
  builds from this), its load, its speed, its error rate and how many copies
  run. **Describe** sets the system's purpose and the stack you want.
- **Run** simulates steady traffic through the drawing. Each wire shows its
  requests per second and each part its load, latency and how full it is.
  Findings are listed as **Bug** (connectors a part does not take, loops that
  wait on themselves, parts nothing reaches), **Scaling** (overloaded parts
  and how many copies they need, how far the load can grow) and **Deploy**
  (requests past a 30 s timeout, single points of failure, low success rates).
  The load slider runs it at 0.1× to 50×, and **Simulate this part failing**
  takes one part down.
- **Build with agent** (enabled once Run finds no bugs) saves, then opens a
  new chat that asks the agent to write the whole system into the
  blueprint's folder in one go, with a README, local run config and tests.

The simulation is a model (steady-state queueing), not your code running: it
catches design mistakes before code exists. The file is plain JSON, so the
agent can read or change a blueprint too. Test the model with
`node ws/app/scripts/test-blueprint.mjs`.

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

Skills are `SKILL.md` folders in the Claude Code format. Two sets are built
into FRIDAY, with nothing to install:

- [Ponytail](https://github.com/DietrichGebert/ponytail): `/ponytail`,
  `/ponytail-audit`, `/ponytail-debt`, `/ponytail-gain`, `/ponytail-help`,
  `/ponytail-review`.
- [Understand-Anything](https://github.com/Egonex-AI/Understand-Anything)'s
  graph skills: `/understand-chat`, `/understand-explain`, `/understand-diff`,
  `/understand-onboard`.

They live in `ws/app/src-tauri/skills` (MIT, licences alongside) and are
compiled into the exe. Add your own anywhere under
`%APPDATA%\dev.debargha.friday\skills` (a whole cloned repo is fine) and
restart; one with a built-in's name replaces it. Settings → Skills lists them.

- Type `/name request` to run one. Its instructions travel with your message,
  so this works with every model, local ones included.
- Claude also sees the list and loads a skill on its own when a request fits.

## Knowledge graphs (Understand-Anything)

[Understand-Anything](https://github.com/Egonex-AI/Understand-Anything) turns
a codebase into a knowledge graph in the project's `.ua` folder.

The graph button (linked dots) in the left pane's toolbar asks for a folder,
then offers the choices below, and **Open it** when the folder already has a
graph.

- **Explore a graph:** UA's dashboard (v2.9.0, prebuilt, in
  `ws/app/public/ua`) is served by FRIDAY itself on 127.0.0.1 behind a
  one-time token, as UA's own viewer does. No Node, no download, no model.
- **Ask about it:** `/understand-chat`, `/understand-explain`,
  `/understand-diff`, `/understand-onboard`. The agent reads the graph with
  its built-in `search_files` and `read_file` tools.
- **Free graph:** FRIDAY reads the code itself (Rust, no model, no Node): files, the functions
  and types they define, what imports what, one layer per folder, a short
  tour. Summaries come from each file's leading comment. It will not replace
  an AI-built graph unless you confirm.
- **AI graph:** opens the folder, starts a new chat with a Claude model and
  sends `/understand` (you can also type it yourself). This is UA's own pipeline: the model dispatches helper
  agents (`dispatch_agent`, up to five at once) that run UA's bundled scripts
  and write real summaries, layers and a tour. It makes many API calls and
  needs Node.js, Python and Git Bash. **Allow all for this reply** on the
  approval prompt saves clicking through every step.

The pipeline lives in `ws/app/src-tauri/ua-pipeline` (gitignored, 38 MB). It
is built by `node ws/app/scripts/build-ua-pipeline.mjs`, which clones UA
v2.9.0, installs with scripts disabled, and bundles each script with esbuild;
`install.ps1` builds it when missing and copies it next to `FRIDAY.exe`.

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
