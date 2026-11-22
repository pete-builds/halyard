# The Halyard session protocol

Version 1. Status: implemented in `halyard-shell` (parsing) and `halyard-agents` (state);
the UI that shows it is not wired yet (see `ROADMAP.md`).

Programs running in a Halyard pane can tell Halyard what they are doing with one escape
sequence. Halyard never needs it (agent state is derived from observable signals, see
`docs/agents.md` and ADR 0004), but when a program reports, the report wins.

## Environment

Halyard sets these in every pane:

| Variable | Value |
|----------|-------|
| `TERM_PROGRAM` | `Halyard` |
| `HALYARD_SESSION_ID` | An opaque id for the pane, 1 to 128 characters from `A-Z a-z 0-9 . _ -`. Stable for the life of the pane. |

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
| `session` | The `HALYARD_SESSION_ID` the program saw. When present and different from the pane's id, the whole report is ignored. |
| `exit` | A decimal exit status that goes with `state=done` or `state=error`. |
| `v` | Protocol version. Optional; `1` is assumed. Reserved for later use. |

Unknown keys are ignored, a repeated key takes its last value, and a report with no
recognized field is dropped. Examples:

```sh
printf '\033]7717;state=working;agent=mybot;session=%s\033\\' "$HALYARD_SESSION_ID"
printf '\033]7717;state=waiting;msg=Approve%%20the%%20migration%%3F\033\\'
printf '\033]7717;state=done;exit=0\033\\'
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
* does not treat `session=` as authentication. It only filters crossed wires.

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
| `OSC 7 ; file://host/path` | Working directory, percent-encoded. |
| `OSC 9 ; message` | Notification, unless the message is a ConEmu subcommand (`1` to `12` followed by `;` or the end). |
| `OSC 9 ; 4 ; state ; percent` | Progress. |
| `OSC 777 ; notify ; title ; body` | Notification. |
| `OSC 0`, `1`, `2` | Titles. |
| `BEL` | Bell. |
| `CSI ? 1049 h/l` (also 1047, 47) | Alternate screen. |
