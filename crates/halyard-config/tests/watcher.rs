use std::fs;
use std::time::{Duration, Instant};

use halyard_config::{Config, ConfigEvent, ConfigWatcher};

/// Wait until an event satisfying `pred` arrives, ignoring others (file systems may emit extras).
fn wait_for(w: &ConfigWatcher, what: &str, pred: impl Fn(&ConfigEvent) -> bool) -> ConfigEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(ev) = w.recv_timeout(Duration::from_millis(200))
            && pred(&ev)
        {
            return ev;
        }
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn reloads_on_change_reports_errors_and_handles_delete() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "[font]\nsize = 12\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let w = ConfigWatcher::new(&file, Duration::from_millis(100)).unwrap();

    fs::write(&file, "[font]\nsize = 18\n").unwrap();
    let ev = wait_for(&w, "size 18", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 18.0).abs() < 0.01)
    });
    assert!(ev.unwrap().warnings.is_empty());

    // A broken edit is reported as an error, not swallowed.
    fs::write(&file, "[font]\nsize = \"huge\"\n").unwrap();
    let ev = wait_for(&w, "error for font.size", |e| e.is_err());
    let e = ev.unwrap_err();
    assert_eq!(e.key.as_deref(), Some("font.size"));
    assert_eq!(e.line, Some(2));

    // Atomic replace (temp file then rename), the way most editors save.
    let tmp = dir.path().join("config.toml.tmp");
    fs::write(&tmp, "[font]\nsize = 20\n").unwrap();
    fs::rename(&tmp, &file).unwrap();
    let _ = wait_for(&w, "size 20 after rename", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 20.0).abs() < 0.01)
    });

    // Deleting the file falls back to the defaults.
    fs::remove_file(&file).unwrap();
    let ev = wait_for(&w, "defaults after delete", |e| {
        e.as_ref().is_ok_and(|l| l.config == Config::default())
    });
    assert!(ev.is_ok());
}

#[test]
fn a_burst_of_writes_is_debounced_into_few_events() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let w = ConfigWatcher::new(&file, Duration::from_millis(400)).unwrap();
    for n in 5..25 {
        fs::write(&file, format!("[font]\nsize = {n}\n")).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    // The last value must win, and 20 rapid writes must not produce 20 reloads.
    let mut count = 0;
    let mut last = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(ev) = w.recv_timeout(Duration::from_millis(500)) {
            count += 1;
            last = Some(ev);
        }
    }
    assert!((1..=4).contains(&count), "expected a debounced handful of events, got {count}");
    let last = last.unwrap().unwrap();
    assert!(
        (last.config.font.size - 24.0).abs() < 0.01,
        "last write must win: {}",
        last.config.font.size
    );
}

#[test]
fn notices_a_config_created_in_a_directory_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("halyard").join("config.toml");
    let w = ConfigWatcher::new(&file, Duration::from_millis(100)).unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "[font]\nsize = 21\n").unwrap();
    let _ = wait_for(&w, "size 21", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 21.0).abs() < 0.01)
    });
}

#[test]
fn unrelated_files_in_the_directory_do_not_trigger_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
    fs::write(dir.path().join("other.txt"), "x").unwrap();
    assert!(w.recv_timeout(Duration::from_millis(1000)).is_none());
}
