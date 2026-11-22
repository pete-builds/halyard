//! Peer credentials and process ancestry, from the kernel.
//!
//! Linux uses `SO_PEERCRED` and `/proc/<pid>/stat`. macOS uses `getpeereid`,
//! `LOCAL_PEERPID` and `proc_pidinfo(PROC_PIDTBSDINFO)`. Other platforms are refused.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::types::PeerInfo;

/// The most parent links [`is_descendant`] follows before giving up.
pub const MAX_ANCESTRY_DEPTH: usize = 1024;

/// The effective user id of this process.
pub fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Reads the connecting process's uid and pid from the kernel.
pub fn peer_info(stream: &UnixStream) -> io::Result<PeerInfo> {
    let (uid, pid) = peer_uid_pid(stream)?;
    Ok(PeerInfo { pid, uid, exe: exe_path(pid) })
}

#[cfg(target_os = "linux")]
fn peer_uid_pid(stream: &UnixStream) -> io::Result<(u32, u32)> {
    let fd = stream.as_raw_fd();
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` is a valid, writable ucred and `len` holds its exact size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != std::mem::size_of::<libc::ucred>() || cred.pid <= 0 {
        return Err(io::Error::other("SO_PEERCRED returned no peer process"));
    }
    Ok((cred.uid, cred.pid as u32))
}

#[cfg(target_os = "macos")]
fn peer_uid_pid(stream: &UnixStream) -> io::Result<(u32, u32)> {
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both out-pointers are valid for writes.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: `pid` is a valid, writable pid_t and `len` holds its exact size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if pid <= 0 {
        return Err(io::Error::other("LOCAL_PEERPID returned no peer process"));
    }
    Ok((uid, pid as u32))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid_pid(_stream: &UnixStream) -> io::Result<(u32, u32)> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "peer credentials are not implemented here"))
}

/// The parent pid of `pid`, or `None` if the process is gone or unreadable.
#[cfg(target_os = "linux")]
pub fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: "pid (comm) state ppid ...". comm may contain spaces and ')', so split
    // after the LAST ')'.
    let after = &stat[stat.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// The parent pid of `pid`, or `None` if the process is gone or unreadable.
#[cfg(target_os = "macos")]
pub fn parent_pid(pid: u32) -> Option<u32> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    Some(info.pbi_ppid)
}

/// The parent pid of `pid`. Unsupported platforms never report a parent.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

/// True when `pid` is `ancestor` or a descendant of it.
///
/// Walks the parent chain from `pid` upward. Stops at pid 1 (init or launchd), at a
/// process that cannot be read, at a loop, or after [`MAX_ANCESTRY_DEPTH`] steps, and
/// answers `false` in each case. `ancestor` values of 0 and 1 never match, because every
/// process descends from them.
pub fn is_descendant(pid: u32, ancestor: u32) -> bool {
    if ancestor <= 1 {
        return false;
    }
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if current == ancestor {
            return true;
        }
        if current <= 1 {
            return false;
        }
        match parent_pid(current) {
            Some(parent) if parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

/// The executable path of `pid`, when the platform reports it.
#[cfg(target_os = "linux")]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// The executable path of `pid`, when the platform reports it.
#[cfg(target_os = "macos")]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for its full length, which is passed as the size.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

/// The executable path of `pid`. Unsupported platforms never report one.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn exe_path(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_descends_from_its_parent() {
        let me = std::process::id();
        let parent = parent_pid(me).expect("own parent pid is readable");
        assert!(is_descendant(me, me));
        assert!(is_descendant(me, parent));
    }

    #[test]
    fn parent_does_not_descend_from_child() {
        let me = std::process::id();
        let parent = parent_pid(me).expect("own parent pid is readable");
        assert!(!is_descendant(parent, me));
    }

    #[test]
    fn init_and_zero_never_count_as_ancestors() {
        let me = std::process::id();
        assert!(!is_descendant(me, 1));
        assert!(!is_descendant(me, 0));
    }

    #[test]
    fn vanished_process_is_not_a_descendant() {
        let mut child = std::process::Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("wait");
        // The pid is reaped; walking from it finds nothing (unless the pid was reused
        // within microseconds by a descendant of this test, which cannot happen here).
        assert!(!is_descendant(pid, std::process::id()));
    }

    #[test]
    fn socketpair_reports_own_uid_and_pid() {
        let (a, _b) = UnixStream::pair().expect("socketpair");
        let info = peer_info(&a).expect("peer info");
        assert_eq!(info.uid, current_euid());
        assert_eq!(info.pid, std::process::id());
        assert!(info.exe.is_some(), "own exe path is readable");
    }
}
