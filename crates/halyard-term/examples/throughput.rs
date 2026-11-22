//! Output throughput benchmark: how fast bytes go from a program, through the
//! PTY and the VT engine, into published frames. No window, no GPU. Method and
//! results: `docs/PERFORMANCE.md`.
//!
//! ```sh
//! cargo run --release -p halyard-term --example throughput            # full run
//! cargo run --release -p halyard-term --example throughput -- --quick # CI smoke test
//! ```
//!
//! Measurements:
//!
//! - **engine**: [`Engine::write`] on an in-memory buffer (parse cost only, no
//!   PTY, no frames), in 64 KiB and 1 KiB writes.
//! - **frame**: [`Engine::frame`] on a full screen (the cost of one published
//!   frame).
//! - **session**: a real [`Session`] running a shell pipeline that writes
//!   `--mb` megabytes, timed from spawn to the `Exited` event. Reports MB/s,
//!   wall time, the CPU time of the session and reader threads, the whole
//!   process's CPU, how many frames were published and how many chunks the
//!   reader handed over.
//! - **echo**: time from writing one byte to `cat` until a frame shows it.
//!
//! Every session run checks that a marker printed after the flood is on the
//! final frame, so a run that drops output or misses the last frame fails.
//! `--quick` runs small payloads once and exits non-zero when a throughput is
//! below a loose floor or `yes-head` uses too much CPU, to catch a large
//! regression in CI.

// The report is this program's output.
#![allow(clippy::print_stdout)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use halyard_term::{Engine, Palette, Session, SessionConfig, SessionEvent};

/// Grid size for every run: a large but ordinary window.
const COLS: u16 = 200;
const ROWS: u16 = 50;
/// Lowest acceptable throughput in `--quick` mode, in MB/s.
const QUICK_FLOOR_MBPS: f64 = 5.0;
/// `yes-head` is limited by the program's own 2-byte writes (about 6 MB/s on
/// an M4 Pro), so its throughput floor is lower.
const QUICK_FLOOR_YES_HEAD_MBPS: f64 = 1.0;
/// Most process CPU time per MB of `yes-head` output in `--quick` mode. Before
/// the reader paused after small reads this was 0.34 s/MB on an M4 Pro; after,
/// about 0.04 s/MB.
const QUICK_CEILING_YES_HEAD_CPU_PER_MB: f64 = 0.2;
/// Printed after every flood; must be on the last published frame.
const DONE_MARKER: &str = "bench-done";

#[derive(Clone, Copy, Debug)]
enum Workload {
    /// `cat` of N bytes of `a`: one endless line that wraps. (The pipeline
    /// `head -c N /dev/zero | tr '\0' a` writes the same bytes, but macOS `tr`
    /// alone runs at about 26 MB/s, so it measures `tr`, not the terminal.)
    LongLine,
    /// `cat` of `yes` output: many two-byte lines in large writes, as plain
    /// `yes` writes them.
    Yes,
    /// `yes | head -c N`: the same lines, but `head` writing to a terminal is
    /// line buffered, so the program makes one 2-byte `write` per line.
    YesHead,
    /// A generated file of colored, mixed-length lines.
    Mixed,
}

impl Workload {
    const ALL: [Self; 4] = [Self::LongLine, Self::Yes, Self::YesHead, Self::Mixed];

    const fn name(self) -> &'static str {
        match self {
            Self::LongLine => "long-line",
            Self::Yes => "yes",
            Self::YesHead => "yes-head",
            Self::Mixed => "mixed",
        }
    }

    /// The bytes the program writes, as the engine receives them after the
    /// PTY's output processing turns `\n` into `\r\n`.
    fn bytes(self, len: usize) -> Vec<u8> {
        match self {
            Self::LongLine => vec![b'a'; len],
            Self::Yes | Self::YesHead => b"y\r\n".iter().copied().cycle().take(len).collect(),
            Self::Mixed => mixed_text(len).replace('\n', "\r\n").into_bytes(),
        }
    }
}

