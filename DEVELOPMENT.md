# Developing Haiku Code

A short guide for contributors. [`README.md`](README.md) covers using and
installing Haiku Code; [`HAIKUCODE.md`](HAIKUCODE.md) has the per-file source
map, feature notes and the Haiku build details. This file is the "how do I
change it" part.

## Build, run, check

```sh
cargo build                                   # debug build: target/debug/haikucode
cargo run -- "say hi"                         # one-shot mode, no TUI
cargo test --bins                             # unit tests (about 260, a few seconds)
cargo test --bins -- --ignored                # also the live MCP / web tests (network)
cargo clippy --all-targets -- -D warnings     # must be clean: CI fails on any lint
```

CI (`.github/workflows/ci.yml`) runs build, test and clippy on Linux with a
**pinned** Rust version (`RUST_VERSION` in the workflow). Clippy is a hard gate,
so a new lint from a newer toolchain can't turn `main` red on its own: bump
`RUST_VERSION` deliberately and fix what the new clippy reports in the same PR.
To match CI exactly, `rustup toolchain install <ver> -c clippy` and run
`cargo +<ver> clippy --all-targets -- -D warnings`.

`cargo fmt --check` is still advisory: the tree predates rustfmt, and
reformatting it in one go would conflict with every open branch. Format the
code you touch; don't reformat whole files in a feature PR.

There are no Haiku or macOS runners. Before shipping, run `cargo test --bins`
on the Haiku VM (`haiku/deploy.sh` syncs and builds there) and on the Mac.

## How it fits together

Two threads, joined by channels:

- **Worker** (`agent.rs`): owns the conversation, calls the model
  (`api.rs`, with `native.rs` for the Anthropic Messages and OpenAI Responses
  protocols), runs tools, enforces permission rules (`policy.rs`) and hooks
  (`hooks.rs`), spawns sub-agents. It blocks on HTTP and tools, which is fine
  because nothing else runs on it.
- **UI** (`ui.rs` + `src/ui/`): the ratatui loop in `ui::run`. Each pass it
  drains `UiEvent`s from the worker (tokens, tool starts and results, diffs,
  approval and password requests), handles keys, and redraws. It never blocks
  on the network, so `Esc` can cancel a turn mid-flight.

The UI talks to the worker with `WorkerCmd` (a prompt, a config change, a
compaction) and answers approvals on a separate `ApprovalResponse` channel.
`main.rs` wires the two up for the TUI (`run_tui`) and for one-shot mode
(`run_oneshot`), which reuses the same worker with a plain stdout printer.

Most UI behavior can be tested without a terminal: build an `App`, feed it
keys or `UiEvent`s, and assert on what it sends the worker
(`test_handles()` in the `ui.rs` tests gives you the channel ends).

## Adding a built-in tool

A tool touches four places. Grep for an existing tool with the same shape
(`grep` for a read-only one, `write_file` for one that changes things) and
follow it.

1. **Schema** — `api.rs`, `tools_spec()`: name, description, JSON parameters.
   The description is the model's only documentation; say what comes back and
   what not to do.
2. **Implementation** — `tools.rs`: a plain function that takes typed
   arguments and returns the result text. Keep it free of worker state so it
   can be unit-tested directly.
3. **Dispatch** — `agent.rs`:
   - Read-only, no prompt, no worker state: add it to `parallel_safe()` and
     `run_readonly()`. Several such calls from one response then run side by
     side. Update `only_read_only_tools_run_in_parallel` too.
   - Anything that writes, prompts or needs the worker: add an arm to
     `run_tool_inner()`, ask with `self.approve(...)` unless the permission
     mode or an allow rule already covers it, and add it to the plan-mode
     refusal list if it changes anything.
   - If permission rules should match on its path or command, extend
     `policy_targets()` and the matching rule parsing in `policy.rs`.
4. **Sub-agents** — `tools_spec_subagent()` strips tools a sub-agent must not
   have (`task`, `ask_user`). Add yours there if it needs the user.

Then add a test next to the implementation, and a line to the tools list in
`README.md`.

## Adding a slash command

The palette entries live in `ui/types.rs` (`SLASH_COMMANDS` for the command,
`SLASH_OPTIONS` for its arguments); the handling is in `App::dispatch` in
`ui.rs`. A command that changes a setting usually sends a `ConfigPatch` to the
worker so the setting persists and the next turn sees it. User-defined
commands and skills need no code: they're Markdown files loaded by
`commands.rs`.

## Conventions

- **Explain the why** in comments, especially for platform workarounds
  (Haiku's tty, crossterm's event source, terminal quirks). The next reader
  won't have the bug report.
- **Never send raw model or tool text to the terminal**: pass it through
  `helpers::clean_text` so stray escape sequences can't corrupt the screen.
- **State files are written atomically** with `config::atomic_write`.
- **Tests name the behavior** (`a_text_only_rejection_marks_the_model_and_strips_images`),
  not the function.
- **American spelling** in code, comments, UI strings and docs.
- The version in `Cargo.toml` is bumped in its own PR before a release; see
  "Updates" in the README for the release steps.
