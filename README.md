# Workspace

A desktop AI workspace for Windows and Linux: one conversation across API
models and a local model, a code editor with language servers and a debugger,
agent teams that plan and build in parallel, and the Workshop for designing
software visually.

<!-- Screenshot: put the image in images/ and uncomment.
![Workspace](images/workspace.png)
-->

- **Chat with any model, switch freely.** Anthropic, OpenAI, Gemini, any
  OpenAI-compatible endpoint (OpenRouter, Groq, a local llama.cpp) and a built-in
  engine for GGUF models. Switching to a model with a smaller window summarises
  the older messages and keeps the recent ones, and shows you exactly what the
  model received.
- **A real editor.** Language servers (definitions, references, rename, code
  actions), a debugger, git (changed lines, staging, commits, blame), split
  editors, a terminal, Ctrl+P, Vim mode, your own keymap. **Run** compiles and
  runs Python, JavaScript/TypeScript, C/C++, Rust and LaTeX files.
- **Agent teams.** Tick *Coordinate*: a lead model plans the task and starts
  workers (on a cheaper model if you like) that run side by side, talk in a team
  chat you can watch and join, work in their own git copies, and ask you only
  when it matters.
- **The Workshop.** Draw a system from parts and connectors, simulate its load
  to find bugs and bottlenecks, then have the agent build it and measure the
  real thing.
- **Tools and skills.** Connect MCP tool servers, use Claude Code-style skills,
  and turn a codebase into a knowledge graph you can explore and ask about.

## Download

Get the latest version from the
[Releases page](https://github.com/D13Maxx/WorkSpace/releases/latest).

**Windows 10/11:** download `Workspace_x.y.z_x64-setup.exe` and run it. It
installs for your user only, no admin rights needed. The installer is not
signed yet, so Windows SmartScreen may warn: choose *More info → Run anyway*.

**Linux (Ubuntu 22.04+, Debian 12+, Mint and other Debian-based systems):**
download `Workspace_x.y.z_amd64.deb`, then

```bash
sudo apt install ./Workspace_*_amd64.deb
```

### What else to install

Workspace itself needs nothing else to chat, edit files and run agents. Some
features use tools from your system, found on PATH:

| To use | Install |
| --- | --- |
| Git features, agent workers in their own copies | Git |
| Running `.py` files, AI knowledge graphs | Python 3, Node.js |
| Running `.js` / `.ts` files, tool servers started with `npx` | Node.js |
| Running C/C++ | gcc/g++ or clang |
| Language servers | the server for each language, e.g. `rust-analyzer`, `pyright`, `typescript-language-server`, `clangd` |
| The debugger | `lldb-dap` (from LLVM) |

On Windows:

```powershell
winget install Git.Git OpenJS.NodeJS Python.Python.3.13 LLVM.LLVM
```

On Linux:

```bash
sudo apt install git nodejs python3 clangd lldb
```

Restart Workspace after installing so it sees the new PATH.

## Getting started

1. Open **Settings** and paste an API key next to `anthropic`, `openai` or
   `gemini`, or add an OpenAI-compatible endpoint such as OpenRouter
   (`https://openrouter.ai/api/v1`) or Groq (`https://api.groq.com/openai/v1`).
   You can also load a local `.gguf` model instead.
2. Pick a model at the top of a chat and start talking.
3. Open a folder from the left pane to edit its files and let the agent work
   on them. Edits show as diffs with Undo; commands ask before they run.
4. For a bigger task, tick **Coordinate** under the message box and the model
   runs it as a team.

The [guide](GUIDE.md) covers each feature in detail: providers, agent
teams, local models, running code, the Workshop, tool servers, skills and
knowledge graphs.

## Your data

API keys are kept in your system's credential store (Windows Credential
Manager, or the Linux desktop keyring such as GNOME Keyring or KWallet) and
nowhere else: not in the database, not in logs, not in exports. Conversations,
settings and your parts inventory live in the app data folder:

- Windows: `%APPDATA%\dev.debargha.friday`
- Linux: `~/.local/share/dev.debargha.friday`

## Build from source

See [DEVELOPING.md](DEVELOPING.md) for building, testing, releasing and
how the code is laid out.

## License

Apache License 2.0; see [LICENSE](LICENSE). The bundled Ponytail and
Understand-Anything skills and pipeline are MIT, with their licences alongside.