/// Realistic terminal output: log and compiler style lines of varying length
/// with SGR colors, bold, 256-color and truecolor runs, and some UTF-8.
fn mixed_text(len: usize) -> String {
    const WORDS: &[&str] = &[
        "compiling",
        "halyard",
        "warning:",
        "error[E0308]:",
        "src/session.rs",
        "->",
        "Ok(())",
        "let",
        "mut",
        "frame",
        "=",
        "engine.frame()?;",
        "δ",
        "ünïcode",
        "漢字",
        "12.5ms",
        "done",
        "test",
        "result:",
        "passed",
        "the",
        "quick",
        "brown",
        "fox",
        "jumps",
        "over",
        "lazy",
    ];
    const STYLES: &[&str] = &[
        "\x1b[0m",
        "\x1b[31m",
        "\x1b[32m",
        "\x1b[1;33m",
        "\x1b[34m",
        "\x1b[2m",
        "\x1b[38;5;208m",
        "\x1b[38;2;120;200;255m",
        "\x1b[4m",
        "\x1b[7m",
        "\x1b[39m",
        "\x1b[0;1m",
    ];
    // A small deterministic generator, so every run sees the same bytes.
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % n as u64).unwrap_or(0)
    };
    let mut out = String::with_capacity(len + 256);
    while out.len() < len {
        let words = 2 + next(24);
        for i in 0..words {
            if next(4) == 0 {
                out.push_str(STYLES[next(STYLES.len())]);
            }
            if i > 0 {
                out.push(' ');
            }
            out.push_str(WORDS[next(WORDS.len())]);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

#[derive(Clone, Debug, Default)]
struct Sample {
    mbps: f64,
    wall: Duration,
    session_cpu: Duration,
    reader_cpu: Duration,
    process_cpu: Duration,
    frames: u64,
    chunks: u64,
}

fn median<T: Copy + PartialOrd>(mut v: Vec<T>) -> T {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / 1e6
}

struct Options {
    quick: bool,
    runs: usize,
    session_mb: usize,
    engine_mb: usize,
    /// Run only the workload with this name.
    only: Option<String>,
}

impl Options {
    fn workloads(&self) -> impl Iterator<Item = Workload> + '_ {
        Workload::ALL.into_iter().filter(|w| self.only.as_deref().is_none_or(|o| o == w.name()))
    }
}

fn parse_args() -> Options {
    let mut o = Options { quick: false, runs: 3, session_mb: 100, engine_mb: 100, only: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut num = |name: &str| {
            args.next().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0).unwrap_or_else(
                || {
                    eprintln!("{name} needs a positive number");
                    std::process::exit(2)
                },
            )
        };
        match a.as_str() {
            "--quick" => o.quick = true,
            "--runs" => o.runs = num("--runs"),
            "--mb" => o.session_mb = num("--mb"),
            "--engine-mb" => o.engine_mb = num("--engine-mb"),
            "--only" => o.only = args.next(),
            "--bench" => {} // passed by `cargo bench`
            other => {
                eprintln!(
                    "unknown argument {other}\nusage: throughput [--quick] [--runs N] [--mb N] [--engine-mb N] [--only long-line|yes|yes-head|mixed]"
                );
                std::process::exit(2);
            }
        }
    }
    if o.quick {
        o.runs = 1;
        o.session_mb = 5;
        o.engine_mb = 20;
    }
    o
}

