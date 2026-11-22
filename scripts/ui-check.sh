#!/usr/bin/env bash
# Drive a real Halyard window with the debug-script harness and check the
# result: each script in crates/halyard-app/ui-tests/ (or the ones named as
# arguments) runs against its own config and state directory, and fails when
# one of its `expect`/`reject` checks fails, a step fails, or it hangs.
#
# macOS only: the harness posts mouse events through AppKit.
#
#   scripts/ui-check.sh                      build (debug) and run every script
#   scripts/ui-check.sh path/to/script.txt   run some
#   HALYARD_BIN=target/debug/halyard scripts/ui-check.sh   skip the build
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
if [[ $(uname -s) != Darwin ]]; then
    echo "ui-check: macOS only (the harness posts mouse events through AppKit)" >&2
    exit 2
fi
bin=${HALYARD_BIN:-}
if [[ -z $bin ]]; then
    (cd "$root" && cargo build --locked -p halyard-app --features debug-script)
    bin=$root/target/debug/halyard
fi
limit=${UI_CHECK_TIMEOUT:-90}

scripts=("$@")
if [[ ${#scripts[@]} -eq 0 ]]; then
    scripts=("$root"/crates/halyard-app/ui-tests/*.txt)
fi

work=$(mktemp -d /tmp/halyard-ui.XXXXXX)
trap 'rm -rf "$work"' EXIT
# A plain shell with a fixed prompt, so checks can name what the shell got.
cat >"$work/config.toml" <<'TOML'
[terminal]
shell = { program = "/bin/sh" }
TOML

# The display must be awake for the window server to deliver events.
caffeinate -u -t 1 2>/dev/null || true

failed=0
for script in "${scripts[@]}"; do
    name=$(basename "$script")
    run=$work/${name%.txt}
    mkdir -p "$run/state" "$run/tmp"
    log=$run/log
    HALYARD_DEBUG_SCRIPT=$script HALYARD_CONFIG=$work/config.toml \
        HALYARD_STATE_DIR=$run/state TMPDIR=$run/tmp/ XDG_RUNTIME_DIR=$run/tmp \
        SHELL=/bin/sh PS1='$ ' ENV='' BASH_SILENCE_DEPRECATION_WARNING=1 \
        "$bin" >"$log" 2>&1 &
    pid=$!
    # Poll instead of a watchdog process, so nothing is left behind.
    ticks=$((limit * 10))
    while kill -0 "$pid" 2>/dev/null && ((ticks > 0)); do
        sleep 0.1
        ticks=$((ticks - 1))
    done
    status=0
    if kill -0 "$pid" 2>/dev/null; then
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        echo "halyard: timed out after ${limit}s" >>"$log"
        status=124
    else
        wait "$pid" || status=$?
    fi
    if [[ $status -eq 0 ]] && ! grep -aq "check failed\|step failed" "$log"; then
        echo "ok    $name"
    else
        echo "FAIL  $name (status $status)"
        sed 's/^/      /' "$log"
        failed=1
    fi
done
exit "$failed"
