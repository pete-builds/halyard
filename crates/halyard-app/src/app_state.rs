//! App-wide state: settings, key bindings and the config watcher.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, BorrowAppContext, Global};
use halyard_config::{Config, ConfigWatcher};
use halyard_workspace::persist;

use crate::keybind;
use crate::settings::Settings;

/// Shared by every window.
pub struct AppState {
    /// The configuration and themes.
    pub settings: Settings,
    /// Keeps the config watcher thread alive.
    _watcher: Option<std::thread::JoinHandle<()>>,
    /// Debug scripts run in the first window only.
    #[cfg(feature = "debug-script")]
    pub script_started: bool,
}

impl Global for AppState {}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("settings", &self.settings).finish_non_exhaustive()
    }
}

/// This machine's host name, for deciding whether an OSC 7 report is local.
pub fn hostname() -> Option<&'static str> {
    static HOST: OnceLock<Option<String>> = OnceLock::new();
    HOST.get_or_init(persist::hostname).as_deref()
}

impl AppState {
    /// Load settings and install the global, the key bindings and the
    /// config watcher.
    pub fn init(cx: &mut App) {
        let path = halyard_config::config_path();
        let settings = Settings::load(path.as_deref());
        let watcher = path.as_ref().and_then(|p| start_watcher(p.clone(), cx));
        cx.set_global(AppState {
            settings,
            _watcher: watcher,
            #[cfg(feature = "debug-script")]
            script_started: false,
        });
        rebind(cx);
    }
}

/// Bind the current keymap's chords to GPUI actions.
pub fn rebind(cx: &mut App) {
    let (bindings, skipped) = keybind::bindings(&cx.global::<AppState>().settings.keymap);
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    if !skipped.is_empty() {
        let state = cx.global_mut::<AppState>();
        for s in skipped {
            state.settings.notices.push(format!("config: key chord {s} cannot be bound"));
        }
    }
}

/// Apply a config load result everywhere: settings, key bindings, windows.
pub fn apply_config(event: halyard_config::ConfigEvent, cx: &mut App) {
    cx.update_global::<AppState, _>(|state, _| state.settings.apply(event));
    rebind(cx);
}

/// Re-read the config file now (the `reload_config` action).
pub fn reload_config(cx: &mut App) {
    let path = cx.global::<AppState>().settings.path.clone();
    match path {
        Some(p) => apply_config(Config::load(&p), cx),
        None => apply_config(
            Ok(halyard_config::Loaded { config: Config::default(), warnings: Vec::new() }),
            cx,
        ),
    }
}

/// Open the config file in the system's editor, writing the commented
/// default first if it does not exist yet.
pub fn open_config(cx: &mut App) -> Result<(), String> {
    let path = cx
        .global::<AppState>()
        .settings
        .path
        .clone()
        .ok_or_else(|| "no home directory, so there is no config path".to_owned())?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create the config folder: {e}"))?;
        }
        std::fs::write(&path, halyard_config::default_config_toml())
            .map_err(|e| format!("cannot write the config file: {e}"))?;
    }
    cx.open_with_system(&path);
    Ok(())
}

/// Watch the config file on a background thread and apply each change on
/// the UI thread. The thread blocks between events, so it costs nothing
/// while idle beyond the watcher's own once-a-second check.
fn start_watcher(path: PathBuf, cx: &mut App) -> Option<std::thread::JoinHandle<()>> {
    let watcher = match ConfigWatcher::new(&path, Duration::from_millis(150)) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("halyard: config live reload is off: {e}");
            return None;
        }
    };
    let (tx, mut rx) = mpsc::unbounded();
    let handle = std::thread::Builder::new()
        .name("halyard-config-forward".into())
        .spawn(move || {
            loop {
                if let Some(ev) = watcher.recv_timeout(Duration::from_secs(1)) {
                    if tx.unbounded_send(ev).is_err() {
                        break;
                    }
                } else if tx.is_closed() {
                    break;
                }
            }
        })
        .map_err(|e| eprintln!("halyard: config live reload is off: {e}"))
        .ok()?;
    cx.spawn(async move |cx| {
        while let Some(ev) = rx.next().await {
            if cx.update(|cx| apply_config(ev, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Some(handle)
}
