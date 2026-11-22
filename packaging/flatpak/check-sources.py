#!/usr/bin/env python3
"""Fail when the Flatpak's generated source lists differ from what they must be.

flatpak-builder trusts these files completely: a source can run commands
(`type: shell`), and the cargo config it writes can point Cargo at any
program. So every entry is checked against an authority, not just counted:

1. cargo-sources.json holds exactly one archive and one checksum file per
   crates.io package in Cargo.lock, each with Cargo.lock's checksum and the
   crate's static.crates.io URL, plus one cargo/config with the exact
   expected contents. Fix: update-cargo-sources.sh.
2. ghostty-sources.json holds Ghostty as a git source at the commit
   libghostty-vt-sys (the version in Cargo.lock) pins, and one source per Zig
   package in Ghostty's build.zig.zon.json at that commit, with that file's
   URL (and git commit, for git packages). Fix: update-ghostty-sources.py.
3. Only `archive`, `git` and `inline` sources appear, with only the fields
   those scripts write.
4. The manifest reads both files and points the build at what they fill.

sha256 values of Zig archives cannot be checked without downloading them;
flatpak-builder refuses a download whose sha256 differs, and the URL check
above stops a swap to a different archive.

Downloads two small files (the libghostty-vt-sys crate and Ghostty's
build.zig.zon.json). Needs Python 3.11 or newer. test_check_sources.py proves
each check fails on a hand edit.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pins  # noqa: E402

MANIFEST = pins.HERE / "io.github.pete_builds.Halyard.yml"
BUILD_DIR = "/run/build/halyard"

# What flatpak-cargo-generator writes for crates.io sources only.
CARGO_CONFIG = (
    '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
    '[source.crates-io]\nreplace-with = "vendored-sources"\n'
)

ALLOWED_FIELDS = {
    "archive": {"type", "archive-type", "url", "sha256", "dest", "strip-components"},
    "git": {"type", "url", "commit", "dest"},
    "inline": {"type", "contents", "dest", "dest-filename"},
}
ARCHIVE_TYPES = {"tar-gzip", "tar-xz", "tar-zst", "tar-bzip2", "zip"}
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")


def check_fields(name: str, src: dict, allowed_types: set[str], errors: list[str]) -> bool:
    """True when the source has an allowed type and only that type's fields."""
    kind = src.get("type")
    if kind not in allowed_types:
        errors.append(f"{name}: source type {kind!r} is not allowed here: {src}")
        return False
    extra = set(src) - ALLOWED_FIELDS[kind]
    if extra:
        errors.append(f"{name}: unexpected fields {sorted(extra)} in {src.get('dest')}")
        return False
    return True


def check_cargo(sources: list, lock_packages: list[dict], errors: list[str]) -> None:
    name = "cargo-sources.json"
    wanted = {f"cargo/vendor/{p['name']}-{p['version']}": p for p in lock_packages}
    archives: set[str] = set()
    checksums: set[str] = set()
    configs = 0
    for src in sources:
        if not check_fields(name, src, {"archive", "inline"}, errors):
            continue
        dest = src.get("dest")
        if src["type"] == "archive":
            pkg = wanted.get(dest)
            if pkg is None:
                errors.append(f"{name} lists {dest}, which Cargo.lock does not have")
                continue
            n, v = pkg["name"], pkg["version"]
            if src.get("url") != f"https://static.crates.io/crates/{n}/{n}-{v}.crate":
                errors.append(f"{name}: {dest} is not fetched from its crates.io URL")
            if src.get("sha256") != pkg["checksum"]:
                errors.append(f"{name} has a different sha256 for {dest}")
            if src.get("archive-type") != "tar-gzip" or "strip-components" in src:
                errors.append(f"{name}: {dest} is not unpacked as a plain .crate")
            if dest in archives:
                errors.append(f"{name} lists {dest} twice")
            archives.add(dest)
            continue
        filename = src.get("dest-filename")
        if dest == "cargo" and filename == "config":
            configs += 1
            if src.get("contents") != CARGO_CONFIG:
                errors.append(f"{name}: cargo/config differs from the expected vendoring config")
            continue
        pkg = wanted.get(dest)
        if pkg is None or filename != ".cargo-checksum.json":
            errors.append(f"{name}: unexpected inline file {dest}/{filename}")
            continue
        try:
            contents = json.loads(src.get("contents", ""))
        except json.JSONDecodeError:
            contents = None
        if contents != {"package": pkg["checksum"], "files": {}}:
            errors.append(f"{name}: {dest}/.cargo-checksum.json does not match Cargo.lock")
        if dest in checksums:
            errors.append(f"{name} lists {dest}/.cargo-checksum.json twice")
        checksums.add(dest)
    for dest in sorted(wanted.keys() - archives):
        errors.append(f"{name} is missing {dest}")
    for dest in sorted(wanted.keys() - checksums):
        errors.append(f"{name} is missing {dest}/.cargo-checksum.json")
    if configs != 1:
        errors.append(f"{name} must write cargo/config exactly once, not {configs} times")
    print(f"cargo: {len(wanted)} crates in Cargo.lock, {len(archives)} in {name}")


