//! Starting programs on the host when Halyard runs inside a Flatpak sandbox.
//!
//! A terminal whose shell runs inside the Flatpak sandbox is not much use: the
//! sandbox has the runtime's tools, not the user's, and no access to the host's
//! programs. So, like other Flatpak terminals, Halyard asks Flatpak to start the
//! program on the host with `flatpak-spawn --host`, which talks to
//! `org.freedesktop.Flatpak` on the session bus (the manifest grants
//! `--talk-name=org.freedesktop.Flatpak`).
//!
//! **Security meaning:** that permission lets the app run anything on the host
//! as the user. The sandbox does not confine the shell, by design: Halyard uses
//! Flatpak to distribute the app, not to confine it. See `docs/flatpak.md`.
//!
//! **Environment values never go on a command line.** Every local user can read
//! a process's arguments (`/proc/<pid>/cmdline`), while its environment is
//! readable by the owner only. So the variables for the host program are
//! written to an owner-only file ([`EnvFile`]) and handed to `flatpak-spawn`
//! with `--env-fd`. portable-pty closes every descriptor above 2 before it runs
//! the program, so a tiny `/bin/sh` step ([`ENV_FD_WRAPPER`]) opens the file as
//! descriptor 3, deletes it, and then runs `flatpak-spawn`.
//!
//! The decisions are pure functions of explicit inputs, so they are tested
//! without Flatpak. [`Session`](crate::Session) calls [`in_flatpak`] once per
//! spawn and, when it is true, runs [`host_launch`]'s command instead of the
//! program itself.

use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The program that asks Flatpak to run a command on the host. It ships in
/// every Freedesktop runtime (flatpak-xdg-utils).
pub const FLATPAK_SPAWN: &str = "flatpak-spawn";

/// The file Flatpak puts at the root of every sandbox.
pub const FLATPAK_INFO: &str = "/.flatpak-info";

/// The descriptor `flatpak-spawn` reads the environment block from.
pub const ENV_FD: u32 = 3;

/// The sandbox-side step between portable-pty and `flatpak-spawn`: open the
/// environment file (`$1`) as descriptor 3, delete it, then run the rest of
/// the arguments. Run as `/bin/sh -c ENV_FD_WRAPPER <name> <file> flatpak-spawn ...`.
pub const ENV_FD_WRAPPER: &str = r#"exec 3<"$1" && rm -f -- "$1" && shift && exec "$@""#;

/// True when this process runs inside a Flatpak sandbox, which Flatpak marks
/// with `/.flatpak-info`. See [`flatpak_info_present`].
#[must_use]
pub fn in_flatpak() -> bool {
    flatpak_info_present(Path::new(FLATPAK_INFO))
}

/// True when `info` is a regular file.
///
/// This is the only signal Halyard trusts. `FLATPAK_ID` is deliberately ignored:
/// it leaks into host shells started from a Flatpak app (an IDE's terminal, for
/// example), and a native Halyard launched from one must still run its panes
/// itself instead of failing every one of them through `flatpak-spawn`.
#[must_use]
pub fn flatpak_info_present(info: &Path) -> bool {
    info.is_file()
}

/// A program and its arguments, ready to hand to the PTY, plus the environment
/// block to write to an [`EnvFile`] first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The program to execute.
    pub program: OsString,
    /// Its arguments, not including `argv[0]`.
    pub args: Vec<OsString>,
    /// `KEY=VALUE` entries, each followed by a NUL byte (the `env -0` format
    /// `flatpak-spawn --env-fd` reads).
    pub env_block: Vec<u8>,
}

/// What the session wants to run, as seen from inside the sandbox.
#[derive(Clone, Copy, Debug)]
pub struct HostSpawn<'a> {
    /// The program to run on the host. `None` runs the user's login shell.
    pub program: Option<&'a Path>,
    /// Arguments for `program`. Ignored when `program` is `None`.
    pub args: &'a [String],
    /// Working directory on the host. `None` uses `home`.
    pub cwd: Option<&'a Path>,
    /// `$HOME`. Flatpak keeps the host's path for it.
    pub home: Option<&'a OsStr>,
    /// `$SHELL` as the sandbox inherited it from the desktop session.
    pub shell: Option<&'a OsStr>,
    /// Variables the program must see (Halyard's `TERM`, `HALYARD_SESSION_ID`,
    /// the session's extras), in order; a later entry wins.
    pub env: &'a [(OsString, OsString)],
    /// Variables to remove from the host environment the program starts with.
    pub unset: &'a [String],
    /// Where the [`EnvFile`] holding `env` will be.
    pub env_file: &'a Path,
}

