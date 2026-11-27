# Workspace guide

Everything Workspace does, in more detail than the [README](README.md).

## Add a provider

API keys live in the system credential store (Windows Credential Manager, or
the Linux desktop keyring) and nowhere else — not in the
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
The agent's tools (reading and writing files, running commands) work through
these endpoints too, so Build with agent can use a model on OpenRouter or Groq;
the OpenAI (Responses) and Gemini connections still only chat.

For OpenRouter, the base URL is `https://openrouter.ai/api/v1`; for Groq,
`https://api.groq.com/openai/v1`. Each key goes under the id you gave it. A gateway that says what each model supports (as
OpenRouter does) has the models that cannot call tools left out of the picker,
since every request carries the agent's tools and they would fail; the rest
get the context window it reports (OpenRouter and Groq both report one), and a reply allowance of what it reports,
up to a quarter of that window and 32k tokens (gateways charge credit up front
for the largest reply allowed). A profile you edit yourself wins.

Model ids are never hardcoded. Anything the provider does not report a context
window for is marked *unprofiled* and uses conservative defaults until you set
real values.

## Agent teams

Tick **Coordinate** under the message box before you send. The model you chat
with becomes the lead: it writes a plan in the project's `.ws/plan.md`, then
hands parts of the task to workers that run side by side. In a git project the
whole team works in one copy of it, so each worker sees the others' changes;
when the run ends they are saved on one branch, and a worker's card in the
chat shows them with **Merge into my files** and **Discard**. Your files
change only when you merge. With Coordinate on, sending an empty message
carries on with the saved plan.

- **Six roles.** The team has fixed roles, and the lead decides which gets
  what and when: the **explorer** maps the code, **implementers** build,
  the **tester** writes and runs tests, the **debugger** fixes failures, the
  **reviewer** checks changes, and the **docs-writer** updates documentation.
  The explorer and reviewer only read. Up to two copies of a role work at
  once (`implementer` and `implementer-2`), six workers in all. A role
  remembers its earlier tasks in the run, so the lead sends a fix back to
  the copy that wrote the code.

- **Every worker at a glance.** A line at the top of the Agents view shows
  each worker's state: **Working**, **Blocked** (waiting for your approval,
  or to reach its model), **Idle** (finished its task while the team runs,
  free for another), **Done** or **Failed**, with its model. Click a name to
  open its pane.
- **Team chat.** The lead's briefs, the workers' notes, questions and reports
  appear in the chat as they happen. Every worker reads every note, so they
  can share what they found. You can write to the team while it works.
- **Asked only when it matters.** When the lead needs a decision it asks you
  in the chat and waits for your answer; if Workspace is in the tray, its
  window comes back to ask.
- **Cheaper workers.** **Settings → Worker models** picks the model workers
  start on; the model you chat with stays the lead. Under it, **By role**
  gives a role its own model, such as a strong one for the reviewer and a
  cheap one for the explorer; roles left on *Same as the worker model* use
  the worker model. **Worker fallbacks** lists
  models a worker switches to when its own fails (overloaded, down, out of
  credit). A provider's rate limit is waited out rather than failing the
  worker.
- **Fewer approval prompts.** **Settings → Approval rules** lets agents edit
  files inside the project, and run commands you list (`cargo test` also covers
  `cargo test -p core`), without asking. Everything else still asks, and a
  command with quotes, pipes, `;`, `&`, `>` or `$` always asks.

### Reverse engineering

