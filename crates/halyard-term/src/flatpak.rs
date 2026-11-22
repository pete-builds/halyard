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
//! Everything here is a pure decision from explicit inputs, so it is tested
//! without Flatpak. [`Session`](crate::Session) calls [`in_flatpak`] once per
//! spawn and, when it is true, runs [`host_launch`]'s command instead of the
//! program itself.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The program that asks Flatpak to run a command on the host. It ships in
/// every Freedesktop runtime (flatpak-xdg-utils).
pub const FLATPAK_SPAWN: &str = "flatpak-spawn";

/// The file Flatpak puts at the root of every sandbox.
pub const FLATPAK_INFO: &str = "/.flatpak-info";

/// True when this process runs inside a Flatpak sandbox: `/.flatpak-info`
/// exists or `FLATPAK_ID` is set. See [`sandbox_detected`].
#[must_use]
pub fn in_flatpak() -> bool {
    sandbox_detected(Path::new(FLATPAK_INFO).exists(), std::env::var_os("FLATPAK_ID").as_deref())
}

/// Decide whether we run inside Flatpak from the two signals Flatpak gives:
/// the `/.flatpak-info` file, and the `FLATPAK_ID` variable (ignored when
/// empty).
#[must_use]
pub fn sandbox_detected(info_file_exists: bool, flatpak_id: Option<&OsStr>) -> bool {
    info_file_exists || flatpak_id.is_some_and(|id| !id.is_empty())
}

/// A program and its arguments, ready to hand to the PTY.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The program to execute.
    pub program: OsString,
    /// Its arguments, not including `argv[0]`.
    pub args: Vec<OsString>,
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
    /// Variables the program must see (Halyard's `TERM`, `HALYARD_SESSION_ID`
    /// and the session's extras), in order; a later entry wins on the host.
    pub env: &'a [(OsString, OsString)],
}

/// The shell command run on the host when the sandbox does not know the
/// user's shell: the host's own `$SHELL` (set by the user's systemd or login
/// session), else `/bin/sh`, as a login shell.
pub const HOST_SHELL_FALLBACK: &str = r#"exec "${SHELL:-/bin/sh}" -l"#;

/// The command that runs `spec` on the host:
///
/// `flatpak-spawn --host --watch-bus [--directory=<cwd>] [--env=K=V ...] <program> <args>`
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
///   process starts from the user's session environment. Every variable in
///   `spec.env` is passed with `--env`, once per name with its last value,
///   plus `SHELL` naming the login shell when one was chosen. Names that are
///   empty or contain `=` cannot be expressed and are skipped.
/// - Variables the session removes from the inherited environment need no
///   handling: the host environment never had the sandbox's copies.
#[must_use]
pub fn host_launch(spec: &HostSpawn<'_>) -> Launch {
    let mut args: Vec<OsString> = vec!["--host".into(), "--watch-bus".into()];
    if let Some(dir) = spec.cwd.map(Path::as_os_str).or(spec.home).filter(|d| !d.is_empty()) {
        args.push(prefixed("--directory=", dir));
    }
    let shell = spec.shell.filter(|s| Path::new(s).is_absolute());
    let mut env: Vec<(&OsStr, &OsStr)> = Vec::with_capacity(spec.env.len() + 1);
    let shell_var =
        spec.program.is_none().then_some(shell).flatten().map(|s| (OsStr::new("SHELL"), s));
    let wanted = spec.env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())).chain(shell_var);
    for (key, value) in wanted {
        if key.is_empty() || key.as_bytes().contains(&b'=') {
            continue;
        }
        // One --env per name, holding the last value, so the result does not
        // depend on how flatpak-spawn treats repeated names.
        match env.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => env.push((key, value)),
        }
    }
    for (key, value) in env {
        let mut arg = prefixed("--env=", key);
        arg.push("=");
        arg.push(value);
        args.push(arg);
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
    Launch { program: FLATPAK_SPAWN.into(), args }
}

