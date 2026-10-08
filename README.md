<div align="center">

<img src="web/icon.svg" width="96" height="96" alt="OYAKATA">

# OYAKATA（親方）

**Watch and direct your Claude Code sessions from the browser.**

OYAKATA gathers the Claude Code sessions running across your repositories into one screen, renders their output properly, and lets you send instructions right there.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](#from-the-terminal)
[![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-8A2BE2.svg)](#get-started-with-the-skill)

**English** | [日本語](README.ja.md)

</div>

![OYAKATA: the session list on the left, Claude's answer rendered with a table, a diagram and code on the right](docs/images/hero.png)

> **Note** The browser UI is in Japanese. Where it helps you find a control, this README gives the on-screen label in parentheses.

## What is it?

Once you run two or three Claude Code sessions in parallel, the terminal stops being enough.

- You lose track of which terminal is doing what.
- Long answers with tables and diagrams are hard to read in a terminal.
- Tool-call logs bury the conversation.

OYAKATA solves this in a single browser tab. It reads the records Claude Code already keeps under `~/.claude`, so you do not change how you launch or use Claude Code. Everything stays on your machine; nothing is sent anywhere.

| | What you can do |
| --- | --- |
| **See** | Every session in every repository, in one list. Running ones are pinned on top, with their state (working / idle / waiting for your decision) refreshed every second |
| **Read** | Markdown, tables, code and diagrams rendered as HTML. Long answers get a table of contents and collapsible sections |
| **Direct** | Send instructions from the browser. Answer permission prompts and questions in place |
| **Ship** | Open the files Claude changed in an editor next to the chat, check the diff, and commit / push |

> About the name: *oyakata* (親方) is the master of a Japanese workshop. You are the oyakata, watching and directing the main Claude (*tōryō*, 棟梁, the master builder) and its sub-agents (*shokunin*, 職人, the craftsmen).

## Get started with the skill

Install the `/oyakata` skill as a Claude Code plugin and you are done. No Rust toolchain needed.

**1. Install the plugin** (inside Claude Code)

```
/plugin marketplace add ShintaroOba/oyakata
/plugin install oyakata@oyakata
```

From a terminal: `claude plugin marketplace add ShintaroOba/oyakata`, then `claude plugin install oyakata@oyakata`.

**2. Type `/oyakata`**

```
/oyakata
```

The first time, the `oyakata` binary is not there yet, so Claude installs a prebuilt one from [GitHub Releases](https://github.com/ShintaroOba/oyakata/releases) using the script bundled with the skill (Windows / macOS / Linux). It then starts the background daemon and opens the browser on the current session.

From then on, the Claude in that session knows it is being read in a browser: it writes comparisons as tables and diagrams in a renderable form (Mermaid).

You do not have to type `/oyakata` literally. Phrases like "show this in the browser" or "what is Claude doing in the other repos?" trigger the skill too.

## A tour of the screen

### Sessions and the conversation

![Session list and conversation (dark theme)](docs/images/hero-dark.png)

- **The sidebar** lists running sessions at the top and each repository's history below. The dot shows the state: orange is working, green is idle, purple is waiting for your decision. The counts appear at the bottom of the sidebar too ("1 working · 1 waiting").
- **Click a session** to open its conversation. The work log (tool calls and so on) is hidden; the single line above the input tells you what Claude is doing right now (`Bash: Run the filter tests … esc to interrupt`). A setting shows the full log.
- **Under the input** is a status line like the terminal's: permission mode on the left (cycle with Shift+Tab), model, effort level and context usage on the right.

### Starting a new session

![New session dialog](docs/images/new-session.png)

Press "＋" in the header and pick a folder. An empty chat opens, and Claude starts when you send the first message. Permission mode starts as auto.

You can send while Claude is busy. The prompt is handed over at the next tool-call boundary; until then it is listed above the input as pending. Permission requests, `AskUserQuestion` prompts and plan approvals appear as cards in the chat that you answer in place. Approvals get a vermilion "承認" (approved) stamp, rejections an indigo "差戻" (sent back) stamp.

### Team chart (sub-agents)

![Team chart](docs/images/team.png)

Press "👥" at the top right of the chat to see how the main Claude (tōryō) and its sub-agents (shokunin) are organised: who is working on what, which tool each is running right now, and what each reported when done. Agents dispatched together are grouped into a squad (陣), updated live.

### Files, diffs and Git

![Editor, diff and Git view](docs/images/workbench.png)

Next to the conversation you can open the files Claude edited in an editor (Ctrl+S saves), read the diff, and commit / push / pull from the Git view in the sidebar. Panes split by dragging tabs, like VS Code, and the layout is remembered per repository.

More things it does:

- `Ctrl+P` fuzzy-opens files; `Ctrl+Shift+F` searches the repository's text or all your conversations
- `F12` / Ctrl+click goes to a definition and `Shift+F12` lists references, with no language server required
- A file reference like `src/main.rs:42` in a message opens that line in the editor
- Wooden clappers sound when a worker finishes. Optional desktop notifications when a session goes idle or waits for a decision
- Themes: light, dark, sepia, Solarized, Nord, Dracula and high contrast, plus accent color, font size and content width
- Delete sessions you no longer need. Records move to a trash folder (`~/.claude/oyakata-trash`) and are purged after 30 days

## From the terminal

OYAKATA also works as a plain command, without the skill. It is a single binary with no runtime dependencies.

### Install the binary

Prebuilt binaries (Windows x64 / arm64, macOS Intel / Apple Silicon, Linux x64 / arm64) are attached to every [GitHub Release](https://github.com/ShintaroOba/oyakata/releases). The install script downloads the one for your machine, verifies its SHA-256 and puts it on your PATH.

```bash
# macOS / Linux: installs to ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.sh | sh
```

```powershell
# Windows: installs to %LOCALAPPDATA%\Programs\oyakata and adds it to your user PATH
irm https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.ps1 | iex
```

`OYAKATA_INSTALL_DIR` changes the location and `OYAKATA_VERSION=v0.1.0` pins a release. With a Rust toolchain (1.80+), `cargo install --git https://github.com/ShintaroOba/oyakata` works too.

You also need:

| | |
| --- | --- |
| Claude Code | The `claude` CLI, used to start sessions from the browser |
| git | For the file tree, diffs and Git operations |

### Start and stop

```bash
oyakata                     # start the daemon (or reuse the running one) and open the browser
oyakata --focus <session>   # same, opening a specific session
oyakata status              # is a daemon running?
oyakata stop                # stop it (sessions OYAKATA started end too; their conversations stay on disk)
oyakata install             # copy the /oyakata skill into ~/.claude/skills/oyakata (if you don't use plugins)
```

If you keep the daemon around for a long time, start it from a terminal (`oyakata`) rather than from inside Claude Code (`/oyakata`). A daemon started from inside a Claude session can be taken down when that session exits. Nothing is lost if that happens: send a message to the session in the browser and OYAKATA resumes it.

<details>
<summary>More commands and options</summary>

| Command | What it does |
| --- | --- |
| `oyakata --no-open` | Start the daemon without opening a browser |
| `oyakata serve` | Run the server in the foreground (handy for watching logs) |
| `oyakata new "first prompt"` | Start an OYAKATA-owned session in the current folder and attach this terminal to it. Also `--cwd <dir>`, `--model`, `--mode`, `--effort`, `--no-attach` |
| `oyakata attach <session-id>` | Attach this terminal to an OYAKATA-owned session (`/quit` detaches, `/stop` ends the session) |
| `oyakata attach --resume <session-id>` | Resume an ended session under OYAKATA |
| `oyakata sessions` | List the sessions OYAKATA owns right now |

| Option | Default | Meaning |
| --- | --- | --- |
| `--port <n>` | `4848` | Port to listen on |
| `--bind <addr>` | `127.0.0.1` | Address to bind |
| `--claude-dir <path>` | `$CLAUDE_CONFIG_DIR` or `~/.claude` | Claude Code config directory |
| `--claude <path>` | found on `PATH` | The `claude` executable used for sessions started from the browser |
| `--repo-root <dir>` | | Extra folder whose direct subfolders are listed as repositories (repeatable) |

The daemon writes its log to `~/.claude/oyakata.log`.

A session OYAKATA owns accepts input from both the browser chat and a terminal. Whatever you type on either side goes into the same conversation, and permission prompts and questions can be answered from either.

```bash
oyakata new "Refactor the parser"   # start here, attached to this terminal
oyakata attach <session-id>         # join the same conversation from another terminal
```

</details>

## FAQ

**Which sessions can I talk to from the browser?**

| Session | From the browser |
| --- | --- |
| Started by OYAKATA ("＋" or `oyakata new`) | Full control. Send anytime, even mid-task, and answer permissions and questions in the chat |
| Ended | Sending a message makes OYAKATA resume it with `claude --resume`; from then on it behaves like the row above |
| Running in a terminal (Windows) | "Send to terminal" (ターミナルへ送信) types into that console and presses Enter. Answer permissions in the terminal |
| Running in an IDE extension or the SDK | No input (there is no console to type into). It can be taken over after it ends |

**Where does my data go?**

Nowhere. The server listens on `127.0.0.1` only, and viewing just reads the files Claude Code writes (`~/.claude/projects/*/*.jsonl`, `~/.claude/sessions/*.json`). The rendering libraries (marked, DOMPurify, highlight.js, Mermaid, CodeMirror) are bundled into the binary, so it works offline.

**Do I need to change my Claude Code settings?**

No. Optionally, to make every session draw diagrams as Mermaid, add a line like this to `~/.claude/CLAUDE.md`:

```
Write diagrams in ```mermaid fences (OYAKATA renders them in the browser). Do not draw diagrams as ASCII art.
```

<details>
<summary>Keyboard shortcuts</summary>

Shortcuts follow VS Code. The full list is also in settings ("Shortcuts").

| Key | Action |
| --- | --- |
| `Ctrl+P` | Open file (fuzzy; `main.rs:42` jumps to a line; `>` commands, `:` line, `@` sessions) |
| `Ctrl+Shift+P` / `F1` | Command palette |
| `Ctrl+Shift+F` | Full-text search (this repository's files / all conversations) |
| `Ctrl+F` / `F3` / `Shift+F3` | Find in editor / next / previous |
| `Ctrl+G` | Go to line |
| `F12` / Ctrl+click | Go to definition (pick from a list if there are several) |
| `Shift+F12` | Find references (listed in the search view) |
| `Alt+←` / `Alt+→` | Back / forward through jump history |
| `Ctrl+Shift+E` / `Ctrl+Shift+G` | Show file tree / Git |
| `Ctrl+B` | Toggle sidebar |
| `` Ctrl+` `` | Focus the chat input |
| `Ctrl+\` | Split the active tab to the right |
| `Ctrl+Alt+N` | New session |
| `Ctrl+,` | Settings |
| `Ctrl+S` | Save in editor |
| `Enter` / `Shift+Enter` | Send / newline in the input |
| `Shift+Tab` | Cycle permission mode (auto → default → acceptEdits → plan) |
| `Esc` | Interrupt Claude if it is working; otherwise close menus and dialogs |
| `/` / `t` | Search sessions / toggle light and dark |
| Middle click | Close tab |

`Ctrl+W`, `Ctrl+Tab`, `Ctrl+N` and similar are left to the browser.

</details>

<details>
<summary>How it works and safety</summary>

- **Indexing.** `~/.claude/projects/<cwd>/<session>.jsonl` is read once at startup to build metadata (title, cwd, turn count, tokens, permission mode, edited files and so on). After that only the appended bytes are read. Only the session on screen keeps its body in memory.
- **Running sessions.** `~/.claude/sessions/<pid>.json` lists them. Only entries whose pid is alive count as running; `status: waiting` is shown as "waiting for decision".
- **Context meter.** The last response's tokens (input + cache + output) divided by the model's context window. For OYAKATA-started sessions the window comes from Claude Code; otherwise it is inferred from the model name.
- **Team chart.** Reads `<session>/subagents/agent-*.jsonl` and `*.meta.json` and matches them to the parent's Agent tool calls.
- **Browser-started sessions** are child processes of `claude -p --input-format stream-json --output-format stream-json --permission-prompt-tool stdio`. The conversation is written to `~/.claude/projects` as usual.
- **Typing into terminals (Windows)** writes keystrokes to that console's input buffer (`AttachConsole` + `WriteConsoleInputW`). The process start time is checked first, so a reused pid is never typed into.
- **Git operations** call the `git` CLI directly. Commit uses the selected files; push adds `-u origin HEAD` when there is no upstream; pull is `--ff-only`. Branch switching is intentionally absent.
- **Go to definition and search** use `git grep`. No language server is involved.
- **Saving files** sends the modification time seen at load. If the file changed on disk, the UI stops and offers "overwrite / reload".
- **Network.** Cross-site requests to `/api` are rejected via `Sec-Fetch-Site`, and mutating calls require a custom header.

</details>

## Development

```bash
cargo test
cargo run -- serve --no-open
```

To release, bump `version` in `Cargo.toml` and `.claude-plugin/plugin.json` to the same number and push a `vX.Y.Z` tag. `.github/workflows/release.yml` builds six targets and attaches the archives to a GitHub Release, which is where the install scripts download from.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

```
src/
  main.rs        CLI (start, daemon, stop, skill install)
  server.rs      axum routes, SSE, file-watch loop, cross-site protection
  index.rs       session index with incremental updates, repository list
  transcript.rs  JSONL → display items, edited files and artifact extraction
  runner.rs      claude -p sessions started by OYAKATA (stream-json)
  console.rs     typing into terminal-run sessions (Windows console input)
  client.rs      terminal front end (new / attach / sessions)
  gitops.rs      git status / tree / diff / log / grep / commit / push / pull / clone
  live.rs        running sessions (pid liveness)
  paths.rs       PATH augmentation and executable lookup
  repo.rs        cwd → repository name
web/
  index.html / style.css / icon.svg
  app.js         shared: API, themes, Markdown, transcript rendering, event bus, settings
  workbench.js   pane split tree, tabs, drag and drop, per-repository layouts
  chat.js        chat pane (conversation, terminal-style input and status line, permission/question/plan cards, progress)
  team.js        team chart (main Claude and sub-agents)
  code.js        go to definition / references, back / forward, file references in messages
  palette.js     Ctrl+P / command palette / pickers
  search.js      full-text search view (files / conversations)
  fx.js          hanko stamps and hyōshigi clappers
  editors.js     CodeMirror editor, diff / commit / URL / sub-agent views
  sidebar.js     sessions / tree / Git (commit, push, pull), delete, add repository
  vendor/        bundled libraries (marked, DOMPurify, highlight.js, Mermaid, CodeMirror 5)
skills/oyakata/  the /oyakata skill
scripts/         install.sh / install.ps1 (download a prebuilt binary from GitHub Releases)
docs/images/     README screenshots (taken with dummy sessions and repositories)
.claude-plugin/  plugin and marketplace manifests
.github/workflows/release.yml  builds six targets on a version tag and publishes the release
```

## License

MIT. Bundled libraries keep their own licenses; see `web/vendor/LICENSE.*`.
