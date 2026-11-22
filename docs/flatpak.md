# Flatpak

The Flatpak package lets regular Linux desktops (Fedora, Ubuntu, Debian, Arch and others)
install Halyard without Nix and without compiling Rust or Zig. It sits alongside the native
Nix package (`flake.nix`), which stays the recommended way on NixOS.

Everything lives in `packaging/flatpak/`:

| File | What it is |
|------|------------|
| `io.github.pete_builds.Halyard.yml` | The manifest |
| `io.github.pete_builds.Halyard.desktop` | Desktop entry |
| `io.github.pete_builds.Halyard.metainfo.xml` | AppStream metadata |
| `cargo-sources.json` | Every crate in `Cargo.lock`, generated |
| `ghostty-sources.json` | Ghostty and its Zig packages, generated |
| `update-cargo-sources.sh`, `update-ghostty-sources.py` | Regenerate the two lists |
| `check-sources.py` | CI check that the lists match `Cargo.lock` and the Ghostty pin |
| `test_check_sources.py` | Controls: hand edits `check-sources.py` must catch |
| `smoke-test.sh` | Checks the built binary's libraries without a display |
| `check-permissions.sh` | CI check that the installed bundle has exactly the documented permissions |

**App ID.** `io.github.pete_builds.Halyard`. Flatpak IDs are reverse-DNS names whose
segments may not contain a hyphen, so the GitHub owner `pete-builds` is written
`pete_builds`, the form Flathub uses for `io.github.*` IDs.

## Install a CI build

Halyard is not on Flathub. Every pull request and every push to `main` builds a bundle:
open the `flatpak` workflow run, download the `halyard-x86_64.flatpak` (or `-aarch64`)
artifact and unzip it. Then:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./halyard.flatpak     # pulls org.freedesktop.Platform 26.08 from Flathub
flatpak run io.github.pete_builds.Halyard
```

The bundle is unsigned. Install builds from this repository's workflow runs only.

## Build it yourself

Needs `flatpak` and `flatpak-builder` (most distributions package both) and network access
for the download step. The build itself runs offline.

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --install \
    flatpak_app packaging/flatpak/io.github.pete_builds.Halyard.yml
flatpak run io.github.pete_builds.Halyard
```

To make a bundle file instead of installing:

```sh
flatpak-builder --user --install-deps-from=flathub --force-clean --repo=repo \
    flatpak_app packaging/flatpak/io.github.pete_builds.Halyard.yml
flatpak build-bundle repo halyard.flatpak io.github.pete_builds.Halyard
```

`flatpak_app/`, `repo/`, `.flatpak-builder/` and `*.flatpak` are ignored by git.

## How the offline build works

`flatpak-builder` downloads every source listed in the manifest, then builds with no
network. Halyard's build normally fetches three things, and each is listed instead:

1. **Crates.** `cargo-sources.json` holds every crates.io package in `Cargo.lock` with its
   checksum, plus a `cargo/config` that points crates.io at the vendored copies. The
   manifest sets `CARGO_HOME` to that directory and builds with `--offline --locked`.
   The `[patch.crates-io]` path crate in `vendor/xattr` needs no entry: it is part of the
   source tree, and Cargo uses it as it does in every other build.
2. **Ghostty.** `libghostty-vt-sys` builds Ghostty's VT library from source and would
   `git clone` the commit it pins (`GHOSTTY_COMMIT` in its build script, currently
   `a887df42c56f6de86c0fe6da9c4eeca37931e083`). `ghostty-sources.json` lists that commit as a
   git source and the manifest sets `GHOSTTY_SOURCE_DIR` to it.
3. **Ghostty's Zig packages.** Zig would download them. `ghostty-sources.json` lists each
   one, from Ghostty's own `build.zig.zon.json` at the pinned commit (the zon2nix output that
   Ghostty's `build.zig.zon.nix`, and so the Nix flake, is generated from), unpacked to
   `ghostty-zig-packages/<package hash>`. The manifest sets `GHOSTTY_ZIG_SYSTEM_DIR` there,
   which makes `libghostty-vt-sys` run `zig build --system <dir>`. Archives that do not
   unpack to a single top-level directory get `strip-components: 0`, matching how Zig
   unpacks them. The list matches Ghostty's own `flatpak/zig-packages.json` at that commit
   entry for entry (URLs, sha256 and the git commit), which was checked when it was generated.

