//! Finding the foreground process of a PTY and what it is running.
//!
//! * The foreground process group comes from `tcgetpgrp` on the PTY master. Its id
//!   is the pid of the group leader (the job the shell started).
//! * macOS: the executable path from `proc_pidpath`, argv from
//!   `sysctl(KERN_PROCARGS2)`.
//! * Linux: argv from `/proc/<pid>/cmdline`, the executable from `/proc/<pid>/exe`.
//!
//! Reading another user's process can fail; fields that cannot be read are left
//! empty rather than failing the whole call, and callers treat a process they
//! cannot read as "not an agent".

use std::io;
#[cfg(unix)]
use std::os::fd::RawFd;

use crate::detect::ProcessInfo;

/// Most arguments kept from a process's argv.
pub const MAX_ARGS: usize = 256;
/// Longest argument kept, in bytes before lossy decoding.
pub const MAX_ARG_LEN: usize = 4096;

/// The foreground process group id of the terminal open on `fd` (a PTY master or
/// slave).
#[cfg(unix)]
pub fn foreground_process_group(fd: RawFd) -> io::Result<i32> {
    // SAFETY: tcgetpgrp reads terminal state for a file descriptor and has no
    // memory safety requirements; an invalid fd yields -1 and errno.
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    if pgid < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pgid)
}

/// The foreground process group leader of the terminal on `fd`, with its
/// executable and argv.
#[cfg(unix)]
pub fn foreground_process(fd: RawFd) -> io::Result<ProcessInfo> {
    process_info(foreground_process_group(fd)?)
}

/// Reads a process's executable path and argv.
pub fn process_info(pid: i32) -> io::Result<ProcessInfo> {
    if pid <= 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid pid"));
    }
    imp::process_info(pid)
}

fn decode_args<'a>(args: impl Iterator<Item = &'a [u8]>) -> Vec<String> {
    args.take(MAX_ARGS)
        .map(|a| String::from_utf8_lossy(&a[..a.len().min(MAX_ARG_LEN)]).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;

    pub(super) fn process_info(pid: i32) -> io::Result<ProcessInfo> {
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))?;
        let argv = parse_cmdline(&cmdline);
        // `exe` is unreadable for other users' processes; argv still identifies them.
        let exe = fs::read_link(format!("/proc/{pid}/exe")).ok();
        Ok(ProcessInfo { pid, exe, argv })
    }

    /// Splits NUL separated arguments, ignoring the trailing NUL.
    pub(super) fn parse_cmdline(data: &[u8]) -> Vec<String> {
        let data = data.strip_suffix(&[0]).unwrap_or(data);
        if data.is_empty() {
            return Vec::new();
        }
        decode_args(data.split(|&b| b == 0))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    pub(super) fn process_info(pid: i32) -> io::Result<ProcessInfo> {
        let exe = pidpath(pid);
        let argv = match procargs(pid) {
            Ok(argv) => argv,
            // A process we may not inspect still has a readable path, usually.
            Err(_) if exe.is_some() => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(ProcessInfo { pid, exe, argv })
    }

    fn pidpath(pid: i32) -> Option<PathBuf> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is valid for writes of its full length, which is what
        // we pass as the size.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buf)))
    }

    fn sysctl(mib: &mut [libc::c_int], buf: &mut [u8]) -> io::Result<usize> {
        let mut size = buf.len();
        // SAFETY: mib and buf are valid for the lengths passed; size is in/out and
        // starts at the buffer's capacity. No new value is written (null, 0).
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(size)
    }

    fn procargs(pid: i32) -> io::Result<Vec<String>> {
        let mut argmax_buf = [0u8; std::mem::size_of::<libc::c_int>()];
        sysctl(&mut [libc::CTL_KERN, libc::KERN_ARGMAX], &mut argmax_buf)?;
        let argmax = libc::c_int::from_ne_bytes(argmax_buf);
        let len = usize::try_from(argmax)
            .ok()
            .filter(|n| (4..=16 * 1024 * 1024).contains(n))
            .ok_or_else(|| io::Error::other("unexpected KERN_ARGMAX"))?;
        let mut buf = vec![0u8; len];
        let n = sysctl(&mut [libc::CTL_KERN, libc::KERN_PROCARGS2, pid], &mut buf)?;
        buf.truncate(n);
        parse_procargs2(&buf).ok_or_else(|| io::Error::other("malformed KERN_PROCARGS2"))
    }

    /// Layout: `int argc`, the exec path, NUL padding, then `argc` NUL terminated
    /// arguments, then the environment (ignored).
    pub(super) fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
        let argc_bytes: [u8; 4] = buf.get(..4)?.try_into().ok()?;
        let argc = usize::try_from(i32::from_ne_bytes(argc_bytes)).ok()?;
        let mut rest = &buf[4..];
        // Skip the exec path and the NULs after it.
        let path_end = rest.iter().position(|&b| b == 0)?;
        rest = &rest[path_end..];
        let first_arg = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
        rest = &rest[first_arg..];
        let args: Vec<&[u8]> = rest.split(|&b| b == 0).take(argc.min(MAX_ARGS)).collect();
        Some(decode_args(args.into_iter()))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub(super) fn process_info(_pid: i32) -> io::Result<ProcessInfo> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process inspection is implemented for macOS and Linux only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pid() {
        assert_eq!(process_info(0).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(process_info(-5).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn tcgetpgrp_on_a_non_tty_fails() {
        let f = std::fs::File::open("/dev/null").unwrap();
        use std::os::fd::AsRawFd;
        assert!(foreground_process_group(f.as_raw_fd()).is_err());
        assert!(foreground_process_group(-1).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn reads_own_process() {
        let me = process_info(std::process::id() as i32).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(me.exe.unwrap().file_name(), exe.file_name(), "exe should be this test binary");
        let args: Vec<String> = std::env::args().collect();
        assert_eq!(me.argv, args);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn missing_process_errors() {
        // Pids near the 32-bit limit are not in use on test machines.
        assert!(process_info(i32::MAX - 7).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cmdline_parsing() {
        assert_eq!(imp::parse_cmdline(b"a\0b c\0\0d\0"), vec!["a", "b c", "", "d"]);
        assert_eq!(imp::parse_cmdline(b""), Vec::<String>::new());
        assert_eq!(imp::parse_cmdline(b"\0"), Vec::<String>::new());
        assert_eq!(imp::parse_cmdline(b"x\xff"), vec!["x\u{fffd}"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn procargs2_parsing() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/sleep\0\0\0\0sleep\x005\0HOME=/x\0");
        assert_eq!(imp::parse_procargs2(&buf).unwrap(), vec!["sleep", "5"]);
        assert!(imp::parse_procargs2(b"\x01\0").is_none());
        let mut neg = (-1i32).to_ne_bytes().to_vec();
        neg.extend_from_slice(b"/x\0a\0");
        assert!(imp::parse_procargs2(&neg).is_none());
        let mut no_nul = 1i32.to_ne_bytes().to_vec();
        no_nul.extend_from_slice(b"/x");
        assert!(imp::parse_procargs2(&no_nul).is_none());
    }
}