fn prefixed(prefix: &str, value: &OsStr) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(value);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(launch: &Launch) -> Vec<String> {
        launch.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    fn spec<'a>(env: &'a [(OsString, OsString)]) -> HostSpawn<'a> {
        HostSpawn {
            program: None,
            args: &[],
            cwd: None,
            home: Some(OsStr::new("/home/ada")),
            shell: Some(OsStr::new("/usr/bin/zsh")),
            env,
        }
    }

    #[test]
    fn detects_flatpak_from_either_signal() {
        assert!(sandbox_detected(true, None));
        assert!(sandbox_detected(false, Some(OsStr::new("io.github.pete_builds.Halyard"))));
        assert!(sandbox_detected(true, Some(OsStr::new("x"))));
    }

    #[test]
    fn no_signal_or_an_empty_id_is_not_flatpak() {
        assert!(!sandbox_detected(false, None));
        assert!(!sandbox_detected(false, Some(OsStr::new(""))));
    }

    #[test]
    fn login_shell_runs_on_the_host_with_cwd_and_env() {
        let env = env(&[("TERM", "xterm-256color"), ("HALYARD_SESSION_ID", "abc")]);
        let launch =
            host_launch(&HostSpawn { cwd: Some(Path::new("/home/ada/src")), ..spec(&env) });
        assert_eq!(launch.program, OsString::from("flatpak-spawn"));
        assert_eq!(
            strs(&launch),
            [
                "--host",
                "--watch-bus",
                "--directory=/home/ada/src",
                "--env=TERM=xterm-256color",
                "--env=HALYARD_SESSION_ID=abc",
                "--env=SHELL=/usr/bin/zsh",
                "/usr/bin/zsh",
                "-l",
            ]
        );
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
            assert!(!args.iter().any(|a| a.starts_with("--env=SHELL=")), "{args:?}");
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
        let got = strs(&launch);
        assert_eq!(got[got.len() - 3..], ["/bin/bash", "-c", "echo hi; exit 3"]);
        assert!(!got.iter().any(|a| a.starts_with("--env=SHELL=")), "{got:?}");
        assert!(!got.contains(&"-l".to_owned()), "{got:?}");
    }

    #[test]
    fn host_options_come_before_the_program() {
        // flatpak-spawn takes the first argument without a leading `-` as the
        // command, so every option of ours comes first and carries its value
        // after `=`, never as a separate argument.
        let env = env(&[("A", "1")]);
        let launch = host_launch(&HostSpawn { cwd: Some(Path::new("/w")), ..spec(&env) });
        let args = strs(&launch);
        let program = args.iter().position(|a| a == "/usr/bin/zsh").unwrap();
        assert_eq!(program, 5, "{args:?}");
        for opt in &args[..program] {
            assert!(opt.starts_with("--"), "{args:?}");
            assert!(
                opt == "--host" || opt == "--watch-bus" || opt.contains('='),
                "{opt} has no inline value: {args:?}"
            );
        }
    }

    #[test]
    fn invalid_variable_names_are_skipped_and_values_kept_verbatim() {
        let env = env(&[("", "x"), ("A=B", "y"), ("OK", "a=b c\nd")]);
        let args = strs(&host_launch(&spec(&env)));
        let envs: Vec<_> = args.iter().filter(|a| a.starts_with("--env=")).collect();
        assert_eq!(envs, ["--env=OK=a=b c\nd", "--env=SHELL=/usr/bin/zsh"]);
    }

    #[test]
    fn a_later_variable_wins_and_is_passed_once() {
        // The session's extras come after Halyard's own and override them, as
        // they do outside Flatpak. SHELL always names the shell that runs.
        let env = env(&[("TERM", "xterm-256color"), ("A", "1"), ("TERM", "dumb"), ("SHELL", "/x")]);
        let args = strs(&host_launch(&spec(&env)));
        let envs: Vec<_> = args.iter().filter(|a| a.starts_with("--env=")).collect();
        assert_eq!(envs, ["--env=TERM=dumb", "--env=A=1", "--env=SHELL=/usr/bin/zsh"]);
    }
}