Zig itself (0.15.2 exactly, which Ghostty requires) is a build-only module: the official
tarballs for x86_64 and aarch64, pinned by the sha256 that `ziglang.org/download/index.json`
publishes. It is removed from the finished app. Rust comes from the
`org.freedesktop.Sdk.Extension.rust-stable` extension (Rust 1.98 on the 26.08 branch at the
time of writing; Halyard's minimum is 1.90). `rust-toolchain.toml` is not used: there is no
rustup in the SDK.

## Running shells on the host

A terminal whose shell runs inside the sandbox is not useful: it would see the runtime's
programs, not yours. So, as other Flatpak terminals do, Halyard asks Flatpak to start the
shell on the host.

When Halyard finds `/.flatpak-info` (the file Flatpak puts at the root of every sandbox),
every pane's program becomes:

```
/bin/sh -c '<open the env file as fd 3, delete it, exec the rest>' halyard-env <env file> \
  flatpak-spawn --host --watch-bus --env-fd=3 --directory=<cwd> [--unset-env=NAME ...] <program>
```

`FLATPAK_ID` alone does not count: it leaks into host shells started from Flatpak apps (an
IDE's terminal, for example), and a native Halyard started from one must keep running its
panes itself.

- The program is your login shell, `$SHELL -l`. If the sandbox does not know `$SHELL`
  (or it is not an absolute path), the host is asked for its own:
  `/bin/sh -c 'exec "${SHELL:-/bin/sh}" -l'`. A configured program runs on the host the
  same way, with its arguments.
- `--directory` is the pane's working directory, or `$HOME`.
- `flatpak-spawn` does not forward the sandbox's environment: the host process starts from
  your session's environment plus exactly the variables Halyard sets for a pane (`TERM`,
  `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `HALYARD_SESSION_ID`, any configured
  extras, `SHELL`, and `LANG=C.UTF-8` when no locale is set, as for a native pane), each
  once. Variables a pane removes become `--unset-env`.
- **No variable value is ever on a command line.** Any local user can read a process's
  arguments in `/proc/<pid>/cmdline`, while its environment is owner-only. Halyard writes
  the variables to a mode 0600 file in `$XDG_RUNTIME_DIR` (private to the sandbox), and the
  small `/bin/sh` step opens it as descriptor 3, deletes it, and runs `flatpak-spawn
  --env-fd=3`. The `/bin/sh` step is needed because portable-pty closes every descriptor
  above 2 before it starts the program. If the program never starts, Halyard deletes the
  file itself.
- `--watch-bus` makes Flatpak's session helper send SIGINT to the host process group when
  Halyard's side of the bus goes away (a crash, for example).
- Every option uses the `--name=value` form and comes before the program: `flatpak-spawn`
  treats the first argument that does not start with `-` as the command, and a value given
  as a separate argument would be taken for it.

Flatpak's session helper starts the host process in a new session and makes the pane's PTY
its controlling terminal (read from `flatpak-session-helper.c`), so job control and the
kernel's hangup on a closed PTY work as they do in a native install.

The decision is a pure function, `halyard_term::flatpak::host_launch`, with unit tests in
`crates/halyard-term/src/flatpak.rs` and `session.rs` (`inside_flatpak_*`,
`outside_flatpak_the_program_runs_directly`), so it is tested without Flatpak. Among them:
no value appears in the arguments (`no_variable_value_appears_in_the_arguments`), the
forwarded set is exact (`the_forwarded_environment_is_exactly_the_session_set`),
`FLATPAK_ID` alone does not switch host mode on (`flatpak_id_alone_does_not_switch_on_host_mode`),
and the real `/bin/sh` step hands a stand-in for `flatpak-spawn` the file as descriptor 3
and deletes it (`the_wrapper_hands_flatpak_spawn_the_file_as_fd_3_and_deletes_it`).

### What this means for security

The manifest grants `--talk-name=org.freedesktop.Flatpak`, and with it Halyard can run any
program on the host as you. **The sandbox does not confine your shell, by design.** Flatpak
is used here to distribute Halyard and its libraries, not to confine it: a terminal exists
to run your programs with your rights, and these run exactly as they would in a terminal
installed any other way. Treat the Flatpak build as trusted as a native install, no more.

The other permissions:

| Permission | Why |
|------------|-----|
| `--device=dri` | GPU rendering |
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc` | A window on Wayland, or X11 when there is no Wayland session |
| `--filesystem=host` | The app sees the same paths as the shells it starts, so working directories and restored panes resolve. Narrower access would not protect anything, since the host shell already sees everything |
| `--talk-name=org.freedesktop.Flatpak` | Starting host shells, as above |

There is no network permission: Halyard makes no network calls. CI checks the installed
bundle's permissions against this list and fails if network access appears.

### Known limits of the host-shell design

- **Process inspection does not cross the sandbox.** The pane's own child is
  `flatpak-spawn`; the shell and its children live in the host's PID namespace, which the
  sandbox cannot see. What Halyard reads from the process table therefore describes
  `flatpak-spawn`, not the shell: the "a command is still running" confirmation on closing a
  pane does not trigger, a saved workspace records `$HOME` instead of the shell's directory
  (unless the shell reports its directory with OSC 7), and agent detection by foreground
  process reports no agent. Expected from reading the code, not yet observed.
- **Signals go through `flatpak-spawn`.** Closing a pane sends SIGHUP to `flatpak-spawn`,
  which forwards it to the host process, and closing the PTY hangs up the shell's
  controlling terminal. An interactive shell ignores the SIGINT that `--watch-bus` sends,
  so a shell left behind by a crash ends through the PTY hangup instead. All of this is
  read from Flatpak's source, not yet observed in a real session.
- **Config and state live in the app's directories.** Flatpak points `XDG_CONFIG_HOME` and
  `XDG_STATE_HOME` at `~/.var/app/io.github.pete_builds.Halyard/`, so the config file is
  `~/.var/app/io.github.pete_builds.Halyard/config/halyard/config.toml`. To share one
  config with a native install, point `HALYARD_CONFIG` at it:
  `flatpak override --user --env=HALYARD_CONFIG=$HOME/.config/halyard/config.toml io.github.pete_builds.Halyard`.
- **Window matching.** Halyard's window app ID is `halyard`, not the Flatpak ID; the desktop
  file sets `StartupWMClass=halyard` so desktops can match the window to the launcher. Not
  yet checked on a real desktop.
- **The session API is off in the Flatpak.** Panes run on the host through
  `flatpak-spawn --host`, and the sandbox cannot see the host's process tree, so no hook in
  a pane could pass the API's ancestry check; its socket would also sit in the sandbox's
  private runtime directory, out of reach of host tools. Halyard says so once in the
  banner and does not start it; `halyard-msg` in a pane falls back to escape sequences,
  which still drive the attention marks (see "Hooks and `halyard-msg`" below for how a
  host hook reaches it). Making it work would need the host's pids (through the Flatpak
  portal) and `--filesystem=xdg-run/halyard:create`.

## Hooks and `halyard-msg`

The bundle ships the hook client at `/app/bin/halyard-msg`, next to `halyard`. That path
exists only inside the sandbox. Your panes, and the agents and hooks you start in them, run
on the host, where `/app/bin` is not on `PATH` and does not exist, so a hook that calls
`halyard-msg` by name fails with "command not found" (exit 127). Halyard does not add
anything to a host pane's `PATH`: the only directory it could add is a sandbox path.

Install a small wrapper on the host instead, for example as `~/.local/bin/halyard-msg`
(any directory on your `PATH`), and make it executable with `chmod +x`:

```sh
#!/bin/sh
# halyard-msg for the Halyard Flatpak: runs the copy inside the app.
# Outside Halyard there is nothing to report, so skip starting a sandbox.
[ -n "${HALYARD_SESSION_ID-}" ] || exit 0
exec flatpak run --command=halyard-msg io.github.pete_builds.Halyard "$@"
```

The hook commands in `docs/session-api.md` then work unchanged. How it reports: `flatpak
run` passes your environment through (so `HALYARD_SESSION_ID` arrives), the session API is
off in the Flatpak, and `halyard-msg` falls back to writing the OSC 7717 escape sequence to
`/dev/tty`, the pane's terminal, which Halyard reads. Each call starts a fresh sandbox,
which takes longer than running a native binary; it has not been timed. Because of the
early `exit 0`, the wrapper prints nothing for `--help` or `--version` outside Halyard; run
`flatpak run --command=halyard-msg io.github.pete_builds.Halyard --help` for those.

## What is verified, and what is not

Verified by CI (`.github/workflows/flatpak.yml`) on every pull request:

- the generated source lists match `Cargo.lock` and the Ghostty pin entry by entry, and
  hold only the source types and fields the generators write (`check-sources.py`);
  `test_check_sources.py` applies ten hand edits (a swapped Zig URL and sha256, an extra
  `shell` source in either file, a `rustc-wrapper` added to the cargo config, changed
  checksums, URLs and commits) and requires each to be caught;
- the bundle builds offline on x86_64 (`ubuntu-24.04`) and aarch64 (`ubuntu-24.04-arm`) in
  the Flathub `freedesktop-26.08` build container;
- the desktop file passes `desktop-file-validate` (inside the build and on Ubuntu) and the
  metainfo passes `appstreamcli validate` (on Ubuntu; the SDK has no `appstreamcli`);
- the installed bundle has exactly the permissions above, nothing missing and nothing extra
  (so no network access) (`check-permissions.sh`);
- inside the Platform runtime (what users get): the binary exists, `ldd` finds every
  library it links, the libraries GPUI loads at run time are present, and `flatpak-spawn`
  is there (`smoke-test.sh`);
- `halyard-msg` is in the bundle and runs through `flatpak run --command=halyard-msg`, and
  that exact host hook path, run under a pseudo-terminal with `HALYARD_SESSION_ID` set,
  puts the OSC 7717 report on the terminal; the control, without `HALYARD_SESSION_ID`,
  writes nothing.

Not verified:

- **No window has been opened from the Flatpak.** CI has no display or GPU, and no one has
  run the bundle on a real Linux desktop yet. Rendering, input, clipboard and fonts inside
  the sandbox are untested.
- **No host shell has been started through `flatpak-spawn`.** The command line is unit
  tested; the real round trip (PTY, job control, signals, `--watch-bus` cleanup) is not.
- **The `halyard-msg` wrapper has not been used from a real pane.** CI runs its
  `flatpak run` line under a pseudo-terminal, not inside a Halyard window, so the attention
  mark it should raise has not been seen.
- The bundle is not on Flathub and has not been through `flatpak-builder-lint`.
- NVIDIA systems need the matching `org.freedesktop.Platform.GL.nvidia-*` extension, which
  Flatpak normally installs; not tested.

## Updating the sources

After any change to `Cargo.lock`:

```sh
packaging/flatpak/update-cargo-sources.sh
```

It downloads `flatpak-cargo-generator.py` from `flatpak/flatpak-builder-tools` (MIT) at a
pinned commit, checks its sha256, runs it in a throwaway virtualenv and rewrites
`cargo-sources.json`. Needs `python3` with `venv`, `curl` and network access. To move to a
newer generator, change `GENERATOR_COMMIT` and `GENERATOR_SHA256` in the script.

After bumping `libghostty-vt` (which can move the Ghostty pin):

```sh
python3 packaging/flatpak/update-ghostty-sources.py
```

It reads the `libghostty-vt-sys` version from `Cargo.lock`, downloads that crate from
crates.io (checked against the `Cargo.lock` checksum), reads `GHOSTTY_COMMIT` from its build
script, downloads Ghostty's `build.zig.zon.json` at that commit, downloads every Zig package
archive to record its sha256 and how it unpacks, and rewrites `ghostty-sources.json`. Needs
Python 3.11 or newer; `.tar.zst` archives need Python 3.14 or the `zstd` program. Compare the
result with Ghostty's own `flatpak/zig-packages.json` at the same commit if it still has one.

Then run `python3 packaging/flatpak/check-sources.py`, which CI also runs. The Nix flake has
its own pin (`flake.nix`'s `ghostty` input), checked by its own `ghostty-pin` check.

To move to a newer Freedesktop runtime, change `runtime-version` in the manifest and the
container image digest in `.github/workflows/flatpak.yml` (the
`ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-<version>` tag) together.

To move Zig, change both tarball URLs and sha256 values from
`https://ziglang.org/download/index.json`. Ghostty decides the Zig version, not Halyard.
