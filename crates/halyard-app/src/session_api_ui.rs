//! Starting the session API and the UI half of it: the loop that runs on the UI
//! thread and answers [`UiRequest`]s from the server's connection threads.
//!
//! The server binds on its own thread, so a slow grants-file lock (another
//! instance holding it) never delays the first window. Panes are told the socket
//! path from the start; until the bind finishes, or if it fails, `halyard-msg`
//! finds nothing listening and falls back to escape sequences.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, WindowHandle};
use halyard_ipc::paths::{default_grants_path, default_socket_path};
use halyard_ipc::{DirectoryError, EventHub, GrantStore, IpcServer, ServerConfig};

use crate::app_state::AppState;
use crate::approval::Ask;
use crate::session_api::{
    AppDirectory, DIALOG_TIMEOUT, DialogApprover, EventPublisher, PaneApi, Registry, ServerSlot,
    UiRequest,
};
use crate::workspace_view::{Reply, WorkspaceView};

/// Starts the session API when the config enables it, returning what each pane
/// needs. Problems become a banner notice; the terminal works either way.
pub fn start(cx: &mut App) -> Option<PaneApi> {
    if !cx.global::<AppState>().settings.config.session_api.enabled {
        return None;
    }
    let socket = match default_socket_path(&std::process::id().to_string()) {
        Ok(p) => p,
        Err(e) => {
            notice(format!("session API is off: {e}"), cx);
            return None;
        }
    };
    let (tx, mut rx) = mpsc::unbounded::<UiRequest>();
    let registry = Registry::default();
    let publisher = Arc::new(Mutex::new(EventPublisher::default()));
    let events = EventHub::new();
    let slot: ServerSlot = Arc::default();
    let api = PaneApi::new(
        socket.clone(),
        registry.clone(),
        Arc::clone(&slot),
        Arc::clone(&publisher),
        events.clone(),
    );
    let directory = Arc::new(AppDirectory::new(registry, tx.clone(), events.clone(), publisher));
    let approver = Arc::new(DialogApprover::new(tx.clone(), DIALOG_TIMEOUT));
    let notices = tx;
    let bind = std::thread::Builder::new().name("halyard-ipc-bind".into()).spawn(move || {
        let grants = match default_grants_path() {
            Some(p) => GrantStore::open(p),
            // No home directory: pairings last until Halyard quits.
            None => Ok(GrantStore::in_memory()),
        };
        let bound = grants.and_then(|grants| {
            let mut config = ServerConfig::new(socket, grants);
            config.events = events;
            IpcServer::bind(config, directory, approver)
        });
        match bound {
            Ok(server) => {
                *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Arc::new(server));
            }
            Err(e) => {
                // The UI loop is alive for the app's lifetime.
                notices.unbounded_send(UiRequest::Notice(format!("session API is off: {e}"))).ok();
            }
        }
    });
    if let Err(e) = bind {
        notice(format!("session API is off: {e}"), cx);
        return None;
    }
    cx.spawn(async move |cx| {
        while let Some(request) = rx.next().await {
            if cx.update(|cx| handle(request, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Some(api)
}

/// Stops the server and removes its socket (at quit).
pub fn shutdown(cx: &mut App) {
    let server = cx.global::<AppState>().session_api.as_ref().and_then(PaneApi::take_server);
    drop(server);
}

fn workspace_windows(cx: &App) -> Vec<WindowHandle<WorkspaceView>> {
    cx.windows().into_iter().filter_map(|w| w.downcast::<WorkspaceView>()).collect()
}

/// Shows a notice in the first window, or keeps it for the first window to open.
fn notice(text: String, cx: &mut App) {
    eprintln!("halyard: {text}");
    let shown = workspace_windows(cx)
        .first()
        .is_some_and(|w| w.update(cx, |view, _, cx| view.push_notice(text.clone(), cx)).is_ok());
    if !shown {
        cx.global_mut::<AppState>().restore_warnings.push(text);
    }
}

/// The window that holds the pane `session_id`.
fn window_of(session_id: &str, cx: &App) -> Option<WindowHandle<WorkspaceView>> {
    workspace_windows(cx)
        .into_iter()
        .find(|w| w.read(cx).is_ok_and(|view| view.pane_for_session(session_id, cx).is_some()))
}

fn handle(request: UiRequest, cx: &mut App) {
    match request {
        UiRequest::Notice(text) => notice(text, cx),
        UiRequest::Changed { session_id } => {
            if let Some(w) = window_of(&session_id, cx) {
                w.update(cx, |view, window, cx| view.api_changed(&session_id, window, cx)).ok();
            }
        }
        UiRequest::Focus { session_id, reply } => {
            let done = window_of(&session_id, cx).is_some_and(|w| {
                w.update(cx, |view, window, cx| {
                    let found = view.focus_session(&session_id, window, cx);
                    if found {
                        window.activate_window();
                    }
                    found
                })
                .unwrap_or(false)
            });
            // The requester may have timed out and gone; nothing to do then.
            reply.try_send(if done { Ok(()) } else { Err(DirectoryError::NotFound) }).ok();
        }
        UiRequest::Pairing { request, reply } => {
            let active = cx.active_window().and_then(|w| w.downcast::<WorkspaceView>());
            let target = active.or_else(|| workspace_windows(cx).into_iter().next());
            // No window to ask in: dropping the reply denies.
            if let Some(w) = target {
                w.update(cx, |view, window, cx| {
                    view.show_approval(Ask::Pairing(request), Reply::Pairing(reply), window, cx);
                })
                .ok();
            }
        }
        UiRequest::SendText { request, reply } => {
            if let Some(w) = window_of(&request.session.id, cx) {
                w.update(cx, |view, window, cx| {
                    view.show_approval(Ask::SendText(request), Reply::Text(reply), window, cx);
                })
                .ok();
            }
        }
    }
}
