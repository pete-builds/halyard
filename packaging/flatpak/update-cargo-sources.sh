#!/usr/bin/env bash
# Regenerate packaging/flatpak/cargo-sources.json from Cargo.lock.
#
# flatpak-builder runs the build with no network, so every crate in Cargo.lock
# has to be listed as a source it downloads beforehand. flatpak-cargo-generator
# (flatpak/flatpak-builder-tools, MIT) does that conversion. This script fetches
# one pinned commit of it, checks the file's sha256, runs it in a throwaway
# virtualenv, and writes the result next to the manifest.
#
# Run it after every change to Cargo.lock:
#   packaging/flatpak/update-cargo-sources.sh
# check-sources.py (run by CI) fails when the two have drifted.
#
# Needs: bash, curl, python3 (3.9 or newer) with venv, network access.
set -euo pipefail

GENERATOR_COMMIT="41c20aa10819cdb2a4f3ca171758a96d1955c018"
GENERATOR_SHA256="0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380"
GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/${GENERATOR_COMMIT}/cargo/flatpak-cargo-generator.py"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$here/cargo-sources.json"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl --fail --silent --show-error --location --proto '=https' \
  -o "$work/flatpak-cargo-generator.py" "$GENERATOR_URL"

actual="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/flatpak-cargo-generator.py")"
if [ "$actual" != "$GENERATOR_SHA256" ]; then
  echo "error: flatpak-cargo-generator.py sha256 is $actual, expected $GENERATOR_SHA256" >&2
  exit 1
fi

python3 -m venv "$work/venv"
"$work/venv/bin/pip" install --quiet --disable-pip-version-check \
  'aiohttp==3.13.2' 'tomlkit==0.13.3'

"$work/venv/bin/python" -I "$work/flatpak-cargo-generator.py" \
  "$repo/Cargo.lock" -o "$out.tmp"
mv "$out.tmp" "$out"

count="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "$out")"
echo "wrote $out ($count sources)"
