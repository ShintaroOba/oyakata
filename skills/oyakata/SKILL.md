---
name: oyakata
description: "OYAKATA (親方) — open the browser command post that lists, follows and drives coding-agent sessions (Claude Code, Codex CLI, Gemini CLI, Copilot CLI, OpenCode) across repositories, and lets the user send instructions from the browser. Use when the user says things like 'open oyakata', 'show this in the browser', 'list my sessions', 'what are the other agents doing', 'this is hard to read in the terminal', or 'show me a diagram' — or in Japanese 「oyakata を開いて」「ブラウザで見たい」「セッション一覧」「他のリポジトリの Claude は何してる」「ターミナルだと読みづらい」「図で見たい」. After opening it, write diagrams as Mermaid."
allowed-tools: Bash(oyakata:*)
argument-hint: "[session-id]"
---

# OYAKATA

A browser-based command post. It indexes every session of every supported agent on this
machine (Claude Code under `~/.claude`, Codex CLI, Gemini CLI, Copilot CLI, OpenCode),
groups them by repository, follows running ones every second, and renders their output as
HTML with Markdown, Mermaid diagrams and syntax-highlighted code. The repository's file
tree, Git diffs, the files the agent edited and any artifacts open in the same window.

## Step 1: open it

`oyakata` starts its own background daemon, opens the browser and returns right away (it
does not block). If the daemon is already running it only opens the browser, so calling it
repeatedly is fine.

```bash
oyakata                      # no argument: the current session opens first
oyakata --focus <session-id> # open that session
```

Run the first line when `$ARGUMENTS` is empty, the second when a session id was given.
`run_in_background` is not needed.

Tell the user the URL from stdout (`OYAKATA is open at http://127.0.0.1:4848/...`) as is.

If the command is not found (`oyakata: command not found`), install the prebuilt binary
with the bundled script (no Rust toolchain needed). Do not guess other commands.

```bash
# macOS / Linux
sh "${CLAUDE_PLUGIN_ROOT}/scripts/install.sh"
# Windows (the Bash tool is Git Bash, so call PowerShell from it)
powershell -NoProfile -ExecutionPolicy Bypass -File "${CLAUDE_PLUGIN_ROOT}/scripts/install.ps1"
```

- The script ends with `Installed: <path>`. The current shell's PATH does not pick it up,
  so use that path instead of `oyakata` for the rest of this session (new terminals can
  call `oyakata`).
- When `${CLAUDE_PLUGIN_ROOT}` is not expanded (the skill was installed into a skills
  directory rather than as a Claude Code plugin), fetch the same script from GitHub:
  `curl -fsSL https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.sh | sh`
  (Windows: `irm https://raw.githubusercontent.com/ShintaroOba/oyakata/main/scripts/install.ps1 | iex`).
- With a Rust toolchain, `cargo install --git https://github.com/ShintaroOba/oyakata` also works.

## Step 2: write for the browser from now on

While OYAKATA is open, this session's replies are rendered as HTML. Follow these rules.

- Diagrams go in ```` ```mermaid ```` fences (flowchart / sequenceDiagram / classDiagram /
  stateDiagram-v2 / erDiagram / gantt / mindmap). Never draw diagrams in ASCII art or box
  characters.
- Comparisons and lists of options go in Markdown tables.
- Split long explanations with `##` headings.
- Give code blocks a language (```` ```rust ````, ```` ```bash ````, ```` ```json ```` …).
- No terminal-style alignment (full-width spaces, ruled boxes).
- After editing files, end the reply with the list of edited paths (OYAKATA's "changed
  files" menu opens them).

## When the user asks how it works

- The browser can send instructions to sessions OYAKATA started (the "＋" button picks a
  folder and an agent and opens an empty chat; `oyakata new "prompt"` from a terminal; or
  sending to an ended session, which OYAKATA resumes — `oyakata attach --resume <id>` does
  the same from a terminal). Claude Code sessions take prompts mid-turn and show permission
  prompts in the chat; the other agents run one turn per prompt and take the next prompt
  when the turn ends. Sessions started with `claude` directly in a terminal can also be
  typed into (Windows only): "Send to terminal" types into that console, so permission
  prompts and questions are answered in the terminal. Sessions running in an IDE
  extension or the SDK cannot be driven.
- A session OYAKATA owns can also be typed into from a terminal with `oyakata attach <id>`
  (alongside the browser). `/quit` detaches the terminal, `/stop` ends the session.
- For long-running use, suggest starting the daemon from a terminal (`oyakata`) rather
  than from inside an agent session: a daemon started inside one can be taken down when
  that session exits.
- `oyakata status` shows whether the daemon runs, `oyakata stop` stops it. Stopping also
  ends sessions OYAKATA started, but their conversations remain and can be resumed.
- The screen refreshes itself every second; the user never needs to reload.
- Other agents are detected automatically from their data folders (`~/.codex`,
  `~/.gemini`, `~/.copilot`, OpenCode's database); which ones are shown, and the UI
  language (Japanese / English), are in the browser's Settings.
