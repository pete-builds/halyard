//! Where the socket and grants file live, and the checks that make those places safe.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::peer::current_euid;

/// Longest socket path, in bytes, that fits `sockaddr_un.sun_path` with its NUL on every
/// supported platform (macOS has 104 bytes, Linux 108).
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

/// The directory that holds Halyard sockets for this user.
///
/// `$XDG_RUNTIME_DIR/halyard` when `XDG_RUNTIME_DIR` is set to an absolute path (Linux
/// with systemd or elogind: a per-user tmpfs, mode 0700). Otherwise
/// `$TMPDIR/halyard-<uid>`, falling back to `/tmp/halyard-<uid>`. On macOS `$TMPDIR` is
/// already a per-user directory under `/var/folders`.
///
/// This only computes the path. [`ensure_private_dir`] creates and checks it.
pub fn default_socket_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && runtime.is_absolute()
    {
        return runtime.join("halyard");
    }
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    tmp.join(format!("halyard-{}", current_euid()))
}

/// The default socket path for a Halyard instance: `<default_socket_dir>/<instance>.sock`.
///
/// `instance` is 1 to 32 characters of `[A-Za-z0-9_-]`. The app uses its pid, so several
/// windows or test instances never collide.
pub fn default_socket_path(instance: &str) -> Result<PathBuf, Error> {
    let valid = !instance.is_empty()
        && instance.len() <= 32
        && instance.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid {
        return Err(Error::InvalidName(instance.to_owned()));
    }
    Ok(default_socket_dir().join(format!("{instance}.sock")))
}

/// The default grants file: `$XDG_STATE_HOME/halyard/grants.json`, falling back to
/// `~/.local/state/halyard/grants.json`. `None` when neither variable is set.
pub fn default_grants_path() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from)
        && state.is_absolute()
    {
        return Some(state.join("halyard").join("grants.json"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())?;
    Some(home.join(".local/state/halyard/grants.json"))
}

/// Creates `dir` with mode 0700, or checks an existing one.
///
/// The directory must end up a real directory (not a symlink), owned by this process's
/// effective uid, with no group or other permission bits. A pre-existing directory that
/// fails any check is refused, never repaired: another user may have planted it to
/// intercept the socket. Creation uses `mkdir(2)` with the mode applied atomically, so
/// there is no window where the directory exists with wider permissions.
pub fn ensure_private_dir(dir: &Path) -> Result<(), Error> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(Error::io(format!("creating {}", dir.display()), e));
        }
    }
    check_private(dir, true)
}

/// Checks that `path` is owned by us, is not a symlink, is a directory (`want_dir`) or a
/// regular file, and has no group or other permission bits.
pub(crate) fn check_private(path: &Path, want_dir: bool) -> Result<(), Error> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| Error::io(format!("inspecting {}", path.display()), e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Err(Error::insecure(path, "it is a symlink"));
    }
    if want_dir && !ft.is_dir() {
        return Err(Error::insecure(path, "it is not a directory"));
    }
    if !want_dir && !ft.is_file() {
        return Err(Error::insecure(path, "it is not a regular file"));
    }
    let euid = current_euid();
    if meta.uid() != euid {
        return Err(Error::insecure(
            path,
            format!("it is owned by uid {}, not by uid {euid}", meta.uid()),
        ));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::insecure(
            path,
            format!("its mode is {mode:04o}; it must not be accessible to group or others"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_dir_with_mode_0700() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("run");
        ensure_private_dir(&dir).expect("create");
        let mode = fs::metadata(&dir).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // Idempotent on a correct directory.
        ensure_private_dir(&dir).expect("second call accepts our own 0700 dir");
    }

    #[test]
    fn refuses_existing_0755_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("run");
        fs::create_dir(&dir).expect("mkdir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        let err = ensure_private_dir(&dir).expect_err("0755 must be refused");
        assert!(matches!(err, Error::Insecure { .. }), "{err}");
        // Refused, not repaired.
        let mode = fs::metadata(&dir).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn refuses_symlink_to_private_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        ensure_private_dir(&real).expect("real dir");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let err = ensure_private_dir(&link).expect_err("symlink must be refused");
        assert!(matches!(err, Error::Insecure { .. }), "{err}");
    }

    #[test]
    fn refuses_a_file_where_the_dir_should_be() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("run");
        fs::write(&file, b"x").expect("write");
        assert!(matches!(ensure_private_dir(&file), Err(Error::Insecure { .. })));
    }

    #[test]
    fn instance_names_are_validated() {
        assert!(default_socket_path("12345").is_ok());
        assert!(default_socket_path("dev_1-a").is_ok());
        for bad in ["", "../x", "a/b", "a b", &"x".repeat(33)] {
            assert!(matches!(default_socket_path(bad), Err(Error::InvalidName(_))), "{bad:?}");
        }
    }
}
