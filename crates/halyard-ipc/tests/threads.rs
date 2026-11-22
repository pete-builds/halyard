//! Thread hygiene. Its own test binary, so no other test's threads are counted.

mod common;

use std::time::{Duration, Instant};

use common::start_with;
use halyard_ipc::Limits;

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").expect("read status");
    let line = status.lines().find(|l| l.starts_with("Threads:")).expect("Threads line");
    line["Threads:".len()..].trim().parse().expect("number")
}

#[cfg(target_os = "macos")]
fn thread_count() -> usize {
    // SAFETY: proc_taskinfo is plain old data; all-zero is valid.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    assert_eq!(n, size, "proc_pidinfo");
    info.pti_threadnum as usize
}

#[test]
fn early_closed_connections_release_their_threads_promptly() {
    // A long handshake deadline: without the fix every watchdog lives 30 s.
    let limits = Limits { handshake_timeout: Duration::from_secs(30), ..Limits::default() };
    let h = start_with(limits);
    let baseline = thread_count();
    let conns: Vec<_> = (0..50).map(|_| h.connect()).collect();
    // Let the server accept them all (each has a connection and a watchdog thread).
    let started = Instant::now();
    while thread_count() < baseline + 50 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(thread_count() >= baseline + 50, "connections were not accepted");
    drop(conns);
    let started = Instant::now();
    while thread_count() > baseline && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        thread_count() <= baseline,
        "{} threads still alive 3 s after closing (baseline {baseline})",
        thread_count()
    );
}