/// The shell command run on the host when the sandbox does not know the
/// user's shell: the host's own `$SHELL` (set by the user's systemd or login
/// session), else `/bin/sh`, as a login shell.
pub const HOST_SHELL_FALLBACK: &str = r#"exec "${SHELL:-/bin/sh}" -l"#;

/// The command that runs `spec` on the host:
///
/// `/bin/sh -c ENV_FD_WRAPPER halyard-env <env_file> flatpak-spawn --host --watch-bus
/// --env-fd=3 [--directory=<cwd>] [--unset-env=NAME ...] <program> <args>`
///
/// - The program is `spec.program` with its arguments, or else the login
///   shell: `<$SHELL> -l` when `$SHELL` is an absolute path, otherwise
///   `/bin/sh -c` [`HOST_SHELL_FALLBACK`]. The sandbox cannot check that the
///   host's shell exists, so `$SHELL` is passed through as is.
/// - `--watch-bus` has Flatpak signal the host process group (SIGINT) when
///   Halyard's side of the bus goes away. A shell that ignores SIGINT still
///   ends when its PTY, its controlling terminal, is closed.
/// - Every option uses the `--name=value` form and precedes the program:
///   `flatpak-spawn` takes the first argument without a leading `-` as the
///   command.
/// - `flatpak-spawn` does not forward the sandbox's environment; the host
///   process starts from the user's session environment plus exactly the
///   variables in `env_block`: `spec.env`, once per name with its last value,
///   and `SHELL` naming the login shell when one was chosen. Names that are
///   empty or contain `=`, and anything containing a NUL byte, cannot be
///   expressed and are skipped. No value appears in the arguments.
/// - Each name in `spec.unset` that is not also set becomes `--unset-env`.
#[must_use]
pub fn host_launch(spec: &HostSpawn<'_>) -> Launch {
    let mut args: Vec<OsString> = vec![
        "-c".into(),
        ENV_FD_WRAPPER.into(),
        "halyard-env".into(),
        spec.env_file.as_os_str().to_owned(),
        FLATPAK_SPAWN.into(),
        "--host".into(),
        "--watch-bus".into(),
        format!("--env-fd={ENV_FD}").into(),
    ];
    if let Some(dir) = spec.cwd.map(Path::as_os_str).or(spec.home).filter(|d| !d.is_empty()) {
        args.push(prefixed("--directory=", dir));
    }
    let shell = spec.shell.filter(|s| Path::new(s).is_absolute());
    let env = merged_env(spec, shell);
    for name in spec.unset {
        let valid = !name.is_empty() && !name.contains(['=', '\0']);
        let set = env.iter().any(|(k, _)| k.as_bytes() == name.as_bytes());
        if valid && !set {
            args.push(format!("--unset-env={name}").into());
        }
    }
    let mut env_block = Vec::new();
    for (key, value) in env {
        env_block.extend_from_slice(key.as_bytes());
        env_block.push(b'=');
        env_block.extend_from_slice(value.as_bytes());
        env_block.push(0);
    }
    match (spec.program, shell) {
        (Some(program), _) => {
            args.push(program.as_os_str().to_owned());
            args.extend(spec.args.iter().map(OsString::from));
        }
        (None, Some(shell)) => {
            args.push(shell.to_owned());
            args.push("-l".into());
        }
        (None, None) => {
            args.extend(["/bin/sh", "-c", HOST_SHELL_FALLBACK].map(OsString::from));
        }
    }
    Launch { program: "/bin/sh".into(), args, env_block }
}

