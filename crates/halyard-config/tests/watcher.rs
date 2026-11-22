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

#[test]
fn a_config_toml_elsewhere_under_a_watched_ancestor_does_not_trigger_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    // The real file's directory does not exist, so the watch sits on `dir` itself.
    let file = dir.path().join("halyard").join("config.toml");
    let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
    let other = dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("config.toml"), "[font]\nsize = 33\n").unwrap();
    fs::write(dir.path().join("config.toml"), "[font]\nsize = 34\n").unwrap();
    assert!(
        w.recv_timeout(Duration::from_millis(1200)).is_none(),
        "a config.toml that is not the watched path must not reload"
    );
    // Control: the real path still works.
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "[font]\nsize = 35\n").unwrap();
    let _ = wait_for(&w, "size 35", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 35.0).abs() < 0.01)
    });
}

#[test]
fn notices_a_config_created_several_missing_directories_deep() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a").join("b").join("halyard").join("config.toml");
    let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "[font]\nsize = 36\n").unwrap();
    let _ = wait_for(&w, "size 36", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 36.0).abs() < 0.01)
    });
}

#[test]
fn a_constant_event_stream_still_reloads_within_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // The debounce is longer than the gap between writes, so without a cap nothing
    // would fire until the writes stop.
    let w = ConfigWatcher::new(&file, Duration::from_millis(400)).unwrap();
    let start = Instant::now();
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
        let mut i = 0;
        while start.elapsed() < Duration::from_millis(2500) {
            i += 1;
            fs::write(&writer_file, format!("[font]\nsize = {}\n", 10 + i % 50)).unwrap();
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    let first = w.recv_timeout(Duration::from_millis(1500));
    let at = start.elapsed();
    let writing_still = !writer.is_finished();
    writer.join().unwrap();
    assert!(first.is_some(), "no reload while events kept coming");
    assert!(writing_still, "the stream ended before the first reload: not a test of the cap");
    // Cap is 500 ms after the first event; allow scheduling and file system latency.
    assert!(at < Duration::from_millis(1100), "first reload took {at:?}");
}

/// Discard events until none arrives for `quiet`.
fn drain(w: &ConfigWatcher, quiet: Duration) {
    while w.recv_timeout(quiet).is_some() {}
}

#[test]
fn a_themes_directory_created_after_startup_is_watched() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();
    let themes = dir.path().join("themes");
    fs::create_dir(&themes).unwrap();
    // Creating the directory is itself an event; let it and the new watch settle.
    let _ = wait_for(&w, "themes dir created", |_| true);
    drain(&w, Duration::from_millis(600));
    fs::write(themes.join("mine.toml"), "name = \"x\"\n").unwrap();
    let _ = wait_for(&w, "a theme file written into the new themes dir", |_| true);
}

#[test]
fn a_relative_config_path_that_does_not_exist_yet_is_noticed() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let w = ConfigWatcher::new(std::path::Path::new("sub/config.toml"), Duration::from_millis(50));
    std::env::set_current_dir(&before).unwrap();
    let w = w.unwrap();
    let file = dir.path().join("sub").join("config.toml");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "[font]\nsize = 37\n").unwrap();
    let ev = wait_for(&w, "size 37 for a relative path", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 37.0).abs() < 0.01)
    });
    assert!(ev.is_ok());
}

#[test]
fn a_deleted_and_recreated_config_directory_is_noticed_again() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("halyard");
    let file = cfg_dir.join("config.toml");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(&file, "[font]\nsize = 12\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let w = ConfigWatcher::new(&file, Duration::from_millis(50)).unwrap();

    fs::remove_dir_all(&cfg_dir).unwrap();
    let _ = wait_for(&w, "defaults after the directory is deleted", |e| {
        e.as_ref().is_ok_and(|l| l.config == Config::default())
    });
    drain(&w, Duration::from_millis(600));
    fs::create_dir(&cfg_dir).unwrap();
    fs::write(&file, "[font]\nsize = 41\n").unwrap();
    let _ = wait_for(&w, "size 41 after the directory is recreated", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 41.0).abs() < 0.01)
    });
    // And once more, to show the moved watch works the same way a second time.
    fs::remove_dir_all(&cfg_dir).unwrap();
    drain(&w, Duration::from_millis(600));
    fs::create_dir(&cfg_dir).unwrap();
    fs::write(&file, "[font]\nsize = 42\n").unwrap();
    let _ = wait_for(&w, "size 42 after a second recreate", |e| {
        e.as_ref().is_ok_and(|l| (l.config.font.size - 42.0).abs() < 0.01)
    });
}
