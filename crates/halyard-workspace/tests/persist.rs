//! Serialization round trip, corrupt-file tolerance, file mode, OSC 7.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use halyard_workspace::persist::{
    self, MAX_DEPTH, MAX_PANES_PER_TAB, MAX_PANES_TOTAL, SavedNode, SavedState, SavedTab,
    SavedWindow, VERSION, osc7_local_path, resolve_cwd, state_dir_with,
};
use halyard_workspace::{Axis, PaneId, Workspace};

fn sample() -> (Workspace, Vec<PaneId>) {
    let (mut ws, a) = Workspace::new();
    let b = ws.split(Axis::Horizontal).unwrap();
    let c = ws.split(Axis::Vertical).unwrap();
    let p = ws.active().root.path_to(c).unwrap();
    ws.drag_divider(
        &p[..1],
        200.0,
        halyard_workspace::Rect::new(0.0, 0.0, 800.0, 600.0),
        4.0,
        halyard_workspace::MinSize { w: 50.0, h: 30.0 },
    );
    ws.focus_pane(b);
    ws.rename_tab(0, Some("build"));
    let d = ws.new_tab();
    ws.select_tab(0);
    (ws, vec![a, b, c, d])
}

fn cwd_for(ids: &[PaneId]) -> impl Fn(PaneId) -> Option<String> + '_ {
    move |p| {
        let i = ids.iter().position(|x| *x == p)?;
        Some(match i {
            0 => "/tmp".to_owned(),
            1 => "/usr/share".to_owned(),
            2 => "relative/not/saved".to_owned(),
            _ => "/var/log".to_owned(),
        })
    }
}

#[test]
fn serialization_round_trips() {
    let (ws, ids) = sample();
    let cwd = cwd_for(&ids);
    let state = SavedState { version: VERSION, windows: vec![ws.to_saved(&cwd)] };
    let json = serde_json::to_string(&state).unwrap();
    let back = persist::parse(&json);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.windows.len(), 1);
    let r = &back.windows[0];
    // Same shape, ratios, names, focus and active tab.
    let w = &r.workspace;
    assert_eq!(w.tabs().len(), 2);
    assert_eq!(w.active_index(), 0);
    assert_eq!(w.tabs()[0].name.as_deref(), Some("build"));
    assert_eq!(w.tabs()[0].root.ratios(), ws.tabs()[0].root.ratios());
    let pos = |w: &Workspace| {
        let t = &w.tabs()[0];
        t.root.panes().iter().position(|p| *p == t.focused)
    };
    assert_eq!(pos(w), pos(&ws));
    // Each pane's directory, in tree order; the relative one was dropped.
    let dirs: Vec<Option<PathBuf>> = r.cwds.iter().map(|(_, d)| d.clone()).collect();
    assert_eq!(
        dirs,
        vec![
            Some(PathBuf::from("/tmp")),
            Some(PathBuf::from("/usr/share")),
            None,
            Some(PathBuf::from("/var/log")),
        ]
    );
    // Saving the restored workspace gives the same file.
    let ids2: Vec<PaneId> = r.cwds.iter().map(|(p, _)| *p).collect();
    let cwd2 = |p: PaneId| {
        r.cwds
            .iter()
            .find(|(q, _)| *q == p)
            .and_then(|(_, d)| d.clone())
            .map(|d| d.to_string_lossy().into_owned())
    };
    let again = SavedState { version: VERSION, windows: vec![w.to_saved(&cwd2)] };
    let mut expected = state.clone();
    if let SavedNode::Split { second, .. } = &mut expected.windows[0].tabs[0].layout
        && let SavedNode::Split { second: c, .. } = second.as_mut()
    {
        **c = SavedNode::Pane { cwd: None };
    }
    assert_eq!(again, expected);
    assert_eq!(ids2.len(), 4);
    w.check().unwrap();
}

