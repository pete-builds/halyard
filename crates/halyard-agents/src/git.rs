//! Reading the current git branch without running git.
//!
//! From a directory, walk up to the first `.git`. A `.git` directory holds `HEAD`
//! directly. A `.git` file (linked worktrees, submodules) holds `gitdir: <path>`,
//! relative to the file's directory or absolute, and `HEAD` lives there.
//!
//! Only `HEAD` is read, at most 4 KiB of it, and only if it is a regular file. Repositories using the reftable
//! backend keep a placeholder `HEAD` (`ref: refs/heads/.invalid`), reported as
//! [`GitHead::Unknown`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use halyard_shell::sanitize::sanitize;

/// Longest branch name kept.
pub const MAX_BRANCH_LEN: usize = 255;
const MAX_READ: u64 = 4096;

/// What `HEAD` points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHead {
    /// A branch, without the `refs/heads/` prefix.
    Branch(String),
    /// A detached commit, abbreviated to 12 hex digits.
    Detached(String),
    /// A repository was found but `HEAD` could not be understood.
    Unknown,
}

/// The `HEAD` of the repository containing `start`, or `None` outside any
/// repository.
pub fn git_head(start: &Path) -> Option<GitHead> {
    let git_dir = find_git_dir(start)?;
    Some(match read_small(&git_dir.join("HEAD")) {
        Some(text) => parse_head(&text),
        None => GitHead::Unknown,
    })
}

/// The git directory (`.git` or what a `.git` file points to) for `start`.
pub fn find_git_dir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        if meta.is_file() {
            let text = read_small(&dot_git)?;
            let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            if target.is_empty() {
                return None;
            }
            let target = Path::new(target);
            return Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                dir.join(target)
            });
        }
    }
    None
}

