//! Reading user files without ever blocking or reading without bound.
//!
//! A config or theme path can be replaced at any time by something that is not a regular file:
//! a FIFO with no writer blocks `open` and `read` forever, a device never ends, a directory
//! cannot be read, and a huge file would be read whole. Everything the crate reads from the
//! user's directories goes through [`read_regular`].

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

/// Files larger than this are refused: 4 MiB is far beyond any config or theme.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Why a file was not read, in words a warning can use. None of these hold file content.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// Not a regular file; the string names the kind (`fifo`, `directory`, `device`, ...).
    NotRegular(&'static str),
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// An ordinary I/O failure, including "not found".
    Io(io::Error),
}

impl Refusal {
    /// A phrase for a warning: "is a fifo, not a regular file" and the like.
    pub(crate) fn phrase(&self) -> String {
        match self {
            Refusal::NotRegular(kind) => format!("is a {kind}, not a regular file"),
            Refusal::TooLarge => format!("is larger than {} MiB", MAX_FILE_BYTES / (1024 * 1024)),
            Refusal::Io(e) => format!("cannot be read ({})", e.kind()),
        }
    }
}

/// The kind of a non-regular file, from metadata that followed symlinks.
pub(crate) fn kind_name(meta: &Metadata) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let t = meta.file_type();
        if t.is_fifo() {
            return "fifo";
        }
        if t.is_socket() {
            return "socket";
        }
        if t.is_block_device() || t.is_char_device() {
            return "device";
        }
    }
    if meta.is_dir() { "directory" } else { "special file" }
}

fn open_nonblocking(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO with no writer would block `open` forever without this.
        opts.custom_flags(libc::O_NONBLOCK);
    }
    opts.open(path)
}

/// Read a regular file, at most [`MAX_FILE_BYTES`], without ever blocking on a special file.
///
/// The type is checked on the path (following symlinks), the file is opened non-blocking, and
/// the type and size are checked again on the open descriptor, so a swap between the check and
/// the open cannot slip a FIFO or device through. The read itself is bounded.
pub(crate) fn read_regular(path: &Path) -> Result<Vec<u8>, Refusal> {
    let meta = fs::metadata(path).map_err(Refusal::Io)?;
    if !meta.is_file() {
        return Err(Refusal::NotRegular(kind_name(&meta)));
    }
    let file = open_nonblocking(path).map_err(Refusal::Io)?;
    let meta = file.metadata().map_err(Refusal::Io)?;
    if !meta.is_file() {
        return Err(Refusal::NotRegular(kind_name(&meta)));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Refusal::TooLarge);
    }
    let mut buf = Vec::new();
    // One byte past the limit shows a file that grew after the size check.
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).map_err(Refusal::Io)?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err(Refusal::TooLarge);
    }
    Ok(buf)
}