Workspace bundles [Reverse Engineer Anything](https://github.com/morluto/rea)
(REA) 6.10.1, and only agent teams can use it. Ask a team how existing
software does something ("how does this app's search work", "what does this
.exe call", "trace this Electron app's clipboard bridge") and the lead
switches REA on by itself: REA starts on your Node.js, the lead gets REA's
workflow guide, and the lead and every worker it starts after that can use it.

REA inspects native binaries (pseudocode, assembly, strings, calls), Electron
and JavaScript apps, .NET assemblies, Android APKs, websites, firmware and
packaged apps, and returns the evidence behind each finding. It has 263 tools,
about 90,000 tokens of descriptions, too many to hand a model at once. So
teams use it in **Code Mode**: the model writes a short JavaScript function
instead of picking from a tool list.

- `rea_search` runs a function over REA's catalog of tools, so the model finds
  the few it needs without the whole list entering its context.
- `rea_execute` runs a function that calls REA (`await rea(tool, args)`): it
  can chain calls, loop over results and run several at once, and returns only
  what the model asked for. Two inspections that answer with 47,000 characters
  of evidence can come back as a two-line summary.
- `rea_call` makes one plain call, for a model whose scripts keep failing, and
  `rea_guide` reads REA's guides.

Scripts run inside Workspace in a sandbox (QuickJS) that has no file system,
network or process access: all a script can do is call REA. Memory, running
time and the number of calls are capped. Plain chats never see any of it.

- **Needs** Node.js 22.23.4+, 24.22.1+ or 26.12.1+. If yours is older, the
  lead says so in the chat. Static JavaScript, .NET and APK manifest work
  needs nothing else; decompiling native binaries needs Ghidra, IDA or
  Hopper installed, and Android classes need JADX.
- **Asks first.** Each `rea_execute` script asks once, showing the script, and
  allowing it allows every REA call it makes; each `rea_call` asks too. Runtime
  capture runs or interacts with the target, so read what a script does.
- **Your responsibility.** REA is for software you own or are authorised to
  examine, and for lawful research and interoperability.

## Run a local model

Open **Settings**, paste the path to a `.gguf` file under **Local model** and
choose **Read file**. That reads the header only -- architecture, quantisation,
and whether it will fit in memory -- so nothing is committed before you can see
what the file is. **Load model** then makes it available in the model picker
like any other.

Llama-architecture GGUFs only, and the file must carry its own chat template:
guessing one produces fluent text from a model that no longer believes it is in
a conversation, so this build refuses instead.

The engine also runs on its own, outside the app; see [DEVELOPING.md](DEVELOPING.md#the-local-engine-on-its-own).

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
| `.tex` | `tectonic`, or `pdflatex` twice; the PDF is written beside it |

Workspace ships no compilers; it uses whatever is on PATH.

### Language packs

On Windows, Workspace installs what each language needs with winget. The
installer asks which languages you want once Workspace is installed, and
**Settings → Languages** adds more later or retries one that failed. Each pack
shows what is already on your computer and installs only what is missing.

| Pack | Installs |
| --- | --- |
| Core | Git (agent teams' copies, the Changes panel), Node.js LTS (Reverse Engineer Anything, knowledge graphs, running `.js`) |
| Python | Python 3.14, debugpy, Pyright |
| JavaScript and TypeScript | TypeScript and its language server (npm) |
| C and C++ | WinLibs (GCC and GDB), LLVM (clangd) |
| Rust | Microsoft's C++ build tools, rustup, rust-analyzer, LLVM (lldb-dap) |
| Go | Go, gopls |
| LaTeX | MiKTeX |

Some of these ask for admin rights, and Microsoft's C++ build tools are a
download of several gigabytes. Restart Workspace afterwards so it finds the new
tools. Silent installs and updates skip the question.

On Ubuntu and Debian, the `.deb` brings Git and Node.js with it, and Workspace
opens the same chooser the first time it starts. It installs with apt, asking
for your password once per part; rustup, npm's language servers and gopls go
into your home folder (`~/.cargo`, `~/.local`) and need no password.

| Pack | Installs (Ubuntu, Debian) |
| --- | --- |
| Core | git, nodejs, npm |
| Python | python3, python3-debugpy, Pyright |
| JavaScript and TypeScript | TypeScript and its language server |
| C and C++ | gcc, g++, gdb, clangd |
| Rust | build-essential, rustup, rust-analyzer, lldb |
| Go | golang-go, gopls |
| LaTeX | TeX Live (texlive-latex-base, texlive-latex-recommended) |

Ubuntu 22.04's own gdb and Node.js are too old for debugging C and for the
language servers; use Ubuntu 24.04 or newer, or install newer ones yourself. On
other Linux systems, install the same programs with your package manager, then
restart Workspace.

### Packages (numpy and friends)

- When a Python run fails with `No module named …`, the output panel offers
  **Install …**, which runs `python -m pip install` for the Python that runs
  your files (common import names map to their package: `cv2` →
  `opencv-python`, `PIL` → `pillow`, `sklearn` → `scikit-learn`, …).
- The terminal button in the left toolbar opens your shell (PowerShell on
  Windows, bash on Linux) in the current folder, inside the output panel, for anything else: `pip install`,
  `npm install`, `cargo add`, virtual environments.

## The window

- **Top bar:** the folder in use, its git branch (click it for Changes) and a
  search box that opens the finder (Ctrl+P).
- **Icon strip** down the left edge: **Files**, **Search** and **Changes** each
  fill the left pane; click the open one to hide the pane. Changes shows how
  many files changed. **Agents** appears once a team has run, with how many
  need you. **Settings** is at the bottom. Which view was open, or that the
  pane was hidden, is remembered.
- **Settings** opens over the whole window on an overview of its categories
  (General, Models, Agents, Connections, Privacy); every topic, such as API
  keys or GitHub, is a page of its own, and the list on the left jumps to any.

## Files and folders

Hover a file or folder on the left for rename (pencil) and delete (bin).
Delete asks first and moves it to the Recycle Bin (the Trash on Linux). The
**+** above the tree opens a file, makes a new file, folder or blueprint, a
knowledge graph, or a terminal; New file and New folder ask where, in the
folder picker, then for a name.

## Code and Markdown files

Code (and config) files and Markdown open in the editor in the middle pane;
Ctrl+S saves, and the agent can change the same files (the view reloads when
it does, unless you have unsaved changes). Plain text (`.txt`, `.log`) opens
too. Other files (PDFs, Word, images…) are left out of the tree, since they do
not open in Workspace.

Markdown is highlighted, and **Preview** (on by default) shows it rendered
beside the editor as you type. The preview is sanitized, since a Markdown
file can carry HTML: scripts, event handlers, frames, forms and inline
styles are removed, links do not navigate, and images load only if they are
embedded in the file.

The agent has its own tools for the open folder: `list_dir`, `search_files`, `read_file`,
`write_file`, `edit_file`, `run_command`, and `get_open_file` (so "fix this
file" works). Writes show as diffs with Undo. Commands ask first, unless
**Settings → Approval rules** lists them; changes made by commands cannot be
undone from the chat.

## Git and GitHub

**Changes** (in the icon strip) shows what changed since the last commit:
stage with **+**, unstage with **−**, write a message and **Commit**. At its
top is the branch bar:

- **Branch** switches branches, or makes a new one (**New branch…**). git
  refuses to switch when that would lose uncommitted changes.
- **Push** sends your commits (↑ how many); the first push puts the branch
  on `origin`. **Pull** brings in new commits (↓ how many), only when no merge
  is needed. Both use git's own sign-in, the same one a terminal uses.
- For a repo on GitHub it shows the branch's pull request, or **Open a PR**
  (opens GitHub's page to make one), and the CI of the commit you are on.

**Clone** (the branch icon above the file tree, next to Open folder) takes `owner/repo` or a
repo's address, asks where to put it, and opens the new folder.

**Settings → GitHub** signs Workspace in, with the GitHub login git already
has (Git Credential Manager may show its own sign-in window) or a pasted
token. The token is kept in the system keychain. Signed in, PR and CI status
works for private repos too, and there is no 60-an-hour limit. **Give the agent GitHub tools** starts
GitHub's own tool server ([github-mcp-server](https://github.com/github/github-mcp-server),
put it on PATH) with that token at the next start, so the agent can read and
write issues and pull requests and check CI; its tools ask before running.

## Workshop: draw a system, simulate it, have the agent build it

The Workshop is where blueprints (`.blueprint` files) are drawn. The **New
blueprint** button on the left (two boxes joined by a wire) opens an untitled
one in its own Workshop window, the way Simulink opens beside MATLAB. Nothing is written until you press **Save as…** (or Ctrl+S), which
asks where; the file then shows on the left. Opening a `.blueprint` file
from the left, or double-clicking one in Windows Explorer (the installer
registers the file type), opens it in its own window too, and opening it again brings
that window forward. Closing the main window closes Workspace with its
blueprint windows, after asking if any has unsaved changes. The canvas is
a bit like LTspice or Multisim but for software:

- **Moving around:** the mouse wheel or two fingers on a touchpad scroll the
  canvas (Shift+wheel sideways), as do the arrow keys and dragging empty
  space. Ctrl+wheel or a pinch zooms; so do the + and − buttons.
- **The left side** shows the parts and connectors you use most (the usual
  ones until you have used some). **View full inventory** opens all of them
  over the canvas, with a search (name, description or category), your own
  parts and connectors, and **+ New part** / **+ New connector**. Drag or click
  from there too; Esc closes it.
- **Parts:** drag them from the left onto the canvas, or click one to add it:
  users, web front end, API service, worker, queue, database, cache, LLM, AI
  agent, vector database, third-party API, and more.
- **Inventory:** **+ New part** and **+ New connector** (in the full inventory) make your own, and
  they are kept in the inventory for every blueprint (in the app data folder,
  `parts-library.json`). ✎ edits one everywhere it is used; 🗑 removes it from
  the inventory, and blueprints that use it keep their own copy. A custom part
  that is only in one blueprint shows ⤓ to keep it in the inventory.
  Each has a **category** (set in its editor; General if left empty), and
  the inventory groups them under collapsible headings. ✎ on a heading
  renames that category.
- **Parts builder:** the editor for a new part or connector starts with
  *Build it from a description*. **Build (free)** reads the description
  itself, no AI and no cost: it spots the kind of part (database, cache,
  queue, LLM, vector store, worker, front end, users, external API, service…),
  the product named (Postgres, Redis, Kafka, gpt-4o…) and any numbers
  ("2 replicas", "5 ms", "300 requests/s"). **Build with AI** asks your model
  instead, for anything unusual. Either fills in the form to check before
  Save. Whatever a model answers is checked and cleaned before it can reach
  the inventory.
- **Ports and settings** (config-driven, as in VectorShift's node configs): a
  part can have named ports in (how others call it) and out (what it calls),
  each taking one connector or any, and settings (text, number or a choice)
  that each placed part can change. Wires attach to a port; Run reports a
  connector a port does not take, or a wire to a port that is gone. The agent
  is told every port, setting and which ports each wire joins.
- **Connectors:** drag from a part's right dot to another part's left dot.
  HTTP, function call, SQL, cache, LLM request, queue message, event, or your
  own. Click a connector on the left to use it for new wires (and the
  selected wire), or drag it onto a wire to change that one; **Automatic**
  picks one the target takes. Dashed connectors do
  not wait for an answer.
- **Details:** right-click a part or a wire to set what it should do (the agent
  builds from this), its load, its speed, its error rate and how many copies
  run.
- **Contracts** say what a part does, not just how it looks: its
  **operations** (`createOrder` takes `items, customerId`, returns `orderId`),
  the **data it keeps** and its **rules**, one per line. Set them in a part's
  details (right-click), or as defaults on a part type that each placed part
  can change; Build with AI fills them in too. On a wire, pick which of the
  target's operations it **calls** and what it **sends**: Run reports a call to
  an operation the target does not have, or one missing what the operation
  takes, and flags parts with no function written (the agent would guess).
  The agent is told every operation, data line and rule, and builds them.
- **Run** simulates steady traffic through the drawing. Each wire shows its
  requests per second and each part its load, latency and how full it is.
  Findings are listed as **Bug** (connectors a part does not take, loops that
  wait on themselves, parts nothing reaches), **Scaling** (overloaded parts
  and how many copies they need, how far the load can grow) and **Deploy**
  (requests past a 30 s timeout, single points of failure, low success rates).
  The load slider runs it at 0.1× to 50×, and **Simulate this part failing**
  takes one part down.
- **Build with agent** (enabled once Run finds no bugs) saves, then opens a
  new chat in the main window (with the model you last picked in a chat, as
  new chats and Build with AI use too) that asks the agent to write the whole system into the
  blueprint's folder in one go, with a README, local run config and tests.

**The sandbox: run what was built.** Build with agent also writes a
`sandbox` section into the blueprint: the command that starts every part
(for example `docker compose up --build`), one that stops it, a URL that
answers once it is up, and a test request (probe) for each part.
**Sandbox** shows those commands and asks first, starts the system, waits
for it, sends each part's probe for 5 s with 8 requests at a time, then stops
it all (the start command's whole process tree too). Each part's measured
typical and slow time, failures and requests answered per second appear next
to what the drawing expects; a part answering fewer per second than the
drawing sends it is flagged. **Use measured numbers** puts each part's own
time and failure rate into the drawing (the time of what it waits on is
taken off), so the next Run works from real numbers. Requests only ever go
to this computer (localhost), and redirects are not followed. The answered
rate is a floor on what a part handles, not its limit.

**The text behind the canvas.** A `.blueprint` file is readable JSON, like a
SPICE netlist: what the parts are and how they are wired, then a `layout`
section with every position, so moving a box changes only its layout lines
in a diff. New parts and wires get readable ids (`orders-api`,
`users-orders-api`). **Source** opens the file's text in the main window's
editor; saving it there redraws the canvas in place, and saving the canvas
updates the text. A file the canvas cannot read (half-typed JSON) is
reported and the drawing is kept.

**From a part to its code.** Build with agent records in the blueprint where
it wrote each part's code (the part's *Its code* field, which you can also
set). Double-click a part, or **Open** in its details, to open that file in
the editor or that folder on the left.

The simulation is a model (steady-state queueing), not your code running: it
catches design mistakes before code exists. The file is plain JSON with a copy
of each custom part it uses, so it works without the inventory and the agent
can read or change it too.

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

**Code Mode.** Models do not see each server's tools one by one: a large
server such as Playwright would fill their context before the task starts.
They get three tools instead, whatever you connect:

- `mcp_search` runs a short JavaScript function over the catalog of every
  server's tools, so the model finds the ones it needs.
- `mcp_execute` runs a function that calls them (`await call("playwright",
  "browser_navigate", {url})`), chaining and looping over calls and returning
  only what the model asked for.
- `mcp_call` makes one plain call, for a model whose scripts keep failing.

Scripts run inside Workspace in a sandbox (QuickJS) with no file system,
network or process access of their own: all a script can do is call your
tool servers. Its memory, running time and number of calls are capped.

**Approval.** A tool you list in that server's `autoApprove` runs without
asking (unless it runs commands); every other tool asks first. A script asks
once, showing the whole script, when it names a tool that asks; a script
that names only auto-approved tools runs straight away, and such a script
cannot call anything else, even by building a tool's name as it runs.

Once an approved edit runs, the thread shows its diff with an **Undo** button
that puts the file back byte for byte; a script that edits several files shows
each with its own Undo. Undo refuses if the file has changed again since,
rather than discard that change. Diffs need `--project` in the server's args,
as above; with `--project-from-cwd` the app cannot tell which file was
edited. Tool calling works with Anthropic models
and OpenAI-compatible endpoints (OpenRouter, Groq…); the OpenAI (Responses) and
Gemini adapters still flatten tool history to text.

## Skills

Skills are `SKILL.md` folders in the Claude Code format. Two sets are built
into Workspace, with nothing to install:

- [Ponytail](https://github.com/DietrichGebert/ponytail): `/ponytail`,
  `/ponytail-audit`, `/ponytail-debt`, `/ponytail-gain`, `/ponytail-help`,
  `/ponytail-review`.
- [Understand-Anything](https://github.com/Egonex-AI/Understand-Anything)'s
  graph skills: `/understand-chat`, `/understand-explain`, `/understand-diff`,
  `/understand-onboard`.

They live in `ws/app/src-tauri/skills` (MIT, licences alongside) and are
compiled into the app. Add your own anywhere under the `skills` folder in the
app data folder (a whole cloned repo is fine) and restart; one with a built-in's name replaces it. Settings → Skills lists them.

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
  `ws/app/public/ua`) is served by Workspace itself on 127.0.0.1 behind a
  one-time token, as UA's own viewer does. No Node, no download, no model.
- **Ask about it:** `/understand-chat`, `/understand-explain`,
  `/understand-diff`, `/understand-onboard`. The agent reads the graph with
  its built-in `search_files` and `read_file` tools.
- **Free graph:** Workspace reads the code itself (Rust, no model, no Node): files, the functions
  and types they define, what imports what, one layer per folder, a short
  tour. Summaries come from each file's leading comment. It will not replace
  an AI-built graph unless you confirm.
- **AI graph:** opens the folder, starts a new chat with a Claude model and
  sends `/understand` (you can also type it yourself). This is UA's own pipeline: the model dispatches helper
  agents (`dispatch_agent`, up to five at once) that run UA's bundled scripts
  and write real summaries, layers and a tour. It makes many API calls and
  needs Node.js, Python and bash (Git Bash on Windows). **Allow all for this reply** on the
  approval prompt saves clicking through every step.
