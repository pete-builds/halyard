//! TEMPORARY experiment, removed before merge: do shells born while other
//! PTYs close die young, and of what? Always fails so CI prints the numbers.
#![cfg(target_os = "macos")]

use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use halyard_term::{Session, SessionConfig};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn size() -> PtySize {
    PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }
}

/// A raw `/bin/sh -i` in a fresh PTY; after `ms` reports how it ended if it
/// already did, with whatever it printed.
fn victim(ms: u64) -> Option<String> {
    let pair = native_pty_system().openpty(size()).ok()?;
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-i");
    let mut child = pair.slave.spawn_command(cmd).ok()?;
    let pid = child.process_id();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().ok()?;
    let out = Arc::new(Mutex::new(Vec::new()));
    let out2 = Arc::clone(&out);
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            out2.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    std::thread::sleep(Duration::from_millis(ms));
    let early = child.try_wait().ok().flatten();
    if early.is_none() {
        child.kill().ok();
    }
    let status = child.wait().ok();
    drop(pair.master);
    t.join().ok();
    early.map(|s| {
        format!(
            "pid {pid:?} {s:?} (final {status:?}) printed {:?}",
            String::from_utf8_lossy(&out.lock().unwrap())
        )
    })
}

/// Session churn like the app tests: spawn, maybe run `def`, drop.
fn session_closer(n: u64) {
    let session = Session::spawn(SessionConfig {
        program: Some("/bin/sh".into()),
        args: vec!["-i".into()],
        cols: 80,
        rows: 24,
        ..SessionConfig::default()
    })
    .expect("spawn");
    if n.is_multiple_of(2) {
        session.write("def\n");
    }
    std::thread::sleep(Duration::from_millis(10 + n % 40));
    drop(session);
}

/// Raw churn: spawn, then close the master with no signal of our own (the
/// kernel hangs up the shell), and reap it.
fn raw_closer(n: u64) {
    let pair = native_pty_system().openpty(size()).unwrap();
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-i");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    std::thread::sleep(Duration::from_millis(10 + n % 40));
    drop(pair.master);
    child.wait().ok();
}

fn experiment(name: &str, secs: u64, closer: Option<fn(u64)>) -> String {
    let end = Instant::now() + Duration::from_secs(secs);
    let births = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let deaths = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut threads = Vec::new();
    for t in 0..3u64 {
        let (births, deaths) = (Arc::clone(&births), Arc::clone(&deaths));
        threads.push(std::thread::spawn(move || {
            let mut n = 0;
            while Instant::now() < end {
                n += 1;
                births.fetch_add(1, Ordering::Relaxed);
                if let Some(d) = victim(30 + (n * 7 + t) % 40) {
                    deaths.lock().unwrap().push(d);
                }
            }
        }));
        if let Some(close) = closer {
            let closes = Arc::clone(&closes);
            threads.push(std::thread::spawn(move || {
                let mut n = t;
                while Instant::now() < end {
                    n += 1;
                    close(n);
                    closes.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
    }
    for t in threads {
        t.join().unwrap();
    }
    let d = deaths.lock().unwrap();
    let mut s = format!(
        "{name}: {} victims, {} closes, {} died young\n",
        births.load(Ordering::Relaxed),
        closes.load(Ordering::Relaxed),
        d.len()
    );
    for line in d.iter().take(15) {
        s.push_str(&format!("    {line}\n"));
    }
    s
}

#[test]
fn tmp_shells_born_while_others_close() {
    let mut report = String::new();
    report.push_str(&experiment("victims alone", 30, None));
    report.push_str(&experiment("with Session churn", 40, Some(session_closer)));
    report.push_str(&experiment("with raw close churn", 40, Some(raw_closer)));
    panic!("EXPERIMENT REPORT\n{report}");
}