fn main() {
    let o = parse_args();
    println!(
        "halyard-term throughput: grid {COLS}x{ROWS}, {} run(s), session payload {} MB, engine payload {} MB{}",
        o.runs,
        o.session_mb,
        o.engine_mb,
        if cfg!(debug_assertions) { " (DEBUG BUILD: numbers are not representative)" } else { "" }
    );
    let mut failures = Vec::new();

    println!("\n== engine: Engine::write on an in-memory buffer (parse only) ==");
    // yes-head feeds the engine the same bytes as yes.
    for w in o.workloads().filter(|w| !matches!(w, Workload::YesHead)) {
        let data = w.bytes(o.engine_mb * 1_000_000);
        // 64 KiB is the reader's buffer; 1 KiB is the most a macOS PTY read
        // returns, so it is what the engine sees there.
        for (chunk, label) in [(64 * 1024, "64K"), (1024, "1K")] {
            let rates: Vec<f64> = (0..o.runs).map(|_| engine_write(&data, chunk)).collect();
            let m = median(rates.clone());
            println!(
                "engine/{:<9} {label:>3} writes  median {m:>8.1} MB/s  runs {}",
                w.name(),
                fmt_rates(&rates)
            );
            if m < QUICK_FLOOR_MBPS {
                failures.push(format!(
                    "engine/{} ({label}) {m:.1} MB/s is below {QUICK_FLOOR_MBPS} MB/s",
                    w.name()
                ));
            }
        }
    }

    println!("\n== engine: Engine::frame on a full {COLS}x{ROWS} screen ==");
    // yes-head feeds the engine the same bytes as yes.
    for w in o.workloads().filter(|w| !matches!(w, Workload::YesHead)) {
        let per = frame_cost(w, if o.quick { 50 } else { 500 });
        println!("frame/{:<10} {:>8.1} us per frame", w.name(), per.as_secs_f64() * 1e6);
    }

    println!("\n== session: real PTY, spawn to exit ==");
    let dir = std::env::temp_dir().join(format!("halyard-throughput-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        std::process::exit(1);
    }
    for w in o.workloads() {
        let bytes = o.session_mb * 1_000_000;
        let script = match session_script(w, bytes, &dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot prepare {}: {e}", w.name());
                std::fs::remove_dir_all(&dir).ok();
                std::process::exit(1);
            }
        };
        let mut samples = Vec::new();
        for _ in 0..o.runs {
            match session_run(&script, bytes) {
                Ok(s) => samples.push(s),
                Err(e) => {
                    eprintln!("session/{} FAILED: {e}", w.name());
                    std::fs::remove_dir_all(&dir).ok();
                    std::process::exit(1);
                }
            }
        }
        let pick = |f: fn(&Sample) -> f64| median(samples.iter().map(f).collect::<Vec<_>>());
        let m = pick(|s| s.mbps);
        println!(
            "session/{:<9} median {m:>7.1} MB/s  wall {:>6.2}s  session cpu {:>6.2}s  reader cpu {:>5.2}s  process cpu {:>6.2}s  frames {:>5.0}  chunks {:>7.0} (avg {:.0} B)  runs {}",
            w.name(),
            pick(|s| s.wall.as_secs_f64()),
            pick(|s| s.session_cpu.as_secs_f64()),
            pick(|s| s.reader_cpu.as_secs_f64()),
            pick(|s| s.process_cpu.as_secs_f64()),
            pick(|s| s.frames as f64),
            pick(|s| s.chunks as f64),
            pick(|s| s.chunks.max(1) as f64).recip() * bytes as f64,
            fmt_rates(&samples.iter().map(|s| s.mbps).collect::<Vec<_>>()),
        );
        let floor = match w {
            Workload::YesHead => QUICK_FLOOR_YES_HEAD_MBPS,
            _ => QUICK_FLOOR_MBPS,
        };
        if m < floor {
            failures.push(format!("session/{} {m:.1} MB/s is below {floor} MB/s", w.name()));
        }
        let cpu_per_mb = pick(|s| s.process_cpu.as_secs_f64()) / mb(bytes);
        if matches!(w, Workload::YesHead) && cpu_per_mb > QUICK_CEILING_YES_HEAD_CPU_PER_MB {
            failures.push(format!(
                "session/{} used {cpu_per_mb:.3} s CPU per MB, above {QUICK_CEILING_YES_HEAD_CPU_PER_MB}",
                w.name()
            ));
        }
    }
    std::fs::remove_dir_all(&dir).ok();

    println!(
        "\n== session: key echo latency (write one byte, wait for the frame that shows it) =="
    );
    let n = if o.quick { 20 } else { 150 };
    match echo_latency(n) {
        Ok(mut lat) => {
            lat.sort();
            let pct = |p: usize| lat[(lat.len() * p / 100).min(lat.len() - 1)].as_secs_f64() * 1e3;
            println!(
                "echo      {n} samples  median {:.3} ms  p95 {:.3} ms  max {:.3} ms",
                pct(50),
                pct(95),
                pct(100)
            );
        }
        Err(e) => {
            eprintln!("echo latency FAILED: {e}");
            std::process::exit(1);
        }
    }

    if o.quick && !failures.is_empty() {
        eprintln!("\nFAIL:\n  {}", failures.join("\n  "));
        std::process::exit(1);
    }
    std::io::stdout().flush().ok();
}

fn fmt_rates(r: &[f64]) -> String {
    r.iter().map(|x| format!("{x:.1}")).collect::<Vec<_>>().join(" / ")
}

fn new_engine() -> Engine {
    let mut e = Engine::new(COLS, ROWS, 10_000, &Palette::default()).expect("engine");
    e.resize(COLS, ROWS, 8, 16).expect("resize");
    e
}

/// Parse `data` in slices of `chunk` bytes; returns MB/s.
fn engine_write(data: &[u8], chunk: usize) -> f64 {
    let mut e = new_engine();
    let start = Instant::now();
    for chunk in data.chunks(chunk) {
        e.write(chunk);
    }
    let secs = start.elapsed().as_secs_f64();
    std::hint::black_box(e.take_effects());
    mb(data.len()) / secs
}

