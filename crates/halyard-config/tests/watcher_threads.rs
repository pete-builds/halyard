//! Lives in its own test binary so no other test's threads are counted.

use std::time::{Duration, Instant};

use halyard_config::ConfigWatcher;

/// Number of OS threads in this process.
fn thread_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").unwrap();
        s.lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-M", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count().saturating_sub(1)
    }
}

/// A per-watcher leak adds one thread per cycle, so [`CYCLES`] cycles would add that many.
/// Anything within [`SLACK`] is the system's own threads, not ours.
const CYCLES: usize = 60;
const SLACK: usize = 10;

#[test]
fn dropping_a_watcher_stops_and_joins_its_thread() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "").unwrap();
    // On macOS the first watcher makes the system start a few process-lifetime threads
    // (FSEvents and dispatch workers) that are never torn down and are not ours. Create and
    // drop one watcher first, so the baseline already includes them. CI saw 5 threads
    // against a baseline of 2 without this; the count is flat across 1, 10, 50 and 500
    // cycles, which a leak would not be.
    drop(ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap());
    let baseline = thread_count();
    let start = Instant::now();
    for _ in 0..CYCLES {
        let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
        drop(w);
    }
    // Drop joins the worker, so even a long poll interval must not slow it down.
    assert!(start.elapsed() < Duration::from_secs(10), "drops were slow: {:?}", start.elapsed());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut now = thread_count();
    while now > baseline + SLACK && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        now = thread_count();
    }
    assert!(
        now <= baseline + SLACK,
        "{now} threads after {CYCLES} create/drop cycles, baseline {baseline}"
    );
}
