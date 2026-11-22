# Agents in Halyard

Status: the model is implemented and tested as a library (`crates/halyard-agents`).
Wiring it into the app (polling the foreground process, feeding focus and input
events, drawing the sidebar and the dashboard) is not done yet; see `ROADMAP.md`.

Halyard shows what a coding agent in a pane is doing using only signals it can
observe (ADR 0004). Every state names the signal that produced it (`Cause` in the
code). When there is no signal, the state is `Unknown`; Halyard does not guess.
Halyard never shows token counts, cost, model names, or anything else it cannot
observe.

## Which agent

`halyard_agents::detect_agent` looks at the pane's foreground process: the process
group leader returned by `tcgetpgrp` on the PTY (`halyard_agents::platform`).

| Agent | Recognized as |
|-------|---------------|
| Claude Code | `claude`; `node .../@anthropic-ai/claude-code/...`; the native installer's `.../claude/versions/<v>` binary |
| Codex | `codex`; the npm wrapper's native `codex-<target triple>` binary; `@openai/codex` |
| OpenCode | `opencode`; `opencode-ai` |
| Gemini CLI | `gemini`; `@google/gemini-cli` |
| Aider | `aider`; `python -m aider`; `aider-chat` |
| Goose | `goose`, except the pressly/goose migration tool (first argument is a database driver or migration command) |
| Amp | `amp`; `@sourcegraph/amp` |
| Other | any name an agent reports with the protocol's `agent=` field |

Names come from `argv[0]`, then the executable path. For launchers (`node`, `bun`,
`deno`, `npx`, `bunx`, `uvx`, `pipx`, `python*`, ...) the first non-option argument
is checked by name and by package path. Arguments of other programs are never
searched, so `vim claude.md` is not an agent.

Process information: macOS reads the executable with `proc_pidpath` and argv with
`sysctl(KERN_PROCARGS2)`; Linux reads `/proc/<pid>/exe` and `/proc/<pid>/cmdline`.
The executable is read before and after argv and the read is retried if they
differ, so an exec between the two reads never pairs one program's argv with
another's executable. Right after a fork the foreground process can still be a copy
of the shell, so the app should poll (for example every 500 ms) and treat the answer
as eventually consistent.

## Attention states

`AttentionState`: `Unknown`, `Working`, `WaitingForInput`, `Finished { exit_code }`,
`Idle`, `Error`. The machine (`AttentionMachine`) is driven by timestamped events
and by `tick(now)`; it never reads a clock. "Agent active" below means an agent was
detected in the foreground, or a protocol report with `state=` arrived, and it has
not exited since.

### Rules

| # | Signal | Condition | Result | `Cause` |
|---|--------|-----------|--------|---------|
| 1 | Protocol report with `state=` (OSC 7717 or OSC 777;halyard) | `session=` absent or equal to the pane's id | `working` Working, `waiting` WaitingForInput, `idle` Idle, `done` Finished (with `exit=` if given), `error` Error. The agent becomes active and the protocol latch is set. | `ProtocolReport` |
| 2 | OSC 9 or OSC 777;notify notification, or BEL | agent active, no protocol latch | WaitingForInput, waiting latch set | `Notification` / `Bell` |
| 2b | The same | agent active, protocol latch set | state unchanged; the pane becomes unread if not focused | (unchanged) |
| 3 | User input (keys, paste) | a latch is set | latch cleared; Working if output arrived within the idle period, else Idle | `UserInput` |
| 4 | Output | agent active, no latch | Working | `OutputActivity` |
| 5 | `tick`: no output for `idle_after` (default 8 s, configurable) | agent active, no latch, Working | Idle | `Quiet` |
| 6 | Agent process started (foreground became an agent) | agent not already active | Working, latches cleared | `AgentStarted` |
| 7 | Agent process left the foreground | agent active | Finished with the exit status if known | `AgentExited` |
| 8 | Shell end-of-command mark (OSC 133;D or 633;D) | agent active | Finished with D's exit status | `CommandFinished` |
| 9 | OSC 133;D | not active, state is Finished without a status | fills in the status | (unchanged) |
| 10 | Focus | | gaining focus clears unread | (unchanged) |

Consequences, each covered by `crates/halyard-agents/src/attention/tests.rs`:

* **The protocol always wins.** While the protocol latch is set, output, silence,
  notifications and bells do not change the state. It holds until the next report,
  user input (rule 3), the agent exiting (7), or the shell's D mark (8).
* **But a report cannot hide a request.** Reports are spoofable (anything the pane
  prints can contain one). A spoofed `state=working` would otherwise swallow a real
  permission prompt's notification or bell until the user typed. Rule 2b keeps the
  state but marks the pane unread, and the dashboard ranks unread panes above
  working ones.
* **Waiting needs a signal.** Only a notification, a bell or a report produces
  WaitingForInput. Silence produces Idle, never "waiting". Output while waiting does
  not clear it (agents redraw their prompt); user input does.
* **Exit status.** Halyard does not spawn the agent (the shell does), so it can not
  wait for it. The status comes from the shell's D mark, which arrives just before or
  just after the foreground change is noticed; both orders give the same result.
* **Error** only comes from `state=error`. An agent exiting non-zero is Finished
  with that status.
* **No agent, no state.** Without an active agent, output, bells and notifications
  leave the state alone (it stays `Unknown`, or `Finished` after an agent exited).
  The last notification text is still recorded.

### Unread

