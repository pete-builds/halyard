"""Shared helpers for the Flatpak source scripts: Cargo.lock and the Ghostty pin.

The Ghostty commit is not chosen here. libghostty-vt-sys hard-codes it as
GHOSTTY_COMMIT in its build script, and a plain `cargo build` clones exactly
that commit. The Flatpak build has no network, so it gets the same commit as a
flatpak-builder source instead, and the two must never drift.
"""

from __future__ import annotations

import hashlib
import io
import re
import tarfile
import tomllib
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
CARGO_LOCK = REPO / "Cargo.lock"
CARGO_SOURCES = HERE / "cargo-sources.json"
GHOSTTY_SOURCES = HERE / "ghostty-sources.json"

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
GHOSTTY_GIT = "https://github.com/ghostty-org/ghostty.git"
GHOSTTY_RAW = "https://raw.githubusercontent.com/ghostty-org/ghostty"

# Where the manifest's halyard module puts each source, relative to its build
# directory. The manifest's GHOSTTY_SOURCE_DIR and GHOSTTY_ZIG_SYSTEM_DIR point
# at these.
GHOSTTY_DEST = "ghostty"
ZIG_PACKAGES_DEST = "ghostty-zig-packages"

_COMMIT_RE = re.compile(r'^const GHOSTTY_COMMIT: &str = "([0-9a-f]{40})";$', re.M)


def fetch(url: str) -> bytes:
    """Download `url` over HTTPS and return the body."""
    if not url.startswith("https://"):
        raise SystemExit(f"error: refusing a non-HTTPS URL: {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "halyard-flatpak-scripts"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return resp.read()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def registry_packages() -> list[dict]:
    """Every crates.io package in Cargo.lock, as name, version and checksum."""
    lock = tomllib.loads(CARGO_LOCK.read_text())
    pkgs = []
    for pkg in lock.get("package", []):
        source = pkg.get("source")
        if source is None:
            continue  # workspace member or [patch] path crate
        if source != CRATES_IO:
            raise SystemExit(
                f"error: {pkg['name']} {pkg['version']} comes from {source}; "
                "only crates.io sources are handled here"
            )
        pkgs.append(pkg)
    return pkgs


def ghostty_commit_from_lock() -> tuple[str, str]:
    """(libghostty-vt-sys version, GHOSTTY_COMMIT) for the version in Cargo.lock.

    Downloads the crate from crates.io, checks it against the checksum Cargo.lock
    records, and reads the constant out of its build script.
    """
    matches = [p for p in registry_packages() if p["name"] == "libghostty-vt-sys"]
    if len(matches) != 1:
        raise SystemExit(f"error: expected one libghostty-vt-sys in Cargo.lock, found {len(matches)}")
    pkg = matches[0]
    name, version = pkg["name"], pkg["version"]
    data = fetch(f"https://static.crates.io/crates/{name}/{name}-{version}.crate")
    if sha256(data) != pkg["checksum"]:
        raise SystemExit(f"error: {name}-{version}.crate does not match its Cargo.lock checksum")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        member = tar.extractfile(f"{name}-{version}/build.rs")
        if member is None:
            raise SystemExit(f"error: {name}-{version} has no build.rs")
        build_rs = member.read().decode()
    found = _COMMIT_RE.findall(build_rs)
    if len(found) != 1:
        raise SystemExit(f"error: no single GHOSTTY_COMMIT constant in {name}-{version}/build.rs")
    return version, found[0]
