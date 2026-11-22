//! Variables describing the launching terminal do not reach the program.
//! Its own test binary, because it changes this process's environment.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use halyard_term::{Session, SessionConfig};

#[test]
fn inherited_terminal_identity_is_scrubbed() {
    // SAFETY: this is the only test in this binary, and no other thread
    // reads the environment while it is being changed.
    unsafe {
        std::env::set_var("TMUX", "/tmp/tmux-1/default,1,0");
        std::env::set_var("KITTY_WINDOW_ID", "7");
        std::env::set_var("HALYARD_KEEP_ME", "kept");
    }
    let s = Session::spawn(SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        args: vec![
            "-c".into(),
            r#"echo "vars:[$TMUX][$KITTY_WINDOW_ID][$HALYARD_KEEP_ME]""#.into(),
        ],
        ..SessionConfig::default()
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let text = s.latest_frame().text();
        if text.contains("vars:") {
            assert!(text.contains("vars:[][][kept]"), "{text}");
            break;
        }
        assert!(Instant::now() < deadline, "no output: {text}");
        let _woken = s.events().recv_timeout(Duration::from_millis(100));
    }
}