def zon_git(url: str) -> tuple[str, str]:
    """(repository URL, commit) from zon2nix's git+https://host/repo[?ref=x]#commit."""
    base, _, commit = url.removeprefix("git+").partition("#")
    return base.split("?", 1)[0], commit


def check_ghostty(sources: list, commit: str, zon: dict, errors: list[str]) -> None:
    name = "ghostty-sources.json"
    prefix = pins.ZIG_PACKAGES_DEST + "/"
    ghostty = 0
    listed: set[str] = set()
    for src in sources:
        if not check_fields(name, src, {"archive", "git"}, errors):
            continue
        dest = src.get("dest", "")
        if dest == pins.GHOSTTY_DEST:
            ghostty += 1
            if (src["type"], src.get("url"), src.get("commit")) != ("git", pins.GHOSTTY_GIT, commit):
                errors.append(f"{name} does not build Ghostty {commit} from {pins.GHOSTTY_GIT}")
            continue
        if not dest.startswith(prefix):
            errors.append(f"{name}: unexpected destination {dest!r}")
            continue
        zig_hash = dest[len(prefix):]
        entry = zon.get(zig_hash)
        if entry is None:
            errors.append(f"{name} lists Zig package {zig_hash}, which Ghostty {commit} does not use")
            continue
        if zig_hash in listed:
            errors.append(f"{name} lists Zig package {zig_hash} twice")
        listed.add(zig_hash)
        url = entry["url"]
        if url.startswith("git+"):
            repo, rev = zon_git(url)
            if (src["type"], src.get("url"), src.get("commit")) != ("git", repo, rev):
                errors.append(f"{name}: Zig package {entry['name']} is not {repo} at {rev}")
        else:
            if src["type"] != "archive" or src.get("url") != url:
                errors.append(f"{name}: Zig package {entry['name']} is not fetched from {url}")
            if not SHA256.match(str(src.get("sha256", ""))):
                errors.append(f"{name}: Zig package {entry['name']} has no valid sha256")
            if src.get("archive-type") not in ARCHIVE_TYPES:
                errors.append(f"{name}: Zig package {entry['name']} has an unexpected archive-type")
            if src.get("strip-components", 1) not in (0, 1):
                errors.append(f"{name}: Zig package {entry['name']} has an unexpected strip-components")
        if src["type"] == "git" and not COMMIT.match(str(src.get("commit", ""))):
            errors.append(f"{name}: {src.get('url')} is not pinned to a commit")
    if ghostty != 1:
        errors.append(f"{name} must hold Ghostty exactly once, not {ghostty} times")
    for h in sorted(zon.keys() - listed):
        errors.append(f"{name} is missing Zig package {zon[h]['name']} ({h})")
    print(f"ghostty: pinned {commit}; {len(zon)} Zig packages, {len(listed)} listed")


def check_manifest(text: str, errors: list[str]) -> None:
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


def authorities() -> tuple[list[dict], str, dict]:
    """Cargo.lock's packages, the Ghostty pin, and Ghostty's Zig package list."""
    _version, commit = pins.ghostty_commit_from_lock()
    zon = json.loads(pins.fetch(f"{pins.GHOSTTY_RAW}/{commit}/build.zig.zon.json"))
    return pins.registry_packages(), commit, zon


def check_all(cargo: list, ghostty: list, manifest: str, auth: tuple[list[dict], str, dict]) -> list[str]:
    lock_packages, commit, zon = auth
    errors: list[str] = []
    check_cargo(cargo, lock_packages, errors)
    check_ghostty(ghostty, commit, zon, errors)
    check_manifest(manifest, errors)
    return errors


def main() -> int:
    errors = check_all(
        json.loads(pins.CARGO_SOURCES.read_text()),
        json.loads(pins.GHOSTTY_SOURCES.read_text()),
        MANIFEST.read_text(),
        authorities(),
    )
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
