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

#[test]
fn dropping_a_watcher_stops_and_joins_its_thread() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "").unwrap();
    let baseline = thread_count();
    let start = Instant::now();
    for _ in 0..50 {
        let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
        drop(w);
    }
    // Drop joins the worker, so even a long poll interval must not slow it down.
    assert!(start.elapsed() < Duration::from_secs(10), "drops were slow: {:?}", start.elapsed());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut now = thread_count();
    while now > baseline + 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        now = thread_count();
    }
    assert!(now <= baseline + 2, "{now} threads after 50 create/drop cycles, baseline {baseline}");
}
