# ADR 0004: Agent awareness from observable signals, ACP optional

Status: accepted, 2026-11-22

## Decision

Coding agents (Claude Code, Codex CLI, OpenCode, Gemini CLI, any other) run as ordinary
terminal programs. Halyard derives agent state only from signals it can observe:

- the foreground process of the pane's PTY (process name and command line),
- desktop notification escapes (OSC 9, OSC 777) and the bell,
- OSC 133 prompt marks (the shell came back, with an exit status),
- output activity and idle time.

Structured integration through the Agent Client Protocol is an optional extra for agents
that speak it, never a requirement.

## Why

- Every CLI agent already works in a terminal. Requiring an integration first would make
  Halyard worse at the thing it exists to host.
- The brief forbids inventing states or token counts. Each state shown in the UI maps to a
  named signal, and the UI says "unknown" rather than guess.

## Consequences

- "Needs input" is reliable only when the agent emits a notification or bell. Halyard ships
  setup notes for the agents that support notification hooks.
- The attention model is testable headlessly: feed escape sequences, assert state.
