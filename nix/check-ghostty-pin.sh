#!/usr/bin/env bash
# Check that the Ghostty commit pinned in flake.nix is the one libghostty-vt-sys
# builds.
#
# Usage: check-ghostty-pin.sh <flake-ghostty-rev> <path to libghostty-vt-sys build.rs>
#
# The flake builds Ghostty from its own `ghostty` input (no network), while a
# plain `cargo build` clones the commit hard-coded as GHOSTTY_COMMIT in the
# libghostty-vt-sys build script. If the two drift, Nix users get a different
# VT engine than everyone else, silently. `nix flake check` runs this through
# nix/checks/ghostty-pin.nix against the version in Cargo.lock.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <flake-ghostty-rev> <libghostty-vt-sys build.rs>" >&2
  exit 2
fi

expected="$1"
build_rs="$2"

if [ ! -f "$build_rs" ]; then
  echo "error: $build_rs does not exist" >&2
  exit 2
fi

pinned="$(sed -n 's/^const GHOSTTY_COMMIT: &str = "\([0-9a-f]\{40\}\)";$/\1/p' "$build_rs")"

if [ -z "$pinned" ]; then
  echo "error: no GHOSTTY_COMMIT constant found in $build_rs" >&2
  exit 2
fi

if [ "$pinned" != "$expected" ]; then
  cat >&2 <<EOF
error: Ghostty pins disagree.
  flake.nix (input "ghostty"):          $expected
  libghostty-vt-sys GHOSTTY_COMMIT:     $pinned
Point the ghostty input in flake.nix at $pinned, then run
  nix flake lock --update-input ghostty
EOF
  exit 1
fi

echo "ok: flake Ghostty input and libghostty-vt-sys both pin $pinned"
