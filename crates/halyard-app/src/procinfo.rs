//! What a pane's shell is doing, read from the operating system.
//!
//! - Whether a command runs in the foreground: the terminal's foreground
//!   process group differs from the shell's own group. Used to confirm before
//!   closing a pane that is running something.
//! - The shell's working directory, used for saved workspaces when the shell
//!   does not report it with OSC 7.
//!
//! Both read only the shell's own process entry: macOS through
//! `proc_pidinfo`, Linux through `/proc/<pid>/stat` and `/proc/<pid>/cwd`.

use std::path::PathBuf;

/// The shell's process group and its terminal's foreground process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Groups {
    /// The shell's process group.
    pub pgid: u32,
    /// The foreground process group of the shell's controlling terminal.
    pub tpgid: u32,
}

impl Groups {
    /// True when something other than the shell holds the terminal.
    #[must_use]
    pub fn running_command(self) -> bool {
        self.tpgid != 0 && self.tpgid != self.pgid
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// Parse `/proc/<pid>/stat`: fields 5 (pgrp) and 8 (tpgid), counted after
/// the command name, which may itself hold spaces and parentheses.
#[must_use]
pub fn parse_proc_stat(stat: &str) -> Option<Groups> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After ")": state(3) ppid(4) pgrp(5) session(6) tty_nr(7) tpgid(8).
    let pgid = fields.get(2)?.parse().ok()?;
    let tpgid: i64 = fields.get(5)?.parse().ok()?;
    Some(Groups { pgid, tpgid: u32::try_from(tpgid).unwrap_or(0) })
}

/// Process groups of `pid`, or `None` when it cannot be read.
#[cfg(target_os = "linux")]
#[must_use]
pub fn groups(pid: u32) -> Option<Groups> {
    parse_proc_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Process groups of `pid`, or `None` when it cannot be read.
#[cfg(target_os = "macos")]
#[must_use]
pub fn groups(pid: u32) -> Option<Groups> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is plain data; zeroed is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(Groups { pgid: info.pbi_pgid, tpgid: info.e_tpgid })
}

/// Process groups are not read on other platforms.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[must_use]
pub fn groups(_pid: u32) -> Option<Groups> {
    None
}

/// The working directory of `pid`.
#[cfg(target_os = "linux")]
#[must_use]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().filter(|p| p.is_absolute())
}

/// The working directory of `pid`.
#[cfg(target_os = "macos")]
#[must_use]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: plain data; zeroed is valid.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: the buffer is a proc_vnodepathinfo of exactly `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
    };
    if n != size {
        return None;
    }
    // SAFETY: vip_path is a NUL-terminated C string inside a fixed buffer
    // (MAXPATHLEN bytes, laid out as 32 rows of 32).
    let raw: &[libc::c_char] =
        unsafe { std::slice::from_raw_parts(info.pvi_cdir.vip_path.as_ptr().cast(), 32 * 32) };
    let bytes: Vec<u8> = raw.iter().map(|c| c.cast_unsigned()).collect();
    let c = CStr::from_bytes_until_nul(&bytes).ok()?;
    let p = PathBuf::from(std::ffi::OsStr::from_bytes(c.to_bytes()));
    p.is_absolute().then_some(p)
}

/// Working directories are not read on other platforms.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[must_use]
pub fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// Whether closing a pane needs a confirmation first.
#[must_use]
pub fn needs_close_confirmation(confirm_setting: bool, groups: Option<Groups>) -> bool {
    confirm_setting && groups.is_some_and(Groups::running_command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_parses_with_awkward_names() {
        let s = "1234 (zsh) S 1 1234 1234 34816 5678 4194304 0 0";
        assert_eq!(parse_proc_stat(s), Some(Groups { pgid: 1234, tpgid: 5678 }));
        let s = "99 (my (odd) prog) R 1 99 99 0 -1 0";
        assert_eq!(parse_proc_stat(s), Some(Groups { pgid: 99, tpgid: 0 }));
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[test]
    fn confirmation_rules() {
        let idle = Some(Groups { pgid: 10, tpgid: 10 });
        let busy = Some(Groups { pgid: 10, tpgid: 22 });
        assert!(!needs_close_confirmation(true, idle));
        assert!(needs_close_confirmation(true, busy));
        assert!(!needs_close_confirmation(false, busy));
        assert!(!needs_close_confirmation(true, None));
    }

    #[test]
    fn own_working_directory_is_read() {
        let me = std::process::id();
        let got = cwd(me).expect("cwd of this process");
        assert_eq!(
            std::fs::canonicalize(got).unwrap(),
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
        );
    }

    /// End to end through a real PTY: an idle shell is not "running a
    /// command"; the same shell running `sleep` is.
    #[test]
    fn foreground_job_is_detected_through_a_pty() {
        use halyard_term::{Session, SessionConfig};
        use std::time::{Duration, Instant};
        let session = Session::spawn(SessionConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-i".into()],
            ..SessionConfig::default()
        })
        .unwrap();
        let pid = session.pid().expect("child pid");
        let wait_for = |want: bool| {
            let start = Instant::now();
            loop {
                if groups(pid).map(Groups::running_command) == Some(want) {
                    return true;
                }
                if start.elapsed() > Duration::from_secs(10) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        assert!(wait_for(false), "idle shell: {:?}", groups(pid));
        session.write("sleep 30\n");
        assert!(wait_for(true), "sleep in the foreground: {:?}", groups(pid));
        session.write([3u8]);
        assert!(wait_for(false), "back at the prompt: {:?}", groups(pid));
    }
}