#[test]
fn the_file_holds_layout_only() {
    let (ws, ids) = sample();
    let cwd = cwd_for(&ids);
    let state = SavedState { version: VERSION, windows: vec![ws.to_saved(&cwd)] };
    let v: serde_json::Value = serde_json::to_value(&state).unwrap();
    let mut keys = std::collections::BTreeSet::new();
    fn walk(v: &serde_json::Value, keys: &mut std::collections::BTreeSet<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    keys.insert(k.clone());
                    walk(x, keys);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, keys)),
            _ => {}
        }
    }
    walk(&v, &mut keys);
    let allowed = [
        "version", "windows", "tabs", "active", "name", "layout", "focused", "zoomed", "pane",
        "cwd", "split", "axis", "ratio", "first", "second",
    ];
    for k in &keys {
        assert!(allowed.contains(&k.as_str()), "unexpected key {k}");
    }
}

#[test]
fn corrupt_files_give_a_default_layout_and_a_warning() {
    for bad in ["", "{", "not json", "{\"version\":1,\"windows\":[{\"tabs\":7}]}", "[1,2,3]"] {
        let r = persist::parse(bad);
        assert!(r.windows.is_empty(), "{bad}");
        assert_eq!(r.warnings.len(), 1, "{bad}: {:?}", r.warnings);
        assert!(r.warnings[0].contains("starting with a new window"), "{:?}", r.warnings);
        let w = r.windows_or_default();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].workspace.tabs().len(), 1);
        assert_eq!(w[0].cwds.len(), 1);
    }
    // The warning never quotes the file.
    let r = persist::parse("{\"version\": 1, \"windows\": [ SECRET ]}");
    assert!(!r.warnings[0].contains("SECRET"));
    // Another version is refused politely.
    let r = persist::parse("{\"version\": 99, \"windows\": []}");
    assert!(r.warnings[0].contains("version 99"));
}

#[test]
fn hostile_values_are_clamped_or_dropped() {
    let big = |n: usize| {
        let mut node = SavedNode::Pane { cwd: None };
        for _ in 1..n {
            node = SavedNode::Split {
                axis: Axis::Vertical,
                ratio: 0.5,
                first: Box::new(SavedNode::Pane { cwd: None }),
                second: Box::new(node),
            };
        }
        node
    };
    let json = serde_json::json!({
        "version": 1,
        "windows": [{
            "active": 99,
            "tabs": [
                {"layout": {"split": {"axis": "horizontal", "ratio": 7.0,
                    "first": {"pane": {"cwd": "/tmp\u{1b}]0;x"}},
                    "second": {"pane": {"cwd": "/tmp"}}}},
                 "focused": 50, "name": "a\u{0}b"},
                {"layout": serde_json::to_value(big(MAX_PANES_PER_TAB + 1)).unwrap()}
            ]
        }]
    });
    let r = persist::parse(&json.to_string());
    let w = &r.windows[0].workspace;
    assert_eq!(w.tabs().len(), 1, "the oversized tab was dropped");
    assert_eq!(w.active_index(), 0, "active clamped");
    assert_eq!(w.tabs()[0].root.ratios(), vec![0.95]);
    assert_eq!(w.tabs()[0].name.as_deref(), Some("ab"));
    assert_eq!(r.windows[0].cwds[0].1, None, "control characters rejected");
    assert_eq!(r.windows[0].cwds[1].1.as_deref(), Some(Path::new("/tmp")));
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    w.check().unwrap();
}

#[test]
fn save_is_atomic_and_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("workspace.json");
    let (ws, ids) = sample();
    let cwd = cwd_for(&ids);
    let state = SavedState { version: VERSION, windows: vec![ws.to_saved(&cwd)] };
    persist::save(&path, &state).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dmode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);
        // A loosened file is tightened again on the next save.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        persist::save(&path, &state).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let loaded = persist::load(&path);
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.windows.len(), 1);
    // No temporary files left behind.
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
    assert_eq!(names.len(), 1);
    // Missing file: nothing to restore, no warning.
    let none = persist::load(&dir.path().join("nope.json"));
    assert!(none.windows.is_empty() && none.warnings.is_empty());
    // A directory in its place is refused, not read.
    let d = dir.path().join("isdir.json");
    std::fs::create_dir(&d).unwrap();
    assert_eq!(persist::load(&d).warnings.len(), 1);
}

