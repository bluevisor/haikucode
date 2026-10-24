# Haiku Code (haikucode)

A full-screen agentic coding CLI written in Rust, running **on Haiku** (the
BeOS-lineage OS; developed against Haiku R1~beta6 hrev60290 arm64 in a UTM VM).
It talks to any OpenAI-compatible chat API (default: DeepSeek) and drives a
Codex/Claude-Code-style tool loop with a ratatui terminal UI. It is a port of
picoder (the Raspberry Pi Zero W agent); the `picoder` git remote is upstream.

This is haikucode's own source. It builds natively on Haiku with
`cargo build --release` — see "Building" below.

## Layout

```
Cargo.toml            deps + release profile; Haiku-only crossterm use-dev-tty feature
haiku/package.sh      on Haiku: build + wrap dist/haikucode-<ver>-<rev>-<arch>.hpkg
haiku/deploy.sh       on the Mac: sync source to $HAIKU_HOST, build + package there, activate
haiku/launcher.sh     "apps/Haiku Code": Deskbar entry that opens haikucode in Terminal
haiku/PackageInfo.in  .PackageInfo template (version/revision/arch filled in)
src/
  main.rs             CLI args, first-run setup, one-shot mode (+ --output), session/context wiring
  config.rs           ~/config/settings/haikucode/config.json (incl. mcp_servers), setup wizard, session paths
  api.rs              OpenAI-compatible streaming SSE, tool schema, multimodal parts, retry, list_models
  tools.rs            tool impls: bash (+ background), read/write/edit/list, grep, glob,
                      web_fetch, web_search, todo, view_image, remember, recall
  agent.rs            worker thread: owns the conversation, runs the model/tool loop, sub-agents,
                      hooks + permission-rule enforcement, thrashing guard, `!cmd` shell runs
  policy.rs           permission rules: allow/deny patterns, bash segment splitting, rule persistence
  hooks.rs            lifecycle hooks (PreToolUse/PostToolUse/UserPromptSubmit/Stop/SessionStart)
  commands.rs         custom slash commands from .haikucode/commands/*.md and skills/*/SKILL.md
  mcp.rs              stdio MCP client: spawn servers, JSON-RPC handshake, tools/list + tools/call
  money.rs            Currency/Balance: keeps the cost and balance readouts in the
                      right units (DeepSeek bills CNY accounts in CNY, prices are USD)
  ui.rs               ratatui full-screen TUI: transcript, composer, status bar
  diff.rs             unified diff for edit/write previews
  askpass.rs          sudo password support: askpass helper + in-TUI masked prompt
  sysinfo.rs          /proc & /sys probes: board model, WiFi SSID, IP, RAM, cores
```

## Architecture

A long-lived **worker thread** (`agent.rs`) owns the message history and runs the
blocking HTTP + tool loop. The **UI thread** (`ui.rs`) renders with ratatui and
sends user input / approvals over mpsc channels; the worker streams back tokens,
tool events, diffs, and approval requests. This keeps the UI responsive and lets
`Esc` interrupt a turn.

## Features

- Tools: bash (timeout, or `background` jobs + bash_output/bash_kill),
  read/write/edit/list, multi_edit (batch edits across files), grep (regex),
  glob, web_fetch (URL → readable text), web_search (DuckDuckGo), todo (visible
  plan), ask_user, task (sub-agent), view_image, remember/recall — plus any MCP tools.
- Git auto-checkpoint: every successful edit is committed to the working-dir
  repo (`auto_commit`, on by default; no-op outside a repo). Recent git history
  is loaded into context at startup so the agent can use `git log`/`show`/`diff`
  as a clue and `git revert` to undo a bad edit.
- Sub-agents: the `task` tool delegates a self-contained job to a fresh agent
  with its own context and the same tools; only its final report returns.
- MCP: stdio servers from `mcp_servers` in config.json are launched at start
  and their tools advertised as `mcp__<server>__<tool>` (`/mcp` lists them).
- Images: `@image.png` attaches as a base64 data URI, and `view_image` loads
  one from disk — both sent as OpenAI multimodal content parts.
