use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::{Duration, Instant};

use halyard_config::{Config, ConfigEvent, ConfigWatcher};

const POLL: Duration = Duration::from_millis(200);
const DEB: Duration = Duration::from_millis(50);

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

fn size_is(n: f32) -> impl Fn(&ConfigEvent) -> bool {
    move |e| e.as_ref().is_ok_and(|l| (l.config.font.size - n).abs() < 0.01)
}

fn is_defaults(e: &ConfigEvent) -> bool {
    e.as_ref().is_ok_and(|l| l.config == Config::default())
}

fn cfg(size: u32) -> String {
    format!("[font]\nsize = {size}\n")
}

/// Run `f` once with the normal watcher (notifications plus poll) and once poll-only, which
/// proves the fingerprint alone finds the change.
fn both_modes(f: impl Fn(&dyn Fn(&Path) -> ConfigWatcher)) {
    f(&|p| ConfigWatcher::with_poll_interval(p, DEB, POLL).unwrap());
    f(&|p| ConfigWatcher::poll_only(p, DEB, POLL).unwrap());
}

fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

#[test]
fn reloads_on_change_reports_errors_and_handles_delete() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "[font]\nsize = 12\n").unwrap();
    settle();
    let w = ConfigWatcher::new(&file, Duration::from_millis(100)).unwrap();

    fs::write(&file, cfg(18)).unwrap();
    let ev = wait_for(&w, "size 18", size_is(18.0));
    assert!(ev.unwrap().warnings.is_empty());

    // A broken edit is reported as an error, not swallowed.
    fs::write(&file, "[font]\nsize = \"huge\"\n").unwrap();
    let e = wait_for(&w, "error for font.size", |e| e.is_err()).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("font.size"));
    assert_eq!(e.line, Some(2));

    // Atomic replace (temp file then rename), the way most editors save.
    let tmp = dir.path().join("config.toml.tmp");
    fs::write(&tmp, cfg(20)).unwrap();
    fs::rename(&tmp, &file).unwrap();
    let _ = wait_for(&w, "size 20 after rename", size_is(20.0));

    // Deleting the file falls back to the defaults, and the event says so.
    fs::remove_file(&file).unwrap();
    let ev = wait_for(&w, "defaults after delete", is_defaults);
    let loaded = ev.unwrap();
    assert!(loaded.warnings.iter().any(|w| w.message.contains("does not exist")), "{loaded:?}");
}

#[test]
fn a_burst_of_writes_is_debounced_into_few_events() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    settle();
    let w = ConfigWatcher::new(&file, Duration::from_millis(400)).unwrap();
    for i in 0..20 {
        fs::write(&file, cfg(10 + i)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut count = 0;
    let mut last = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(ev) = w.recv_timeout(Duration::from_millis(500)) {
            count += 1;
            last = Some(ev);
        } else if count > 0 {
            break;
        }
    }
    assert!((1..=4).contains(&count), "expected a debounced handful of events, got {count}");
    let last = last.unwrap().unwrap();
    assert!((last.config.font.size - 29.0).abs() < 0.01, "last write must win");
}

#[test]
fn a_constant_event_stream_still_reloads_within_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "").unwrap();
    settle();
    // The debounce is longer than the gap between writes, so without a cap nothing would
    // fire until the writes stop.
    let w = ConfigWatcher::with_poll_interval(&file, Duration::from_millis(400), POLL).unwrap();
    let start = Instant::now();
    let writer_file = file.clone();
    let writer = std::thread::spawn(move || {
        let mut i = 0;
        while start.elapsed() < Duration::from_millis(2500) {
            i += 1;
            fs::write(&writer_file, cfg(10 + i % 50)).unwrap();
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    let first = w.recv_timeout(Duration::from_millis(1500));
    let at = start.elapsed();
    let writing_still = !writer.is_finished();
    writer.join().unwrap();
    assert!(first.is_some(), "no reload while changes kept coming");
    assert!(writing_still, "the stream ended before the first reload: not a test of the cap");
    // The cap is 500 ms after the change is first seen; the poll can add up to one interval.
    assert!(at < Duration::from_millis(1200), "first reload took {at:?}");
}

#[test]
fn nothing_changed_means_no_event_over_three_seconds_of_polling() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, cfg(12)).unwrap();
    fs::create_dir(dir.path().join("themes")).unwrap();
    fs::write(dir.path().join("themes").join("a.toml"), "name = \"x\"\n").unwrap();
    settle();
    let w = ConfigWatcher::new(&file, DEB).unwrap();
    // Unrelated files in the same directory, and in themes/ with another extension.
    fs::write(dir.path().join("other.txt"), "x").unwrap();
    fs::write(dir.path().join("themes").join("notes.txt"), "x").unwrap();
    let ev = w.recv_timeout(Duration::from_millis(3200));
    assert!(ev.is_none(), "spurious event: {ev:?}");
}

#[test]
fn a_config_toml_elsewhere_is_not_ours() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("halyard").join("config.toml");
    let w = ConfigWatcher::with_poll_interval(&file, DEB, POLL).unwrap();
    let other = dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("config.toml"), cfg(33)).unwrap();
    fs::write(dir.path().join("config.toml"), cfg(34)).unwrap();
    assert!(w.recv_timeout(Duration::from_millis(1500)).is_none());
    // Control: the real path still works.
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, cfg(35)).unwrap();
    let _ = wait_for(&w, "size 35", size_is(35.0));
}

