//! Dropping a session releases its threads and descriptors even when a
//! background job keeps the PTY open. This lives in its own test binary
//! because it counts process-wide file descriptors and threads.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use halyard_term::{Session, SessionConfig};

fn open_fds() -> usize {
    let dir = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    std::fs::read_dir(dir).map(Iterator::count).unwrap_or(0)
}

/// Threads whose name starts with `prefix`, where the platform can list
/// them (Linux; names are cut to 15 bytes there).
fn threads_named(prefix: &str) -> Option<usize> {
    let tasks = std::fs::read_dir("/proc/self/task").ok()?;
    Some(
        tasks
            .filter_map(Result::ok)
            .filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok())
            .filter(|name| name.starts_with(prefix))
            .count(),
    )
}

fn shell() -> Session {
    let env = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    Session::spawn(SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        env: vec![env("PS1", "hy$ "), env("HISTFILE", "/dev/null")],
        env_remove: vec!["ENV".into(), "BASH_ENV".into()],
        ..SessionConfig::default()
    })
    .expect("spawn /bin/sh")
}

fn wait_for_text(s: &Session, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !s.latest_frame().text().contains(text) {
        assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
        let _woken = s.events().recv_timeout(Duration::from_millis(100));
    }
}

#[test]
fn drop_releases_threads_and_fds_despite_a_background_job() {
    // Warm up once so lazily created process-wide descriptors are counted
    // in the baseline.
    drop(shell());
    std::thread::sleep(Duration::from_millis(300));
    let fds_before = open_fds();

    // The shell (session leader) ignores SIGHUP, and a background job that
    // also ignores it keeps the PTY open. Closing the window must still
    // stop the shell and release the reader thread and every descriptor.
    let s = Session::spawn(SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        args: vec![
            "-c".into(),
            "trap '' HUP; (exec sleep 30) & echo job:$!; while :; do sleep 1; done".into(),
        ],
        ..SessionConfig::default()
    })
    .expect("spawn /bin/sh");
    wait_for_text(&s, "job:");
    let text = s.latest_frame().text();
    let job: libc::pid_t = text
        .lines()
        .find_map(|l| l.strip_prefix("job:"))
        .and_then(|p| p.trim().parse().ok())
        .expect("job pid");
    // Let the job install its signal disposition.
    std::thread::sleep(Duration::from_millis(200));
    assert!(open_fds() > fds_before, "the session holds no descriptors?");
    drop(s);
    let dropped = Instant::now();

    // The reader stops as soon as the session does (its wake pipe closes),
    // well before the shell is force-killed 2 s later.
    while threads_named("halyard-pty-rea").is_some_and(|n| n > 0) {
        assert!(
            dropped.elapsed() < Duration::from_millis(1500),
            "reader thread outlived the session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let deadline = dropped + Duration::from_secs(10);
    loop {
        let fds = open_fds();
        let threads = threads_named("halyard-");
        if fds <= fds_before && threads.is_none_or(|t| t == 0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "after drop: {fds} fds (baseline {fds_before}), halyard threads {threads:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The job must still be alive, holding the PTY open, or this test did
    // not exercise the case it is about.
    // SAFETY: signal 0 only checks that the process exists.
    assert_eq!(unsafe { libc::kill(job, 0) }, 0, "the background job {job} died");
    // The job ignores hangup by design; clean it up.
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(job, libc::SIGKILL) };
}
