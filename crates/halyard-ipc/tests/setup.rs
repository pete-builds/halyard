//! Socket placement and lifecycle: permissions, stale sockets, shutdown.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::Arc;

use common::{FakeDirectory, ScriptedApprover, start};
use halyard_ipc::{Error, GrantStore, IpcServer, ServerConfig};

fn bind_at(socket: &std::path::Path) -> Result<IpcServer, Error> {
    IpcServer::bind(
        ServerConfig::new(socket, GrantStore::in_memory()),
        Arc::new(FakeDirectory::default()),
        Arc::new(ScriptedApprover::default()),
    )
}

fn mode(p: &std::path::Path) -> u32 {
    fs::symlink_metadata(p).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn socket_is_0600_inside_a_0700_directory() {
    let h = start();
    assert_eq!(mode(&h.socket), 0o600);
    assert_eq!(mode(h.socket.parent().unwrap()), 0o700);
}

#[test]
fn a_0755_socket_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let err = bind_at(&dir.join("x.sock")).expect_err("0755 dir must be refused");
    assert!(matches!(err, Error::Insecure { .. }), "{err}");
    assert!(!dir.join("x.sock").exists(), "nothing was created");
}

#[test]
fn a_symlinked_socket_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let link = tmp.path().join("run");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(bind_at(&link.join("x.sock")), Err(Error::Insecure { .. })));
}

#[test]
fn a_live_socket_is_not_stolen() {
    let h = start();
    assert!(matches!(bind_at(&h.socket), Err(Error::AddressInUse(_))));
    // The first server still works.
    h.hello();
}

#[test]
fn a_stale_socket_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("x.sock");
    drop(UnixListener::bind(&socket).unwrap()); // leaves the file behind, like a crash
    assert!(socket.exists());
    let _server = bind_at(&socket).expect("stale socket replaced");
}

#[test]
fn a_regular_file_in_the_way_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("x.sock");
    fs::write(&socket, b"not a socket").unwrap();
    assert!(matches!(bind_at(&socket), Err(Error::Insecure { .. })));
    assert_eq!(fs::read(&socket).unwrap(), b"not a socket", "the file was left alone");
}

#[test]
fn an_overlong_path_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("d".repeat(120)).join("x.sock");
    assert!(matches!(bind_at(&socket), Err(Error::PathTooLong(_))));
}

#[test]
fn shutdown_closes_connections_and_removes_the_socket() {
    let h = start();
    let mut c = h.hello();
    let socket = h.socket.clone();
    h.server.shutdown();
    assert!(!socket.exists());
    assert!(c.call("sessions.list", serde_json::json!({})).is_err());
}