/// Average time to build one frame of a full screen of `w` content. The
/// screen changes between frames (one more line), as it does while output
/// streams, so every row is dirty.
fn frame_cost(w: Workload, frames: u32) -> Duration {
    let mut e = new_engine();
    let fill = w.bytes(usize::from(COLS) * usize::from(ROWS) * 2);
    e.write(&fill);
    let line = w.bytes(usize::from(COLS) + 2);
    let mut total = Duration::ZERO;
    for _ in 0..frames {
        e.write(&line);
        let start = Instant::now();
        let f = e.frame().expect("frame");
        total += start.elapsed();
        std::hint::black_box(f);
    }
    total / frames
}

/// The shell command for a session run. The marker is printed after the flood.
fn session_script(w: Workload, bytes: usize, dir: &Path) -> std::io::Result<String> {
    let flood = match w {
        Workload::LongLine => {
            let file = dir.join("long-line.txt");
            std::fs::write(&file, vec![b'a'; bytes])?;
            format!("cat '{}'", file.display())
        }
        Workload::Yes => {
            let file = dir.join("yes.txt");
            std::fs::write(&file, b"y\n".repeat(bytes / 2))?;
            format!("cat '{}'", file.display())
        }
        Workload::YesHead => format!("yes | head -c {bytes}"),
        Workload::Mixed => {
            let file = dir.join("mixed.txt");
            let mut text = mixed_text(bytes);
            text.truncate(bytes);
            std::fs::write(&file, text)?;
            format!("cat '{}'", file.display())
        }
    };
    Ok(format!("{flood}; printf '\\033[0m\\n{DONE_MARKER}\\n'"))
}

fn session_run(script: &str, bytes: usize) -> Result<Sample, String> {
    let chunks = Arc::new(AtomicU64::new(0));
    let tap_chunks = Arc::clone(&chunks);
    let config = SessionConfig {
        program: Some(PathBuf::from("/bin/sh")),
        args: vec!["-c".into(), script.into()],
        cols: COLS,
        rows: ROWS,
        env: vec![("LC_ALL".into(), "C".into())],
        output_tap: Some(Box::new(move |_chunk: &[u8]| {
            tap_chunks.fetch_add(1, Ordering::Relaxed);
        })),
        ..SessionConfig::default()
    };
    // Thread handles are reused, so start with no session threads alive:
    // then every Halyard thread seen during this run belongs to this run.
    let quiet = Instant::now() + Duration::from_secs(5);
    while threads::cpu_by_thread().keys().any(|(_, name)| name.starts_with("halyard-"))
        && Instant::now() < quiet
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = threads::cpu_by_thread();
    let cpu0 = process_cpu();
    let start = Instant::now();
    let session = Session::spawn(config).map_err(|e| e.to_string())?;
    let mut frames = 0u64;
    let mut seen = before.clone();
    let deadline = start + Duration::from_secs(600);
    let wall = loop {
        match session.events().recv_timeout(Duration::from_millis(50)) {
            Ok(SessionEvent::FrameReady) => frames += 1,
            Ok(SessionEvent::Exited(_)) => break start.elapsed(),
            Ok(_) | Err(_) => {}
        }
        // Sample per-thread CPU while it runs: the reader thread is gone by
        // the time the program has exited.
        for (k, v) in threads::cpu_by_thread() {
            seen.insert(k, v);
        }
        if Instant::now() > deadline {
            return Err("no exit within 600 s".into());
        }
    };
    for (k, v) in threads::cpu_by_thread() {
        seen.insert(k, v);
    }
    let process_cpu = process_cpu().saturating_sub(cpu0);
    let text = session.latest_frame().text();
    drop(session);
    if !text.lines().any(|l| l == DONE_MARKER) {
        let tail: Vec<&str> = text.lines().rev().take(5).collect();
        return Err(format!("the end marker is not on the final frame; last rows: {tail:?}"));
    }
    let cpu_of = |prefix: &str| {
        seen.iter()
            .filter(|((_, name), _)| name.starts_with(prefix))
            .map(|(k, v)| v.saturating_sub(before.get(k).copied().unwrap_or_default()))
            .sum::<Duration>()
    };
    Ok(Sample {
        mbps: mb(bytes) / wall.as_secs_f64(),
        wall,
        session_cpu: cpu_of("halyard-session"),
        reader_cpu: cpu_of("halyard-pty-rea"),
        process_cpu,
        frames,
        chunks: chunks.load(Ordering::Relaxed),
    })
}