- Streaming with a Claude-style composer (`›`, reverse-block cursor, placeholder).
- Context compaction: `/compact` summarizes older turns (keeping the system
  prefix and latest exchange); auto-triggers at 80% of the context window.
- `Ctrl+C` clears a draft line and quits on a second press inside the
  double-press window (a non-empty composer used to swallow it entirely);
  `Esc` clears the line too.
- Queued input: the composer stays live while the agent works — Enter queues
  messages that send in order as turns finish (Esc interrupts and restores them).
- One-shot `--output FILE` writes the final reply to disk after the run.
- Status bar: model · session tokens + cost · context-window bar · account
  balance. `money.rs` tags both figures with their ISO code whenever the
  account's billing currency differs from `price_currency` (a CNY DeepSeek
  account using the USD price list), and a one-time notice says how to align
  them; prices are never converted across currencies.
- Permission modes via Shift+Tab (also Ctrl+P, which the Pi's framebuffer console
  needs since it can't report Shift+Tab): ask / bypass / plan (read-only); colored
  diff before write/edit. Handled in every mode, and never confused with the
  composer's Tab autocomplete.
- Instruction files: the first of `HAIKUCODE.md`/`AGENTS.md`/`CLAUDE.md`/`GEMINI.md`
  in `~/config/settings/haikucode/` and in each directory from the repo root down to the
  working directory, stacked general → specific (12k chars each, 32k total,
  budget spent nearest-first).
- Session persistence + resume (`haikucode --continue`, per working directory).
- Composer: typing `/` opens a command palette (suggestions ranked by your
  usage history; ↑/↓ select, Tab fills, Enter runs), `@file` attach, Tab
  autocomplete (commands + paths), history (↑/↓ on the composer's first/last line),
  word-skip (Alt/Ctrl/Cmd + arrows), word-delete (Option/Alt+Backspace and
  Option/Alt+Delete), code-block highlighting.
- Enables the Kitty keyboard protocol when the terminal supports it, so
  modified keys (e.g. Option+Backspace) are reported unambiguously instead of
  being flattened to a bare Backspace by terminals like Warp.
- Transcript scrolling: Ctrl/Alt+↑/↓ roll the output 3 lines, PgUp/PgDn a whole
  viewport, the mouse wheel a notch (where capture is on). Scrolling up pins the
  view and turns on a "↓ new" hint; sending a message snaps back to the live
  end. Scrolling has a keyboard gesture rather than only the wheel because Warp
  skips mouse capture (below) and would otherwise have no way to scroll.
- Mouse capture is skipped on Warp (`TERM_PROGRAM=WarpTerminal`) so click-drag
  text selection keeps working; `HAIKUCODE_MOUSE=1`/`0` overrides either way.
- ASCII fallback + clear-on-exit for the Pi's framebuffer console (`TERM=linux`),
  whose 16-color palette can't show a theme's RGB shades: the palette is snapped
  to the nearest ANSI color there (`palette::for_16color`), since the console
  ignores an `ESC[38;2;…m` and leaves the cell in whatever color came before.
- Launch banner: rainbow-color HAIKUCODE logo + live status (MEM, WiFi SSID + IP).
- Themes (`/theme`, numbered picker): `HaikuOS` (default: tab-yellow accent and
  logo on dark desktop blue, `~> `), `Rainbow` (picoder's original look: Apple-
  rainbow logo, terminal background; configs saying `Default` get HaikuOS),
  `Apple ][` (green phosphor, `] ▒`),
  `MSDOS` (gray, `C:\>`), `macOS` (dark mode system colors, `~ `),
  `SUN` (amber/gold, `sun% `), `NeXT` (platinum monochrome, `NeXT>`),
  `SGI` (indigo/teal IRIX, `irix# `); persisted in config. Theme sets colors, prompt, cursor.
- `/model` opens an interactive picker over the provider's model list —
  type to filter (handy for OpenRouter's hundreds), ↑/↓ + Enter to select;
  `/model <id|number>` still sets directly.
- Slash commands: `/model /auto /reset /compact [focus] /config /mcp /memory /theme /init /new
  /diff /undo /rewind /review /status /cost /permissions /hooks /commands /clear /help /exit`, plus any
  custom command from `.haikucode/commands/<name>.md` (`$ARGUMENTS`). `!cmd` runs a shell
  command directly and records the output in the conversation.
- Permission rules (`policy.rs`): `permissions.allow` / `permissions.deny` arrays merged from
  `config.json`, `.haikucode/settings.json`, `.haikucode/settings.local.json`. Deny is checked
  before the permission mode (so it holds in bypass), allow skips the prompt in ask mode.
  The approval prompt offers **P** — persist a suggested rule (`bash(cargo test:*)`,
  `edit_file(src/**)`) to `settings.local.json` (or config.json outside a repo). **N** denies
  and lets the model continue; **Esc** denies and interrupts the turn. Bash rules split on
  `&& || ; | &`; `$(…)`/backticks/`sh -c`/`sudo`/`xargs` never auto-allow. Path rules match the
  lexically normalized path (`tools::expand`), so `..`/`./`/`~` spellings can't dodge a deny.
- Hooks (`hooks.rs`): Claude-Code-shaped `hooks` block; each hook is `sh -c` with a JSON
  payload on stdin. Exit 2 blocks (PreToolUse → tool not run; UserPromptSubmit → turn
  skipped; Stop → stderr fed back and the loop continues, max 3 times). stdout becomes context.
- Thrashing guard: the third consecutive identical tool call (same name + args) is answered
  with an error instead of executed.
- History hygiene: `agent::repair_orphans` answers tool_calls that never got a result (panic
  mid-round, session saved by a crash) so the next request isn't a 400; `wait_reply` makes
  approval/ask_user prompts give up when the turn is cancelled, so quitting never hangs `join`.
- Edits match raw text first and fall back to NFC only when that fails, so a file with
  decomposed characters isn't rewritten wholesale; `apply_write` and `multi_edit` refuse
  symlinks (O_NOFOLLOW) like `edit_file`; auto-commit commits only the edited paths, never the
  user's own staged changes.
- One-shot scripting: piped stdin is appended as `<stdin>` context (or is the task);
  `--json` prints a final `{result, usage, cost, tool_calls, duration_ms, errors}` object,
  `--stream-json` prints JSON-lines events (`token`, `tool_start`, `tool_result`, `notice`,
  `error`, `diff`, `reasoning`, `result`).
- `/config`: interactive settings panel — provider preset (deepseek/openai/anthropic/groq/openrouter/google),
  base URL, model, API key (masked), reasoning (auto/off/low/medium/high, sent
  in each provider's own field; also `/effort`), default
  permission mode (ask/bypass/plan — applies live and saves for new sessions),
  auto-commit, theme, context window, max tool calls, price currency. Changes
  apply immediately and persist.
- Costs and balance: `src/money.rs` formats both, tagging each with its ISO code
  when the account's billing currency differs from `price_currency`. DeepSeek
  reports CNY for China-region accounts, so a CNY account on the default USD
  prices sees `$0.03 USD · … │ bal ¥135.70 CNY` plus a one-time hint to set
  `price_currency` (and CNY prices) if they want a single unit. Nothing is ever
  converted — an exchange rate would be a guess.

## Building (natively on Haiku)

There is no macOS→Haiku cross toolchain here; the Haiku box builds itself.

```sh
cargo build --release        # on Haiku
cargo test --bins            # the suite passes on Haiku and on the Mac
haiku/package.sh             # → dist/haikucode-<ver>-1-<arch>.hpkg
haiku/deploy.sh              # from the Mac: sync, build + package on HAIKU_HOST, activate
```

Copying the .hpkg into `~/config/packages` activates it a moment later
(binary → `~/config/bin`, launcher → `~/config/apps`, Deskbar entry under
`~/config/data/deskbar/menu`). `deploy.sh` waits for deactivation, then
activation, and `cmp`s the installed binary with the build: swapping a
same-version package in one go races the package daemon.

Haiku-specific code is behind `cfg(target_os = "haiku")`: `config::CONFIG_SUBDIR`,
`sysinfo::{haiku_os_name, mem_used_total}`, the `HAIKU_SHELL_RULE` in the system
prompt, `auth::open_browser` (`open`), and `helpers::tty_hung_up` in the UI loop.

Haiku gotchas this code works around:
- crossterm's default mio event source stops delivering keys after the first
  batch on Haiku → `use-dev-tty` feature (Cargo.toml).
- Haiku sends no SIGHUP when a pty goes away; the tty polls POLLHUP and reads
  fail → the UI loop checks `tty_hung_up()` and exits.
- Block elements (`█ ░`) render wider than a cell in Haiku Terminal → logo cells
  are background-colored spaces (`helpers::art_spans`).
- A roster-launched script that `exec`s Terminal leaves Terminal windowless →
  `launcher.sh` backgrounds it.

## Config / state

`~/config/settings/haikucode/` — `config.json` (provider/model/key), `memory.md`,
`history`, `sessions/`.

MCP servers are optional and configured by hand in `config.json`:

```json
{
  "provider": "deepseek",
  "model": "deepseek-v4-pro",
  "api_key": "...",
  "mcp_servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/boot/home"],
      "env": {}
    }
  }
}
```

Each server is spawned over stdio at launch; its tools appear as
`mcp__filesystem__<tool>`. `/model` and `/theme` rewrites preserve the block.

## Roadmap / wishlist

- **Session forking / naming**; `--resume` picker over several sessions per directory.
- **OS-native sandboxing** (Seatbelt / bubblewrap) for bash — rules + hooks are policy,
  not containment.
- **MCP server mode** (expose haikucode's tools over stdio).

- **ui.rs split** (~3100 lines → submodules). A clean split would extract:
  `ui/banner.rs` (banner rendering, ~300 lines), `ui/panels.rs` (config picker,
  model picker, theme picker, `/` palette — ~800 lines), `ui/transcript.rs`
  (~400 lines), and `ui/palette.rs` (Palette + THEMES, ~150 lines). Each
  submodule depends on ratatui/crossterm; they'd be `pub(crate)` re-exported by
  `ui.rs` so the rest of the crate sees no change.

- **Config/environment-driven integration tests**. MCP handshake and web_search
  tests currently `#[ignore]` because they need a live server; a `HAIKUCODE_TEST=1`
  gate would run them in CI with a containerized MCP stub.

## Performance / safety notes

- **Transcript render cache** (`ui.rs`). `ensure_display_cache` renders the
  static (non-`live`) transcript into `disp_cache` once and reuses it until the
  content (`tver`, bumped via `dirty()`/`set_palette`) or terminal width
  changes; each frame clones only the visible window. This avoids re-wrapping
  thousands of lines per streamed token on a single-core Pi (where picoder started).
- **Atomic state writes** (`config::atomic_write`). `config.json` and session
  files are written to a per-process temp file, fsync'd, then renamed over the
  target, so a power loss on the SD card never leaves a truncated file.
- **Symmetric symlink refusal** (`tools::deny_symlink`). `read`/`list` refuse
  symlinked paths just like `write`/`edit`; `expand` lexically collapses
  `.`/`..` so an approved path is the path actually used.
- **No raw escapes reach the terminal** (`helpers::clean_text`). Model and tool
  text is stripped of control characters before it is drawn — whole ANSI
  sequences (CSI/OSC, not just the ESC byte), so reading a colored log can't
  spill `[1;31m` into the transcript or corrupt the frame's colors. The composer
  buffer is sanitized at insert time too, so pasting colored text can't desync
  the screen either. `render_tline` splits on `\n` first, so multi-line tool
  output keeps its line structure instead of being reflowed into one paragraph.

## CI

`.github/workflows/ci.yml` runs `cargo test` on Linux (hard gate); GitHub has
no Haiku runners, so run `cargo test --bins` on the VM before shipping. Clippy
and `cargo fmt --check` run advisory-only until the tree is clean.