/// Reads at most [`MAX_READ`] bytes of a regular file. The directory comes from
/// untrusted terminal output, so anything else (a FIFO, a device, a socket) is
/// refused rather than opened: opening or reading a FIFO would block forever. The
/// file is opened non-blocking and checked again on the open descriptor, so swapping
/// it after the first check does not help.
fn read_small(path: &Path) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    file.take(MAX_READ).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Parses the contents of a `HEAD` file.
pub fn parse_head(text: &str) -> GitHead {
    let line = text.lines().next().unwrap_or("").trim();
    if let Some(target) = line.strip_prefix("ref:") {
        let target = target.trim();
        let name = target.strip_prefix("refs/heads/").unwrap_or(target);
        if name.is_empty() || name == ".invalid" {
            return GitHead::Unknown;
        }
        return GitHead::Branch(sanitize(name, MAX_BRANCH_LEN));
    }
    let is_hex = line.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex && (line.len() == 40 || line.len() == 64) {
        return GitHead::Detached(line[..12].to_ascii_lowercase());
    }
    GitHead::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "halyard-git-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn parse_head_variants() {
        assert_eq!(parse_head("ref: refs/heads/main\n"), GitHead::Branch("main".into()));
        assert_eq!(parse_head("ref: refs/heads/feat/x-y\n"), GitHead::Branch("feat/x-y".into()));
        assert_eq!(
            parse_head("ref:refs/remotes/origin/main"),
            GitHead::Branch("refs/remotes/origin/main".into())
        );
        assert_eq!(
            parse_head("8619358ABCDEF0123456789abcdef0123456789a\n"),
            GitHead::Detached("8619358abcde".into())
        );
        assert_eq!(parse_head(&"a".repeat(64)), GitHead::Detached("a".repeat(12)));
        assert_eq!(parse_head("ref: refs/heads/.invalid"), GitHead::Unknown);
        assert_eq!(parse_head("ref: "), GitHead::Unknown);
        assert_eq!(parse_head("deadbeef"), GitHead::Unknown);
        assert_eq!(parse_head(""), GitHead::Unknown);
        assert_eq!(
            parse_head("ref: refs/heads/evil\u{1b}]0;x\u{7}"),
            GitHead::Branch("evil]0;x".into())
        );
    }

    #[test]
    fn plain_repository_from_a_subdirectory() {
        let t = TempDir::new("plain");
        write(&t.0.join(".git/HEAD"), "ref: refs/heads/main\n");
        let deep = t.0.join("src/a/b");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(git_head(&deep), Some(GitHead::Branch("main".into())));
        assert_eq!(git_head(&t.0), Some(GitHead::Branch("main".into())));
    }

    #[test]
    fn worktree_with_relative_and_absolute_gitdir() {
        let t = TempDir::new("worktree");
        write(&t.0.join("main/.git/HEAD"), "ref: refs/heads/main\n");
        write(&t.0.join("main/.git/worktrees/wt/HEAD"), "ref: refs/heads/feature\n");
        let abs = t.0.join("main/.git/worktrees/wt");
        write(&t.0.join("wt/.git"), &format!("gitdir: {}\n", abs.display()));
        assert_eq!(git_head(&t.0.join("wt")), Some(GitHead::Branch("feature".into())));
        // Submodules use a relative gitdir.
        write(&t.0.join("main/.git/modules/sub/HEAD"), &"c".repeat(40));
        write(&t.0.join("main/sub/.git"), "gitdir: ../.git/modules/sub\n");
        fs::create_dir_all(t.0.join("main/sub/deep")).unwrap();
        assert_eq!(git_head(&t.0.join("main/sub/deep")), Some(GitHead::Detached("c".repeat(12))));
    }

    #[test]
    fn broken_layouts() {
        let t = TempDir::new("broken");
        // .git directory without HEAD.
        fs::create_dir_all(t.0.join("a/.git")).unwrap();
        assert_eq!(git_head(&t.0.join("a")), Some(GitHead::Unknown));
        // .git file pointing nowhere.
        write(&t.0.join("b/.git"), "gitdir: /nonexistent/halyard/x\n");
        assert_eq!(git_head(&t.0.join("b")), Some(GitHead::Unknown));
        // .git file without a gitdir line.
        write(&t.0.join("c/.git"), "garbage\n");
        assert_eq!(git_head(&t.0.join("c")), None);
        write(&t.0.join("d/.git"), "gitdir:   \n");
        assert_eq!(git_head(&t.0.join("d")), None);
    }

    #[test]
    fn outside_any_repository() {
        // The filesystem root is not a repository on CI machines or here.
        assert_eq!(git_head(Path::new("/")), None);
    }

    #[test]
    fn read_is_capped_at_4_kib() {
        let t = TempDir::new("cap");
        let p = t.0.join("big");
        fs::write(&p, vec![b'x'; 1 << 20]).unwrap();
        assert_eq!(read_small(&p).unwrap().len(), MAX_READ as usize);
    }

    #[cfg(unix)]
    fn mkfifo(p: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path and a mode.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    }

    /// Runs `git_head` on another thread and fails instead of hanging.
    #[cfg(unix)]
    fn git_head_within(dir: PathBuf, limit: std::time::Duration) -> Option<GitHead> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(git_head(&dir));
        });
        rx.recv_timeout(limit).expect("git_head blocked on a FIFO")
    }

    #[cfg(unix)]
    #[test]
    fn fifo_head_does_not_block() {
        let t = TempDir::new("fifo");
        // .git/HEAD is a FIFO with no writer.
        fs::create_dir_all(t.0.join("a/.git")).unwrap();
        mkfifo(&t.0.join("a/.git/HEAD"));
        let limit = std::time::Duration::from_secs(5);
        assert_eq!(git_head_within(t.0.join("a"), limit), Some(GitHead::Unknown));
        // A .git that is itself a FIFO is not a repository marker.
        fs::create_dir_all(t.0.join("b")).unwrap();
        mkfifo(&t.0.join("b/.git"));
        assert_eq!(git_head_within(t.0.join("b"), limit), None);
        // A worktree whose gitdir points at a directory with a FIFO HEAD.
        fs::create_dir_all(t.0.join("gd")).unwrap();
        mkfifo(&t.0.join("gd/HEAD"));
        write(&t.0.join("c/.git"), &format!("gitdir: {}\n", t.0.join("gd").display()));
        assert_eq!(git_head_within(t.0.join("c"), limit), Some(GitHead::Unknown));
        // Reading a FIFO directly is refused.
        assert_eq!(read_small(&t.0.join("gd/HEAD")), None);
    }

    #[test]
    fn huge_head_is_capped() {
        let t = TempDir::new("huge");
        let name = "b".repeat(10_000);
        write(&t.0.join(".git/HEAD"), &format!("ref: refs/heads/{name}\n"));
        match git_head(&t.0) {
            Some(GitHead::Branch(b)) => assert_eq!(b.len(), MAX_BRANCH_LEN),
            other => panic!("{other:?}"),
        }
    }
}
