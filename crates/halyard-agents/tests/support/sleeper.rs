//! Test helper: sleeps for `argv[1]` seconds (default 5) whatever name it runs under.
//!
//! The foreground tests run it as `claude` to check detection by program name. They
//! cannot use `sleep` for that: on NixOS (coreutils), busybox and uutils, `sleep` is a
//! multicall binary that picks its behavior from `argv[0]` and exits at once when
//! called `claude`.

fn main() {
    let secs = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    std::thread::sleep(std::time::Duration::from_secs(secs));
}
