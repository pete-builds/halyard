#!/bin/sh
# Claude Code hook that reports state to Halyard (docs/agents.md, docs/protocol.md).
#
# Usage in a hook command: claude-code-hook.sh working|waiting|idle|done|error
#
# Claude Code runs hooks without a controlling terminal, so the hook cannot write to
# /dev/tty. It returns the sequence in the `terminalSequence` JSON field instead, and
# Claude Code writes it. That field only allows OSC 0, 1, 2, 9, 99, 777 and BEL, so
# the report uses the OSC 777 carrier of the Halyard protocol.
#
# Outside Halyard (HALYARD_SESSION_ID unset) it prints nothing.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

# Drain the event JSON Claude Code sends on stdin.
cat >/dev/null

[ -n "${HALYARD_SESSION_ID-}" ] || exit 0

case "${1-}" in
  working|waiting|idle|done|error) state=$1 ;;
  *) exit 0 ;;
esac

# Halyard session ids only contain A-Z a-z 0-9 . _ - (docs/protocol.md); refuse
# anything else rather than build broken JSON.
case "$HALYARD_SESSION_ID" in
  *[!A-Za-z0-9._-]*) exit 0 ;;
esac

printf '{"terminalSequence":"\\u001b]777;halyard;state=%s;agent=claude;session=%s\\u0007"}\n' \
  "$state" "$HALYARD_SESSION_ID"