Unread is set when the state becomes WaitingForInput or Finished while the pane is
not focused, or by a notification or bell that a protocol state held back (rule 2b),
and cleared when the pane gains focus. Becoming Working, Idle or Error never sets it.

### Dashboard

`Dashboard` keeps one `SessionSummary` per pane: agent kind, working directory, git
`HEAD` (read from `.git/HEAD`, following the `gitdir:` file of worktrees and
submodules, without running git), state, cause, since, last output, unread, and the
last notification text (sanitized, at most 256 characters). Order: waiting, then
error, then anything else that is unread, then working, then idle, then finished and
read, then unknown. Within a group, the longest in its state comes first.

The working directory comes from OSC 7, which is untrusted output. It is always
shown, but it is used as a directory on this machine (reading git, and later opening
a new pane there) only when both hold:

* the host in the URI is empty, `localhost`, or this machine's host name (compared
  without case, or by its first label), so a cwd reported from inside `ssh` is not
  looked up locally;
* the path was reported verbatim: sanitizing for display did not change it. A path
  with a control character in it (`new\nline` is a legal directory name) would be
  shown as a different path, so it is display only.

Git reads only regular files: `HEAD` and `.git` files are type-checked before and
after a non-blocking open, so a FIFO planted at `.git/HEAD` cannot hang the reader,
and at most 4 KiB is read.

## Setting up agents

Every agent below already works without setup: Halyard sees its process, its output
and its bell. Setup makes "needs input" and "done" reliable.

### Claude Code

Verified against the hooks reference at https://code.claude.com/docs/en/hooks
(fetched 2026-11-22) and against the `terminalSequence` validator shipped in Claude
Code 2.1.325. Not yet verified by a live Claude Code session in Halyard.

Claude Code runs hooks **without a controlling terminal**, so a hook cannot write to
`/dev/tty` (the docs say so explicitly). Instead a hook prints JSON with a
`terminalSequence` field and Claude Code writes the sequence. That field accepts only
OSC 0, 1, 2, 9, 99, 777 and BEL (OSC 9 bodies may not start with a digit), so the
hook uses the protocol's OSC 777 carrier. The hook needs to run in the foreground
(no `async`), and Claude Code ignores the field in `-p` mode and in the Agent SDK.

1. Copy `crates/halyard-agents/hooks/claude-code-hook.sh` to
   `~/.config/halyard/claude-code-hook.sh` and make it executable. It prints nothing
   unless `HALYARD_SESSION_ID` is set, so it is harmless in other terminals.
2. Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh working" }] }
    ],
    "Notification": [
      { "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh waiting" }] },
      { "matcher": "elicitation_dialog", "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh waiting" }] },
      { "matcher": "agent_needs_input", "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh waiting" }] },
      { "matcher": "idle_prompt", "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh waiting" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh done" }] }
    ],
    "StopFailure": [
      { "hooks": [{ "type": "command", "command": "$HOME/.config/halyard/claude-code-hook.sh error" }] }
    ]
  }
}
```

The notification types (`permission_prompt`, `idle_prompt`, `elicitation_dialog`,
`agent_needs_input`, ...), the Stop event ("when Claude finishes responding"),
StopFailure ("when the turn ends due to an API error", which still honors
`terminalSequence`) and UserPromptSubmit come from the hooks reference. The hook
script and its output are tested in `crates/halyard-agents/tests/claude_hook.rs`
(the output is run through the scanner, the attention machine, and a model of
Claude Code's allowlist). Unverified: that `$HOME` expands in hook commands (hooks
run through a shell per the docs' examples, but this was not run live).

Without hooks, Claude Code's own terminal notifications still reach Halyard when its
notification channel (`preferredNotifChannel`) is iTerm2 (OSC 9), Ghostty (OSC 777)
or the terminal bell: each moves an active agent to WaitingForInput. The Kitty
channel (OSC 99) is not parsed by Halyard yet. Which channel `auto` picks inside
Halyard was not checked.

### Codex CLI

Verified against the configuration reference and sample at
https://learn.chatgpt.com/docs/config-file/config-reference and
https://learn.chatgpt.com/docs/config-file/config-sample (redirected from
developers.openai.com/codex, fetched 2026-11-22). Not verified by a live Codex
session.

Codex's TUI can emit OSC 9 notifications itself. In `~/.codex/config.toml`:

```toml
[tui]
notifications = ["agent-turn-complete", "approval-requested"]
notification_method = "osc9"
# The default, "unfocused", relies on focus reporting; Halyard tracks focus itself.
notification_condition = "always"
```

Each OSC 9 moves an active Codex pane to WaitingForInput (a finished turn is also
waiting for the user). The key names and values (`notifications` as a boolean or a
list of event types, `notification_method = auto | osc9 | bel`,
`notification_condition = unfocused | always`) are from the reference.

**Unverified:** Codex's top-level `notify = [...]` runs an external program with a
JSON payload. The official reference does not document the payload; community
write-ups say it fires only for `agent-turn-complete` and carries `type`, `cwd` and
`last-assistant-message`. Whether that program has a controlling terminal (and so
could write a Halyard report to `/dev/tty`) was not checked. The `[tui]` route above
needs none of this.

### Any other program

Print the protocol sequence (see `docs/protocol.md`), guarded on
`HALYARD_SESSION_ID`:

```sh
[ -n "$HALYARD_SESSION_ID" ] && printf '\033]7717;state=waiting;agent=mytool;session=%s\033\\' "$HALYARD_SESSION_ID"
```