#[test]
fn only_the_default_state_dir_counts_as_halyards_own() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    };
    let mac = Path::new("/Users/p/Library/Application Support/Halyard");
    assert!(persist::is_default_state_dir(mac, env(&[("HOME", "/Users/p")]), true));
    // An override pointing at home is not Halyard's own folder.
    assert!(!persist::is_default_state_dir(
        Path::new("/Users/p"),
        env(&[("HOME", "/Users/p"), ("HALYARD_STATE_DIR", "/Users/p")]),
        true
    ));
    let linux = Path::new("/home/p/.local/state/halyard");
    assert!(persist::is_default_state_dir(linux, env(&[("HOME", "/home/p")]), false));
    assert!(!persist::is_default_state_dir(
        Path::new("/srv/shared"),
        env(&[("HOME", "/home/p")]),
        false
    ));
}

#[test]
fn state_dir_per_platform() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    };
    assert_eq!(
        state_dir_with(env(&[("HOME", "/Users/p")]), true),
        Some(PathBuf::from("/Users/p/Library/Application Support/Halyard"))
    );
    assert_eq!(
        state_dir_with(env(&[("HOME", "/home/p"), ("XDG_STATE_HOME", "/s")]), false),
        Some(PathBuf::from("/s/halyard"))
    );
    assert_eq!(
        state_dir_with(env(&[("HOME", "/home/p"), ("XDG_STATE_HOME", "rel")]), false),
        Some(PathBuf::from("/home/p/.local/state/halyard"))
    );
    assert_eq!(
        state_dir_with(env(&[("HOME", "/home/p"), ("HALYARD_STATE_DIR", "/x")]), false),
        Some(PathBuf::from("/x"))
    );
    assert_eq!(state_dir_with(env(&[]), false), None);
}

#[test]
fn osc7_is_used_only_when_local() {
    let h = Some("mybox.local");
    assert_eq!(osc7_local_path("file:///Users/p/src", h).as_deref(), Some("/Users/p/src"));
    assert_eq!(osc7_local_path("file://localhost/tmp", h).as_deref(), Some("/tmp"));
    assert_eq!(osc7_local_path("file://mybox.local/tmp", h).as_deref(), Some("/tmp"));
    assert_eq!(osc7_local_path("file://mybox/tmp", h), None, "first label only is not enough");
    assert_eq!(osc7_local_path("file://MYBOX.LOCAL/tmp", h).as_deref(), Some("/tmp"));
    assert_eq!(
        osc7_local_path("file://mybox.local/tmp", Some("mybox")),
        None,
        "a longer name is another host"
    );
    assert_eq!(osc7_local_path("file://other/tmp", h), None, "remote host");
    assert_eq!(osc7_local_path("file:///a%20b/c", h).as_deref(), Some("/a b/c"));
    assert_eq!(osc7_local_path("file:///a%1bb", h), None, "decoded control char");
    assert_eq!(osc7_local_path("file:///a%zz", h), None, "bad escape");
    assert_eq!(
        osc7_local_path("kitty-shell-cwd://mybox.local/x%20y", h).as_deref(),
        Some("/x%20y")
    );
    assert_eq!(osc7_local_path("/plain/path", h).as_deref(), Some("/plain/path"));
    assert_eq!(osc7_local_path("relative", h), None);
    assert_eq!(osc7_local_path("http://mybox/tmp", h), None);
    // Verbatim: never normalized.
    assert_eq!(osc7_local_path("file:///a/../b/", h).as_deref(), Some("/a/../b/"));
}

/// A chain of `n` panes: each split nests the previous one, depth `n - 1`.
fn chain(n: usize) -> SavedNode {
    let mut node = SavedNode::Pane { cwd: None };
    for _ in 1..n {
        node = SavedNode::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(SavedNode::Pane { cwd: None }),
            second: Box::new(node),
        };
    }
    node
}

fn tab(layout: SavedNode) -> SavedTab {
    SavedTab { name: None, layout, focused: 0, zoomed: false }
}