/// `spec.env` plus `SHELL`, one entry per valid name holding its last value,
/// in first-seen order.
fn merged_env<'a>(spec: &HostSpawn<'a>, shell: Option<&'a OsStr>) -> Vec<(&'a OsStr, &'a OsStr)> {
    let mut env: Vec<(&OsStr, &OsStr)> = Vec::with_capacity(spec.env.len() + 1);
    let shell_var =
        spec.program.is_none().then_some(shell).flatten().map(|s| (OsStr::new("SHELL"), s));
    let wanted = spec.env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())).chain(shell_var);
    for (key, value) in wanted {
        let (k, v) = (key.as_bytes(), value.as_bytes());
        if k.is_empty() || k.contains(&b'=') || k.contains(&0) || v.contains(&0) {
            continue;
        }
        match env.iter_mut().find(|(seen, _)| *seen == key) {
            Some(slot) => slot.1 = value,
            None => env.push((key, value)),
        }
    }
    env
}

fn prefixed(prefix: &str, value: &OsStr) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(value);
    s
}

/// The directory for [`EnvFile`]s: `$XDG_RUNTIME_DIR` (a per-user, mode 0700
/// directory; inside Flatpak, private to the sandbox) when it is absolute,
/// else the temporary directory.
#[must_use]
pub fn env_file_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

/// An owner-only (mode 0600) file holding a [`Launch::env_block`] until the
/// [`ENV_FD_WRAPPER`] step opens and deletes it.
///
/// Dropping it deletes the file, which is right when the program never
/// started. Once the program has started, call [`EnvFile::handed_over`]
/// instead: the wrapper deletes the file itself, and deleting it first would
/// lose the environment.
#[derive(Debug)]
pub struct EnvFile {
    path: PathBuf,
    armed: bool,
}

impl EnvFile {
    /// A fresh path in `dir` for a new file; nothing is created yet.
    #[must_use]
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(format!("halyard-env-{}", uuid::Uuid::new_v4().simple()))
    }

    /// Create `path` (which must not exist) with mode 0600 and write `block`.
    pub fn create(path: PathBuf, block: &[u8]) -> io::Result<Self> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        let env_file = Self { path, armed: true };
        file.write_all(block)?;
        file.sync_data()?;
        Ok(env_file)
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The program started and owns the file now; do not delete it on drop.
    pub fn handed_over(mut self) {
        self.armed = false;
    }
}

impl Drop for EnvFile {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("halyard-term: removing {}: {e}", self.path.display());
        }
    }
}

/// How old an env file must be before [`sweep_stale_env_files`] treats it as
/// abandoned. A live file exists for milliseconds (created, then opened and
/// deleted by the wrapper), so ten minutes leaves a wide margin for another
/// running Halyard and for clock differences.
pub const STALE_ENV_FILE_AGE: Duration = Duration::from_secs(10 * 60);

const ENV_FILE_PREFIX: &str = "halyard-env-";

