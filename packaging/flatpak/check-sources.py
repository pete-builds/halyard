#!/usr/bin/env python3
"""Fail when the Flatpak's generated source lists have drifted from the code.

Checks, without building anything:

1. cargo-sources.json lists exactly the crates.io packages in Cargo.lock, each
   with the checksum Cargo.lock records. Fix: update-cargo-sources.sh.
2. ghostty-sources.json builds the Ghostty commit that libghostty-vt-sys (at
   the version in Cargo.lock) pins, and lists exactly the Zig packages in
   Ghostty's build.zig.zon.json at that commit. Fix: update-ghostty-sources.py.
3. The manifest reads both files and points GHOSTTY_SOURCE_DIR and
   GHOSTTY_ZIG_SYSTEM_DIR at the directories they fill.

Downloads two small files (the libghostty-vt-sys crate and Ghostty's
build.zig.zon.json). Needs Python 3.11 or newer.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pins  # noqa: E402

MANIFEST = pins.HERE / "io.github.pete_builds.Halyard.yml"
BUILD_DIR = "/run/build/halyard"


def check_cargo(errors: list[str]) -> None:
    sources = json.loads(pins.CARGO_SOURCES.read_text())
    listed = {}
    for src in sources:
        if src["type"] == "archive":
            listed[src["dest"]] = src["sha256"]
    wanted = {}
    for pkg in pins.registry_packages():
        wanted[f"cargo/vendor/{pkg['name']}-{pkg['version']}"] = pkg["checksum"]
    for dest in sorted(wanted.keys() - listed.keys()):
        errors.append(f"cargo-sources.json is missing {dest}")
    for dest in sorted(listed.keys() - wanted.keys()):
        errors.append(f"cargo-sources.json lists {dest}, which Cargo.lock does not have")
    for dest in sorted(wanted.keys() & listed.keys()):
        if wanted[dest] != listed[dest]:
            errors.append(f"cargo-sources.json has a different sha256 for {dest}")
    print(f"cargo: {len(wanted)} crates in Cargo.lock, {len(listed)} in cargo-sources.json")


def check_ghostty(errors: list[str]) -> None:
    version, commit = pins.ghostty_commit_from_lock()
    sources = json.loads(pins.GHOSTTY_SOURCES.read_text())
    ghostty = [s for s in sources if s.get("dest") == pins.GHOSTTY_DEST]
    if len(ghostty) != 1 or ghostty[0].get("commit") != commit or ghostty[0].get("url") != pins.GHOSTTY_GIT:
        errors.append(
            f"ghostty-sources.json does not build Ghostty {commit}, "
            f"the commit libghostty-vt-sys {version} pins"
        )
    prefix = pins.ZIG_PACKAGES_DEST + "/"
    listed = {s["dest"][len(prefix):] for s in sources if s.get("dest", "").startswith(prefix)}
    zon = json.loads(pins.fetch(f"{pins.GHOSTTY_RAW}/{commit}/build.zig.zon.json"))
    for h in sorted(zon.keys() - listed):
        errors.append(f"ghostty-sources.json is missing Zig package {zon[h]['name']} ({h})")
    for h in sorted(listed - zon.keys()):
        errors.append(f"ghostty-sources.json lists Zig package {h}, which Ghostty {commit} does not use")
    for s in sources:
        if s["type"] == "archive" and not s.get("sha256"):
            errors.append(f"ghostty-sources.json: {s['url']} has no sha256")
        if s["type"] == "git" and len(s.get("commit", "")) != 40:
            errors.append(f"ghostty-sources.json: {s['url']} is not pinned to a commit")
    print(f"ghostty: libghostty-vt-sys {version} pins {commit}; {len(zon)} Zig packages, {len(listed)} listed")


def check_manifest(errors: list[str]) -> None:
    text = MANIFEST.read_text()
    needed = [
        pins.CARGO_SOURCES.name,
        pins.GHOSTTY_SOURCES.name,
        f"GHOSTTY_SOURCE_DIR: {BUILD_DIR}/{pins.GHOSTTY_DEST}",
        f"GHOSTTY_ZIG_SYSTEM_DIR: {BUILD_DIR}/{pins.ZIG_PACKAGES_DEST}",
        f"CARGO_HOME: {BUILD_DIR}/cargo",
    ]
    for needle in needed:
        if needle not in text:
            errors.append(f"{MANIFEST.name} does not contain `{needle}`")


def main() -> int:
    errors: list[str] = []
    check_cargo(errors)
    check_ghostty(errors)
    check_manifest(errors)
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    if errors:
        print("Regenerate with packaging/flatpak/update-cargo-sources.sh and "
              "packaging/flatpak/update-ghostty-sources.py (see docs/flatpak.md).", file=sys.stderr)
        return 1
    print("ok: Flatpak sources match Cargo.lock and the Ghostty pin")
    return 0


if __name__ == "__main__":
    sys.exit(main())
