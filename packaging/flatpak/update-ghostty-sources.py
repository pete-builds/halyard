#!/usr/bin/env python3
"""Regenerate packaging/flatpak/ghostty-sources.json.

libghostty-vt-sys builds Ghostty's VT library with Zig. Left alone it clones
Ghostty and lets Zig download Ghostty's package dependencies, and
flatpak-builder has no network during the build. This script lists all of it
as flatpak-builder sources instead:

1. Ghostty itself, as a git source at the commit libghostty-vt-sys pins
   (GHOSTTY_COMMIT in its build.rs, for the version in Cargo.lock). The
   manifest points GHOSTTY_SOURCE_DIR at it.
2. Every Zig package Ghostty depends on, from Ghostty's own build.zig.zon.json
   at that commit (the zon2nix output its build.zig.zon.nix is generated from,
   which the Nix flake uses). Each one is unpacked to
   ghostty-zig-packages/<zig package hash>, the layout `zig build --system`
   reads. The manifest points GHOSTTY_ZIG_SYSTEM_DIR at that directory.

Archives are downloaded here to record their sha256 (the Nix hashes in
build.zig.zon.json are NAR hashes for unpacked packages, not file hashes) and
to check how they unpack: Zig strips a single top-level directory, and so does
flatpak-builder by default, so an archive without exactly one gets
strip-components 0.

Run after bumping libghostty-vt in Cargo.lock:
    python3 packaging/flatpak/update-ghostty-sources.py
Needs Python 3.11 or newer and network access. .tar.zst archives need Python
3.14 or the `zstd` program.
"""

from __future__ import annotations

import io
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pins  # noqa: E402


def archive_type(url: str, data: bytes) -> str:
    """flatpak-builder archive-type, from the file's magic bytes."""
    if data[:2] == b"\x1f\x8b":
        return "tar-gzip"
    if data[:6] == b"\xfd7zXZ\x00":
        return "tar-xz"
    if data[:4] == b"\x28\xb5\x2f\xfd":
        return "tar-zst"
    if data[:3] == b"BZh":
        return "tar-bzip2"
    if data[:4] == b"PK\x03\x04":
        return "zip"
    raise SystemExit(f"error: unknown archive format for {url}")


def member_names(kind: str, data: bytes) -> list[str]:
    if kind == "zip":
        import zipfile

        with zipfile.ZipFile(io.BytesIO(data)) as z:
            return z.namelist()
    if kind == "tar-zst":
        try:
            with tarfile.open(fileobj=io.BytesIO(data), mode="r:zst") as tar:
                return tar.getnames()
        except tarfile.CompressionError:
            zstd = shutil.which("zstd")
            if zstd is None:
                raise SystemExit("error: a .tar.zst archive needs Python 3.14 or the zstd program")
            raw = subprocess.run([zstd, "-dc"], input=data, capture_output=True, check=True).stdout
            with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as tar:
                return tar.getnames()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:*") as tar:
        return tar.getnames()


def single_root(names: list[str]) -> bool:
    """True when every entry sits under one top-level directory."""
    roots = set()
    has_nested = False
    for name in names:
        parts = [p for p in name.split("/") if p not in ("", ".")]
        if not parts:
            continue
        roots.add(parts[0])
        has_nested |= len(parts) > 1
    return len(roots) == 1 and has_nested


def git_source(url: str, dest: str) -> dict:
    # zon2nix writes git dependencies as git+https://host/repo[?ref=x]#<commit>.
    bare = url.removeprefix("git+")
    base, _, commit = bare.partition("#")
    base = base.split("?", 1)[0]
    if len(commit) != 40 or any(c not in "0123456789abcdef" for c in commit):
        raise SystemExit(f"error: git dependency {url} is not pinned to a full commit")
    return {"type": "git", "url": base, "commit": commit, "dest": dest}


def main() -> None:
    version, commit = pins.ghostty_commit_from_lock()
    print(f"libghostty-vt-sys {version} pins Ghostty {commit}")
    zon = json.loads(pins.fetch(f"{pins.GHOSTTY_RAW}/{commit}/build.zig.zon.json"))

    sources: list[dict] = [
        {
            "type": "git",
            "url": pins.GHOSTTY_GIT,
            "commit": commit,
            "dest": pins.GHOSTTY_DEST,
        }
    ]
    for zig_hash in sorted(zon):
        entry = zon[zig_hash]
        url = entry["url"]
        dest = f"{pins.ZIG_PACKAGES_DEST}/{zig_hash}"
        if url.startswith("git+"):
            sources.append(git_source(url, dest))
            print(f"  git      {entry['name']}")
            continue
        if urlparse(url).scheme != "https":
            raise SystemExit(f"error: {entry['name']} is not fetched over HTTPS: {url}")
        data = pins.fetch(url)
        kind = archive_type(url, data)
        src = {"type": "archive", "archive-type": kind, "url": url, "sha256": pins.sha256(data), "dest": dest}
        if not single_root(member_names(kind, data)):
            src["strip-components"] = 0
        sources.append(src)
        print(f"  archive  {entry['name']} ({len(data)} bytes)")

    out = pins.GHOSTTY_SOURCES
    tmp = out.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(sources, indent=2) + "\n")
    tmp.replace(out)
    print(f"wrote {out} ({len(sources)} sources)")


if __name__ == "__main__":
    main()