#[test]
fn restore_caps_the_total_number_of_panes() {
    // Balanced 32-pane tabs (depth 5), 9 windows: 288 panes in the file.
    fn balanced(n: usize) -> SavedNode {
        if n == 1 {
            return SavedNode::Pane { cwd: None };
        }
        SavedNode::Split {
            axis: Axis::Vertical,
            ratio: 0.5,
            first: Box::new(balanced(n / 2)),
            second: Box::new(balanced(n - n / 2)),
        }
    }
    let windows: Vec<SavedWindow> = (0..9)
        .map(|_| SavedWindow { tabs: vec![tab(balanced(MAX_PANES_PER_TAB))], active: 0 })
        .collect();
    let r = persist::restore(&SavedState { version: VERSION, windows });
    let total: usize = r.windows.iter().map(|w| w.cwds.len()).sum();
    assert_eq!(total, MAX_PANES_TOTAL, "8 full windows fit, the 9th is dropped");
    assert_eq!(r.windows.len(), 8);
    assert!(r.warnings.iter().any(|w| w.contains("too many panes in total")), "{:?}", r.warnings);
}

#[test]
fn restore_drops_a_tab_nested_deeper_than_the_limit() {
    let ok = chain(MAX_DEPTH + 1); // depth 16
    let deep = chain(MAX_DEPTH + 2); // depth 17, only 18 panes
    let state = SavedState {
        version: VERSION,
        windows: vec![SavedWindow { tabs: vec![tab(ok), tab(deep)], active: 0 }],
    };
    let r = persist::restore(&state);
    assert_eq!(r.windows[0].workspace.tabs().len(), 1, "the depth-17 tab is dropped");
    assert_eq!(r.windows[0].workspace.tabs()[0].root.depth(), MAX_DEPTH);
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
}

#[test]
fn restored_directories_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let there = dir.path().join("there");
    std::fs::create_dir(&there).unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(resolve_cwd(Some(&there), &home), there);
    assert_eq!(resolve_cwd(Some(&dir.path().join("gone")), &home), home);
    assert_eq!(resolve_cwd(Some(&file), &home), home, "not a directory");
    assert_eq!(resolve_cwd(Some(Path::new("relative")), &home), home);
    assert_eq!(resolve_cwd(None, &home), home);
}

#[cfg(unix)]
#[test]
fn load_does_not_block_on_a_fifo() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("workspace.json");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let p = fifo.clone();
    std::thread::spawn(move || tx.send(persist::load(&p)).ok());
    let r = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("load returned");
    assert!(r.windows.is_empty());
    assert!(r.warnings[0].contains("not a regular file"), "{:?}", r.warnings);
}

#[cfg(unix)]
#[test]
fn save_refuses_symlinks_and_tightens_the_folder() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let (ws, ids) = sample();
    let cwd = cwd_for(&ids);
    let state = SavedState { version: VERSION, windows: vec![ws.to_saved(&cwd)] };

    // A symlinked state directory is refused.
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(persist::save(&link.join("workspace.json"), &state).is_err());
    assert!(std::fs::read_dir(&real).unwrap().next().is_none(), "nothing written through it");

    // An existing folder that is not Halyard's default (a user-set
    // HALYARD_STATE_DIR such as a shared folder) keeps its mode.
    let state_dir = dir.path().join("state");
    std::fs::create_dir(&state_dir).unwrap();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A symlink planted at the temporary file's name is not followed.
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    let tmp = state_dir.join(format!(".workspace.json.{}.tmp", std::process::id()));
    symlink(&victim, &tmp).unwrap();
    persist::save(&state_dir.join("workspace.json"), &state).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    let mode = std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "a folder save did not create is left alone");
    assert_eq!(persist::load(&state_dir.join("workspace.json")).windows.len(), 1);
    // A folder save creates is 0700, even under a permissive umask.
    let fresh = dir.path().join("fresh").join("halyard");
    // SAFETY: umask has no memory-safety preconditions; restored below.
    let old = unsafe { libc::umask(0o000) };
    let r = persist::save(&fresh.join("workspace.json"), &state);
    // SAFETY: as above.
    unsafe { libc::umask(old) };
    r.unwrap();
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}