/// True when `name` is exactly what [`EnvFile::path_in`] makes: the prefix and
/// 32 lowercase hex digits.
fn is_env_file_name(name: &OsStr) -> bool {
    name.as_bytes().strip_prefix(ENV_FILE_PREFIX.as_bytes()).is_some_and(|rest| {
        rest.len() == 32 && rest.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Remove env files that a crash or kill left in `dir`, and return how many.
///
/// An [`EnvFile`] holds the pane's environment (possibly secrets) from its
/// creation until the wrapper opens and deletes it. If Halyard dies in that
/// window the file stays. Halyard cannot unlink it earlier: portable-pty closes
/// every descriptor above 2 before the program starts, so the wrapper has to
/// open it by path. This sweep bounds the damage instead. It removes an entry
/// only when all of these hold:
///
/// - the name is `halyard-env-` plus 32 lowercase hex digits;
/// - it is a regular file by `symlink_metadata`, so a symlink (or anything
///   else) is never followed or touched;
/// - it is owned by the current effective user, so another user's files in a
///   shared directory such as `/tmp` are left alone;
/// - its modification time is at least `min_age` before `now`, so a file a
///   running Halyard is about to hand to a wrapper is left alone. A time in the
///   future counts as fresh.
///
/// Errors on single entries (already gone, not permitted) are skipped: a sweep
/// must never keep a terminal from starting.
pub fn sweep_stale_env_files(dir: &Path, min_age: Duration, now: SystemTime) -> usize {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    sweep_for_uid(dir, min_age, now, uid)
}

fn sweep_for_uid(dir: &Path, min_age: Duration, now: SystemTime, uid: u32) -> usize {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !is_env_file_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= min_age);
        if meta.file_type().is_file()
            && meta.uid() == uid
            && old
            && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn strs(launch: &Launch) -> Vec<String> {
        launch.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    fn block(pairs: &[&str]) -> Vec<u8> {
        pairs.iter().flat_map(|p| p.bytes().chain([0])).collect()
    }

    const FILE: &str = "/run/user/1000/halyard-env-x";

    fn spec<'a>(env: &'a [(OsString, OsString)]) -> HostSpawn<'a> {
        HostSpawn {
            program: None,
            args: &[],
            cwd: None,
            home: Some(OsStr::new("/home/ada")),
            shell: Some(OsStr::new("/usr/bin/zsh")),
            env,
            unset: &[],
            env_file: Path::new(FILE),
        }
    }

    /// Everything after the wrapper's own arguments: flatpak-spawn's argv.
    fn spawn_args(launch: &Launch) -> Vec<String> {
        strs(launch)[4..].to_vec()
    }

    #[test]
    fn only_the_info_file_marks_a_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let info = dir.path().join(".flatpak-info");
        assert!(!flatpak_info_present(&info));
        std::fs::create_dir(&info).unwrap();
        assert!(!flatpak_info_present(&info), "a directory is not the marker");
        std::fs::remove_dir(&info).unwrap();
        std::fs::write(&info, "[Application]\nname=x\n").unwrap();
        assert!(flatpak_info_present(&info));
    }

    /// Runs only as the child of `flatpak_id_alone_does_not_switch_on_host_mode`.
    #[test]
    fn probe_in_flatpak_matches_the_info_file() {
        if std::env::var_os("HALYARD_FLATPAK_PROBE").is_none() {
            return;
        }
        assert_eq!(std::env::var_os("FLATPAK_ID").as_deref(), Some(OsStr::new("org.example.Ide")));
        assert_eq!(in_flatpak(), Path::new(FLATPAK_INFO).is_file());
    }

    #[test]
    fn flatpak_id_alone_does_not_switch_on_host_mode() {
        // A native Halyard started from a Flatpak IDE's host terminal inherits
        // FLATPAK_ID. Run this test binary again with it set: in_flatpak()
        // must still follow /.flatpak-info only.
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "flatpak::tests::probe_in_flatpak_matches_the_info_file"])
            .env("HALYARD_FLATPAK_PROBE", "1")
            .env("FLATPAK_ID", "org.example.Ide")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("1 passed"), "the probe did not run: {stdout}");
    }

    #[test]
    fn login_shell_runs_on_the_host_with_cwd_and_env() {
        let env = env(&[("TERM", "xterm-256color"), ("HALYARD_SESSION_ID", "abc")]);
        let launch =
            host_launch(&HostSpawn { cwd: Some(Path::new("/home/ada/src")), ..spec(&env) });
        assert_eq!(launch.program, OsString::from("/bin/sh"));
        assert_eq!(
            strs(&launch),
            [
                "-c",
                ENV_FD_WRAPPER,
                "halyard-env",
                FILE,
                "flatpak-spawn",
                "--host",
                "--watch-bus",
                "--env-fd=3",
                "--directory=/home/ada/src",
                "/usr/bin/zsh",
                "-l",
            ]
        );
        assert_eq!(
            launch.env_block,
            block(&["TERM=xterm-256color", "HALYARD_SESSION_ID=abc", "SHELL=/usr/bin/zsh"])
        );
    }

    #[test]
    fn no_variable_value_appears_in_the_arguments() {
        let secret = "s3cret-session-token-value";
        let env = env(&[("HALYARD_SESSION_TOKEN", secret), ("TERM", "xterm-256color")]);
        let launch = host_launch(&spec(&env));
        for arg in strs(&launch) {
            assert!(!arg.contains(secret), "{arg}");
            assert!(!arg.contains("xterm-256color"), "{arg}");
            assert!(!arg.starts_with("--env="), "{arg}");
        }
        assert!(launch.env_block.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }

    #[test]
    fn the_forwarded_environment_is_exactly_the_session_set() {
        // Nothing from Halyard's own (sandbox) environment is added: not
        // PATH, not HOME, not anything else this test process has.
        let env = env(&[("A", "1"), ("B", "2")]);
        let launch =
            host_launch(&HostSpawn { program: Some(Path::new("/bin/true")), ..spec(&env) });
        assert_eq!(launch.env_block, block(&["A=1", "B=2"]));
        let launch = host_launch(&spec(&[]));
        assert_eq!(launch.env_block, block(&["SHELL=/usr/bin/zsh"]));
    }

    #[test]
    fn without_cwd_the_shell_starts_in_home() {
        let launch = host_launch(&spec(&[]));
        assert!(strs(&launch).contains(&"--directory=/home/ada".to_owned()), "{launch:?}");
    }

    #[test]
    fn without_cwd_or_home_flatpak_spawn_picks_the_directory() {
        let launch = host_launch(&HostSpawn { home: None, ..spec(&[]) });
        assert!(!strs(&launch).iter().any(|a| a.starts_with("--directory")), "{launch:?}");
        let launch = host_launch(&HostSpawn { home: Some(OsStr::new("")), ..spec(&[]) });
        assert!(!strs(&launch).iter().any(|a| a.starts_with("--directory")), "{launch:?}");
    }

    #[test]
    fn unknown_shell_asks_the_host_for_its_own() {
        for shell in [None, Some(OsStr::new("")), Some(OsStr::new("zsh"))] {
            let launch = host_launch(&HostSpawn { shell, ..spec(&[]) });
            let args = strs(&launch);
            assert_eq!(args[args.len() - 3..], ["/bin/sh", "-c", HOST_SHELL_FALLBACK], "{shell:?}");
            assert!(launch.env_block.is_empty(), "{shell:?}");
        }
    }

    #[test]
    fn explicit_program_runs_with_its_arguments_and_no_shell() {
        let args = vec!["-c".to_owned(), "echo hi; exit 3".to_owned()];
        let launch = host_launch(&HostSpawn {
            program: Some(Path::new("/bin/bash")),
            args: &args,
            ..spec(&[])
        });
        let got = spawn_args(&launch);
        assert_eq!(got[got.len() - 3..], ["/bin/bash", "-c", "echo hi; exit 3"]);
        assert!(!got.contains(&"-l".to_owned()), "{got:?}");
        assert!(launch.env_block.is_empty());
    }

    #[test]
    fn host_options_come_before_the_program() {
        // flatpak-spawn takes the first argument without a leading `-` as the
        // command, so every option of ours comes first and carries its value
        // after `=`, never as a separate argument.
        let unset = vec!["OLD".to_owned()];
        let launch = host_launch(&HostSpawn {
            cwd: Some(Path::new("/w")),
            unset: &unset,
            ..spec(&env(&[("A", "1")]))
        });
        let args = spawn_args(&launch);
        assert_eq!(args[0], "flatpak-spawn");
        let program = args.iter().position(|a| a == "/usr/bin/zsh").unwrap();
        assert_eq!(program, 6, "{args:?}");
        for opt in &args[1..program] {
            assert!(opt.starts_with("--"), "{args:?}");
            assert!(
                opt == "--host" || opt == "--watch-bus" || opt.contains('='),
                "{opt} has no inline value: {args:?}"
            );
        }
    }

    #[test]
    fn removed_variables_are_unset_on_the_host_unless_set_again() {
        let unset = vec!["OLD".to_owned(), "A".to_owned(), String::new(), "X=Y".to_owned()];
        let launch = host_launch(&HostSpawn { unset: &unset, ..spec(&env(&[("A", "1")])) });
        let unsets: Vec<_> =
            strs(&launch).into_iter().filter(|a| a.starts_with("--unset-env")).collect();
        assert_eq!(unsets, ["--unset-env=OLD"]);
    }

    #[test]
    fn invalid_variables_are_skipped_and_values_kept_verbatim() {
        let mut env = env(&[("", "x"), ("A=B", "y"), ("OK", "a=b c\nd")]);
        env.push((OsString::from("NUL"), OsString::from("a\0b")));
        let launch = host_launch(&spec(&env));
        assert_eq!(launch.env_block, block(&["OK=a=b c\nd", "SHELL=/usr/bin/zsh"]));
    }

    #[test]
    fn a_later_variable_wins_and_is_passed_once() {
        // The session's extras come after Halyard's own and override them, as
        // they do outside Flatpak. SHELL always names the shell that runs.
        let env = env(&[("TERM", "xterm-256color"), ("A", "1"), ("TERM", "dumb"), ("SHELL", "/x")]);
        let launch = host_launch(&spec(&env));
        assert_eq!(launch.env_block, block(&["TERM=dumb", "A=1", "SHELL=/usr/bin/zsh"]));
    }

    #[test]
    fn env_file_is_owner_only_and_removed_unless_handed_over() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = EnvFile::path_in(dir.path());
        let file = EnvFile::create(path.clone(), b"A=1\0").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"A=1\0");
        drop(file);
        assert!(!path.exists(), "an env file nobody used must not stay behind");

        let path = EnvFile::path_in(dir.path());
        EnvFile::create(path.clone(), b"").unwrap().handed_over();
        assert!(path.exists(), "the wrapper deletes a handed-over file, not the drop");
        assert!(EnvFile::create(path.clone(), b"").is_err(), "an existing file is never reused");
    }

    #[test]
    fn the_wrapper_hands_flatpak_spawn_the_file_as_fd_3_and_deletes_it() {
        // Run the real wrapper with a stand-in for flatpak-spawn that prints
        // what it reads from descriptor 3, the way `--env-fd=3` does.
        let dir = tempfile::tempdir().unwrap();
        let path = EnvFile::path_in(dir.path());
        EnvFile::create(path.clone(), b"A=1\0B=two words\0").unwrap().handed_over();
        let out = Command::new("/bin/sh")
            .args(["-c", ENV_FD_WRAPPER, "halyard-env"])
            .arg(&path)
            .args(["/bin/sh", "-c", "cat <&3; test ! -e \"$0\" && echo gone"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout, b"A=1\0B=two words\0gone\n");
        assert!(!path.exists());
    }

    fn make_env_file(dir: &Path, age: Duration) -> PathBuf {
        let path = EnvFile::path_in(dir);
        EnvFile::create(path.clone(), b"SECRET=1\0").unwrap().handed_over();
        let when = SystemTime::now() - age;
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(when).unwrap();
        path
    }

    #[test]
    fn sweep_removes_an_old_own_file_and_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let old = make_env_file(dir.path(), Duration::from_secs(3600));
        let fresh = make_env_file(dir.path(), Duration::from_secs(5));
        let other_name = dir.path().join("halyard-env-notahexname");
        std::fs::write(&other_name, "x").unwrap();
        let unrelated = dir.path().join("other-file");
        std::fs::write(&unrelated, "x").unwrap();
        // A symlink with a matching name, pointing at an old file we must not delete.
        let target = dir.path().join("target-file");
        std::fs::write(&target, "x").unwrap();
        let link = EnvFile::path_in(dir.path());
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // A matching name that is a directory.
        let sub = EnvFile::path_in(dir.path());
        std::fs::create_dir(&sub).unwrap();

        let n = sweep_stale_env_files(dir.path(), STALE_ENV_FILE_AGE, SystemTime::now());
        assert_eq!(n, 1);
        assert!(!old.exists(), "the old own file must go");
        assert!(fresh.exists(), "a fresh file may belong to a running Halyard");
        assert!(other_name.exists() && unrelated.exists());
        assert!(link.symlink_metadata().is_ok() && target.exists(), "symlinks are never touched");
        assert!(sub.is_dir());
    }

    #[test]
    fn sweep_leaves_files_of_another_user_and_future_dated_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = make_env_file(dir.path(), Duration::from_secs(3600));
        let me = unsafe { libc::geteuid() };
        let n =
            sweep_for_uid(dir.path(), STALE_ENV_FILE_AGE, SystemTime::now(), me.wrapping_add(1));
        assert_eq!(n, 0);
        assert!(old.exists(), "a file owned by someone else is not ours to remove");

        let future = EnvFile::path_in(dir.path());
        EnvFile::create(future.clone(), b"").unwrap().handed_over();
        let when = SystemTime::now() + Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&future).unwrap().set_modified(when).unwrap();
        sweep_stale_env_files(dir.path(), STALE_ENV_FILE_AGE, SystemTime::now());
        assert!(future.exists(), "a future mtime counts as fresh");
        assert!(!old.exists());
    }

    #[test]
    fn sweep_of_a_missing_directory_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("nope");
        assert_eq!(sweep_stale_env_files(&gone, STALE_ENV_FILE_AGE, SystemTime::now()), 0);
    }
}