/// Time from writing one byte to `cat` (the terminal driver echoes it at once)
/// until a published frame shows it, `samples` times, 10 ms apart so frame
/// pacing (one frame per 8 ms while output streams) is not measured.
fn echo_latency(samples: usize) -> Result<Vec<Duration>, String> {
    let config = SessionConfig {
        program: Some(PathBuf::from("/bin/cat")),
        cols: COLS,
        rows: ROWS,
        ..SessionConfig::default()
    };
    let session = Session::spawn(config).map_err(|e| e.to_string())?;
    let shown = |s: &Session| s.latest_frame().text().matches('x').count();
    let mut out = Vec::with_capacity(samples);
    for i in 1..=samples {
        std::thread::sleep(Duration::from_millis(10));
        while session.events().try_recv().is_ok() {}
        let start = Instant::now();
        session.write(b"x".to_vec());
        while shown(&session) < i {
            if start.elapsed() > Duration::from_secs(2) {
                return Err(format!("echo {i} did not reach the screen"));
            }
            session.events().recv_timeout(Duration::from_millis(100)).ok();
        }
        out.push(start.elapsed());
    }
    Ok(out)
}

/// User plus system CPU time of this whole process (every thread).
fn process_cpu() -> Duration {
    // SAFETY: rusage is plain old data and getrusage fills it in.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `ru` is valid, writable storage for one rusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut ru) } != 0 {
        return Duration::ZERO;
    }
    let tv = |t: libc::timeval| {
        Duration::from_secs(u64::try_from(t.tv_sec).unwrap_or(0))
            + Duration::from_micros(u64::try_from(t.tv_usec).unwrap_or(0))
    };
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// CPU time per live thread of this process, keyed by (thread id, name).
mod threads {
    use std::collections::HashMap;
    use std::time::Duration;

    pub type Key = (u64, String);

    #[cfg(target_os = "macos")]
    pub fn cpu_by_thread() -> HashMap<Key, Duration> {
        // Not exported by the libc crate; from <sys/proc_info.h>.
        const PROC_PIDLISTTHREADS: libc::c_int = 6;
        let mut out = HashMap::new();
        let pid = std::process::id() as libc::c_int;
        let mut ids = vec![0u64; 512];
        let size = libc::c_int::try_from(ids.len() * 8).unwrap_or(0);
        // SAFETY: `ids` is writable storage of `size` bytes.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDLISTTHREADS, 0, ids.as_mut_ptr().cast(), size)
        };
        let count = usize::try_from(n).unwrap_or(0) / 8;
        for &id in &ids[..count.min(ids.len())] {
            // SAFETY: proc_threadinfo is plain old data.
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            let want =
                libc::c_int::try_from(std::mem::size_of::<libc::proc_threadinfo>()).unwrap_or(0);
            // SAFETY: `info` is writable storage of `want` bytes.
            let got = unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDTHREADINFO, id, (&raw mut info).cast(), want)
            };
            if got != want {
                continue;
            }
            let name: Vec<u8> = info
                .pth_name
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c.to_ne_bytes()[0])
                .collect();
            let cpu = Duration::from_nanos(info.pth_user_time + info.pth_system_time);
            out.insert((id, String::from_utf8_lossy(&name).into_owned()), cpu);
        }
        out
    }

    #[cfg(target_os = "linux")]
    pub fn cpu_by_thread() -> HashMap<Key, Duration> {
        let mut out = HashMap::new();
        // SAFETY: sysconf has no preconditions.
        let tick = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100);
        let Ok(dir) = std::fs::read_dir("/proc/self/task") else { return out };
        for entry in dir.flatten() {
            let Ok(tid) = entry.file_name().to_string_lossy().parse::<u64>() else { continue };
            let p = entry.path();
            let (Ok(comm), Ok(stat)) =
                (std::fs::read_to_string(p.join("comm")), std::fs::read_to_string(p.join("stat")))
            else {
                continue;
            };
            // Fields after the parenthesized command name; utime and stime
            // are fields 14 and 15 of the whole line.
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else { continue };
            let f: Vec<&str> = rest.split_whitespace().collect();
            let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let total = ticks(11) + ticks(12);
            out.insert(
                (tid, comm.trim_end().to_owned()),
                Duration::from_nanos(total * 1_000_000_000 / tick.max(1)),
            );
        }
        out
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn cpu_by_thread() -> HashMap<Key, Duration> {
        HashMap::new()
    }
}