#[test]
fn notices_a_config_created_several_missing_directories_deep() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("b").join("halyard").join("config.toml");
        let w = make(&file);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, cfg(36)).unwrap();
        let _ = wait_for(&w, "size 36", size_is(36.0));
    });
}

#[test]
fn symlink_swapped_by_rename_over_is_noticed() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("gen-1.toml"), dir.path().join("gen-2.toml"));
        fs::write(&one, cfg(12)).unwrap();
        fs::write(&two, cfg(22)).unwrap();
        let link = dir.path().join("config.toml");
        symlink(&one, &link).unwrap();
        settle();
        let w = make(&link);
        // The way `ln -sfn` and home-manager switch replace a link: new link, rename over.
        let tmp = dir.path().join("config.toml.new");
        symlink(&two, &tmp).unwrap();
        fs::rename(&tmp, &link).unwrap();
        let _ = wait_for(&w, "size 22 after link swap", size_is(22.0));
        // And again, so the new target is the one being followed from now on.
        symlink(&one, &tmp).unwrap();
        fs::rename(&tmp, &link).unwrap();
        let _ = wait_for(&w, "size 12 after the second swap", size_is(12.0));
    });
}

#[test]
fn symlink_swapped_by_unlink_and_link_is_noticed() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("gen-1.toml"), dir.path().join("gen-2.toml"));
        fs::write(&one, cfg(12)).unwrap();
        fs::write(&two, cfg(23)).unwrap();
        let link = dir.path().join("config.toml");
        symlink(&one, &link).unwrap();
        settle();
        let w = make(&link);
        fs::remove_file(&link).unwrap();
        symlink(&two, &link).unwrap();
        let _ = wait_for(&w, "size 23 after unlink and link", size_is(23.0));
    });
}

#[test]
fn an_edit_made_through_a_symlink_into_another_directory_is_noticed() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let home = dir.path().join("home");
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&home).unwrap();
        let real = store.join("config.toml");
        fs::write(&real, cfg(12)).unwrap();
        let link = home.join("config.toml");
        symlink(&real, &link).unwrap();
        settle();
        let w = make(&link);
        // The target's directory is not the link's directory.
        fs::write(&real, cfg(24)).unwrap();
        let _ = wait_for(&w, "size 24 after editing the target", size_is(24.0));
    });
}

#[test]
fn a_directory_moved_into_place_by_rename_is_noticed() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        fs::create_dir_all(staging.join("themes")).unwrap();
        fs::write(staging.join("config.toml"), cfg(25)).unwrap();
        let file = dir.path().join("halyard").join("config.toml");
        let w = make(&file);
        fs::rename(&staging, dir.path().join("halyard")).unwrap();
        let _ = wait_for(&w, "size 25 after the directory is renamed into place", size_is(25.0));
    });
}

#[test]
fn a_directory_moved_away_and_back_is_noticed_both_ways() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let cfg_dir = dir.path().join("halyard");
        fs::create_dir_all(&cfg_dir).unwrap();
        let file = cfg_dir.join("config.toml");
        fs::write(&file, cfg(26)).unwrap();
        settle();
        let w = make(&file);
        let trash = dir.path().join("trash");
        fs::rename(&cfg_dir, &trash).unwrap();
        let ev = wait_for(&w, "defaults after the directory moves away", is_defaults);
        assert!(ev.unwrap().warnings.iter().any(|w| w.message.contains("does not exist")));
        fs::rename(&trash, &cfg_dir).unwrap();
        let _ = wait_for(&w, "size 26 after it moves back", size_is(26.0));
    });
}

#[test]
fn a_deleted_and_recreated_config_directory_is_noticed_again() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let cfg_dir = dir.path().join("halyard");
        let file = cfg_dir.join("config.toml");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::write(&file, cfg(12)).unwrap();
        settle();
        let w = make(&file);
        for n in [41, 42] {
            fs::remove_dir_all(&cfg_dir).unwrap();
            let _ = wait_for(&w, "defaults after the directory is deleted", is_defaults);
            fs::create_dir(&cfg_dir).unwrap();
            fs::write(&file, cfg(n)).unwrap();
            let _ = wait_for(&w, "the recreated file", size_is(n as f32));
        }
    });
}

#[test]
fn a_themes_directory_created_late_and_its_files_are_noticed() {
    both_modes(|make| {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "").unwrap();
        settle();
        let w = make(&file);
        let themes = dir.path().join("themes");
        fs::create_dir(&themes).unwrap();
        fs::write(themes.join("mine.toml"), "name = \"x\"\n").unwrap();
        let _ = wait_for(&w, "the new themes directory and its file", |_| true);
        // A later edit to a theme file is a change too.
        std::thread::sleep(Duration::from_millis(700));
        while w.try_recv().is_some() {}
        fs::write(themes.join("mine.toml"), "name = \"y\"\n").unwrap();
        let _ = wait_for(&w, "an edit inside themes/", |_| true);
    });
}

#[test]
fn a_relative_config_path_that_does_not_exist_yet_is_noticed() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let w = ConfigWatcher::poll_only(Path::new("sub/config.toml"), DEB, POLL);
    std::env::set_current_dir(&before).unwrap();
    let w = w.unwrap();
    let file = dir.path().join("sub").join("config.toml");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, cfg(37)).unwrap();
    let _ = wait_for(&w, "size 37 for a relative path", size_is(37.0));
}
