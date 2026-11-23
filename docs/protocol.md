# The Halyard session protocol

Version 1. Status: implemented in `halyard-shell` (parsing) and `halyard-agents` (state),
and shown by the app on each pane's tab and badge (see `docs/agents.md`).

Programs running in a Halyard pane can tell Halyard what they are doing with one escape
sequence. Halyard never needs it (agent state is derived from observable signals, see
`docs/agents.md` and ADR 0004), but when a program reports, the report wins.

## Environment

Halyard sets these in every pane:

| Variable | Value |
|----------|-------|
| `TERM_PROGRAM` | `Halyard` |
| `HALYARD_SESSION_ID` | An opaque id for the pane, 1 to 128 characters from `A-Z a-z 0-9 . _ -`. Stable for the life of the pane. |
| `PATH` | The inherited `PATH`, with the directory of the running `halyard` appended when `halyard-msg` is installed there, so hooks find it. Left as it is when `PATH` already finds a `halyard-msg` or other users could change that directory; the full rules are in `docs/session-api.md`. Not in the Flatpak, whose panes run on the host. |

`HALYARD_SESSION_ID` exists because agent hook payloads carry no terminal or tab
identifier. A hook that echoes the id back lets Halyard drop reports that reached the
wrong pane (for example through a shared tmux server).

Scripts should do nothing when `HALYARD_SESSION_ID` is unset.

## The sequence

```
ESC ] 7717 ; key=value ; key=value ... ST
```

`ST` is `ESC \`; `BEL` (0x07) is accepted as a terminator too. Fields are separated by
`;`, keys are lowercase ASCII, values are UTF-8 with these bytes percent-encoded as
`%HH`: `%`, `;`, and every byte below 0x20 or equal to 0x7f. Decoders accept any
`%HH`.

| Key | Meaning |
|-----|---------|
| `state` | `working`, `waiting` (needs the user), `idle`, `done`, or `error`. Any other value is ignored. |
| `agent` | The program's name for itself, for example `claude` or `codex`. Known names map to Halyard's agent kinds; anything else is shown as given. At most 64 characters. |
| `msg` | A short message for the user. At most 1024 characters are kept; the dashboard shows 256. |
| `session` | The `HALYARD_SESSION_ID` the program saw. **Required by the app:** a report without it, or with another pane's id, is treated as plain output (no state, no agent name, no message, no notification). |
| `exit` | A decimal exit status that goes with `state=done` or `state=error`. |
| `v` | Protocol version. Optional; `1` is assumed. Reserved for later use. |

Unknown keys are ignored, a repeated key takes its last value, and a report with no
recognized field is dropped. Examples:

```sh
printf '\033]7717;state=working;agent=mybot;session=%s\033\\' "$HALYARD_SESSION_ID"
printf '\033]7717;state=waiting;msg=Approve%%20the%%20migration%%3F;session=%s\033\\' "$HALYARD_SESSION_ID"
printf '\033]7717;state=done;exit=0;session=%s\033\\' "$HALYARD_SESSION_ID"
```

### The OSC 777 carrier

Some hosts only let a plugin emit notification sequences. Claude Code is one: a hook's
`terminalSequence` output must be OSC 0, 1, 2, 9, 99, 777 or BEL. For these, Halyard
accepts the same fields inside OSC 777:

```
ESC ] 777 ; halyard ; key=value ; ... ST
```

OSC 777's first field names a urxvt extension (`notify` is the one other terminals
know); `halyard` is a new extension name, so terminals that do not know it ignore the
sequence. It is parsed exactly like OSC 7717.

## Why 7717

* It is not assigned by xterm's ctlseqs, iTerm2 (which uses 1337), kitty (5113, 5522,
  30001, 99 and others), VS Code (633), WezTerm, foot, FinalTerm (133), urxvt (777),
  ConEmu or Windows Terminal (9), or systemd (3008), as far as their published lists
  and a web search on 2026-11-22 show. No hit for "OSC 7717" was found anywhere.
* Four digits keeps it away from the low numbers xterm keeps adding.
* Terminals ignore OSC numbers they do not know, so emitting it outside Halyard is
  harmless. Scripts should still check `TERM_PROGRAM` or `HALYARD_SESSION_ID` first.

## Trust

Every byte a pane prints is untrusted, including these reports: any program in the
pane, or a file that is `cat`ed, can print them. Halyard therefore:

* caps the whole sequence (8 KiB) and each value, strips control and bidirectional
  override characters from every value, and parses nothing beyond the fields above;
* uses a report only to set the pane's displayed state, agent name and message. A
  report never runs anything, never focuses a pane, and never reaches a model;
* in the app, acts on a report only when it carries `session=` with this pane's
  `HALYARD_SESSION_ID`. A file that is `cat`ed cannot know the id (it is random per
  pane), so its reports are plain output; a hook started in the pane gets the id
  from its environment. This is a filter, not authentication: any program running
  in the pane can read the id and report;
* sends desktop notifications with text stripped of control characters, bidi
  overrides and isolates, direction marks and zero-width characters, and capped;
  on Linux the text is also markup-escaped (`&`, `<`, `>`), because notification
  servers render markup such as `<a href>` and `<img>`. Tested in
  `crates/halyard-agents/src/pane/tests.rs`
  (`reports_without_this_panes_session_are_plain_output`) and
  `crates/halyard-app/src/notify.rs` (`markup_is_escaped_for_the_notification_server`).

A spoofed report can do two things, and both are bounded:

* **Fake a state**, for example show a pane as waiting. It goes away at the next
  user input, report, agent exit, or end-of-command mark.
* **Hide a real request.** While a report's state holds, notifications and bells do
  not change the state ("the protocol wins"), so a spoofed `state=working` printed
  just before a real permission prompt would keep the pane looking busy. To limit
  this, a notification or bell that arrives while a report's state holds still marks
  the pane unread (when it is not focused), and unread panes sort above working ones
  in the dashboard. Tested in
  `crates/halyard-agents/src/attention/tests.rs`
  (`protocol_state_cannot_hide_a_real_notification`).

## Shell integration sequences

For reference, the other sequences Halyard reads (all parsed by
`crates/halyard-shell/src/scanner.rs`):

| Sequence | Meaning |
|----------|---------|
| `OSC 133 ; A [; aid=ID]` | Prompt start. `aid` identifies the shell, which lets nested shells be told apart. |
| `OSC 133 ; B` | Prompt end, input start. |
| `OSC 133 ; C [; cmdline_url=...]` | Command output start. |
| `OSC 133 ; D [; status] [; aid=ID]` | Command finished. |
| `OSC 633 ; A`, `B`, `C`, `D` | VS Code's equivalents. |
| `OSC 633 ; E ; cmdline [; nonce]` | Command line, with `\\` and `\xHH` escaping. The nonce is ignored. |
| `OSC 633 ; P ; Cwd=path` | Working directory. |
| `OSC 7 ; file://host/path` | Working directory, percent-encoded. Used locally only when the host is this machine and the path survived sanitizing unchanged (see `docs/agents.md`). |
| `OSC 9 ; message` | Notification, unless the message is a ConEmu subcommand (`1` to `12` followed by `;` or the end). |
| `OSC 9 ; 4 ; state ; percent` | Progress. |
| `OSC 777 ; notify ; title ; body` | Notification. |
| `OSC 0`, `1`, `2` | Titles. |
| `BEL` | Bell. |
| `CSI ? 1049 h/l` (also 1047, 47) | Alternate screen. |

How the block model reads them (`crates/halyard-shell/src/blocks.rs`):

* A and B repeated with no C in between are one prompt drawn again, never a new or
  empty block. fish 4 repaints its prompt this way (after `cd`, and while it waits
  for answers to the terminal queries it sends at every prompt).
* Shells are told apart by `aid=`. Shells that send none count as one more shell,
  so fish 4's own marks (no `aid`) nest correctly with Halyard's zsh and bash
  scripts (which send `aid=$$`) in either direction.
* fish: fish 4 emits OSC 133 itself unless `fish_features` has `no-mark-prompt`;
  Halyard's fish script then adds only OSC 7, and emits the marks itself on fish 3
  or with marking turned off. fish 4.0.0 sends no command text, fish 4.0 to 4.2
  send no B mark, and fish 4.8 and later turn marking off when they believe they
  run in Konsole.
