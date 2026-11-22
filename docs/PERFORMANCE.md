# Performance

Measured, never asserted (`docs/PRODUCT.md`). This file records what has been measured, how, and
what has not been measured yet. Numbers are from one Mac; rerun the benchmark before relying on
them anywhere else.

## Environment

| | |
|---|---|
| Machine | Apple M4 Pro, 12 cores (8 performance, 4 efficiency), 24 GB |
| OS | macOS 15.3.1 (24D70) |
| Toolchain | Rust 1.100.0, Zig 0.15.2 |
| Build | `--release` (workspace profile: thin LTO, `codegen-units = 1`) |
| libghostty-vt | built by Zig in `ReleaseFast`: the workspace sets `debug = false` for `libghostty-vt-sys`, so its build script does not pick `Debug` (see "Suspects ruled out") |
| Load | Other builds ran on the machine at the same time (load average 3 to 6 of 12 cores). Run to run spread is in the raw lines below |

## How to run

```sh
cargo run --release -p halyard-term --example throughput              # full: 3 runs of 100 MB each
cargo run --release -p halyard-term --example throughput -- --quick    # CI smoke test, about 5 s
cargo run --release -p halyard-term --example throughput -- --only yes-head --runs 5 --mb 50
```

The benchmark is `crates/halyard-term/examples/throughput.rs`. It needs no window and no GPU.

- **engine**: `Engine::write` on an in-memory buffer, in 64 KiB writes (the reader's buffer)
  and in 1 KiB writes (the most one macOS PTY read returns). Parse cost only: no PTY, no frames.
- **frame**: `Engine::frame` on a full 200x50 screen whose rows all changed: the cost of one
  published frame.
- **session**: a real `Session` (200x50) runs `/bin/sh -c '<workload>; printf ... bench-done'`
  and is timed from spawn to the `Exited` event. MB/s is payload bytes over that wall time.
  CPU time is read per thread (`proc_pidinfo` on macOS, `/proc/self/task` on Linux) for the
  session and reader threads, and with `getrusage` for the whole process. Every run checks that
  the marker printed after the flood is on the final frame, so a run that drops output or
  misses the last frame fails. A counting output tap records how many chunks the reader
  handed over.
- **echo**: `cat` in a session; write one byte and time until a published frame shows the
  echo, 150 times, 10 ms apart.

Workloads:

| Name | Program | Shape |
|------|---------|-------|
| `long-line` | `cat` of N bytes of `a` | One endless line that wraps |
| `yes` | `cat` of N/2 lines of `y` | Two-byte lines in large writes, as plain `yes` writes them |
| `yes-head` | `yes \| head -c N` | The same lines, but `head` writing to a terminal is line buffered: one 2-byte `write` per line |
| `mixed` | `cat` of a generated file | Log and compiler style lines of 2 to 26 words, SGR colors (16, 256, truecolor), bold, underline, inverse, some UTF-8 and CJK |

`head -c N /dev/zero | tr '\0' a` was the first choice for `long-line`, but macOS `tr` alone
runs at 26 MB/s (`head -c 100000000 /dev/zero | LC_ALL=C tr '\0' a > /dev/null` took 3.78 s),
so it measured `tr`. In the first baseline it gave 30 MB/s with Halyard's threads mostly idle.

`--quick` runs 5 MB per session workload and 20 MB per engine workload once, and exits non-zero
when an engine throughput is below 5 MB/s, a session throughput below 1 MB/s (0.25 MB/s for
`yes-head`, which is limited by the program's own writes), on macOS when `yes-head` arrives in
chunks averaging under 64 bytes, when an end marker is missing from the final frame, or when an
echo never arrives. CI runs it on macOS and Ubuntu. The chunk size check is the one that guards
the reader change, and it does not depend on how fast the machine is: a first version used a
CPU-per-MB ceiling, and a GitHub macOS runner came within 5% of it with the fix in place (0.19
s/MB against 0.2). The session floor was 5 MB/s until a GitHub Ubuntu runner gave `yes` 12.4
MB/s in one run and 4.5 MB/s in the next on the same code.
These are loose limits that catch a large regression, not a benchmark.

## Results

Median of 3 runs of 100 MB each. "Before" is `main` at `456cc80`; "after" is this change. Both
use the same benchmark binary source; only `crates/halyard-term/src/session.rs` differs.

### Session (real PTY)

| Workload | MB/s before | MB/s after | Process CPU before | Process CPU after | Chunks before (avg) | Chunks after (avg) |
|----------|------------:|-----------:|-------------------:|------------------:|--------------------:|-------------------:|
| long-line | 151.6 | 158.6 | 0.71 s | 0.68 s | 97,658 (1024 B) | 97,658 (1024 B) |
| yes | 60.7 | 61.3 | 2.58 s | 2.53 s | 147,319 (679 B) | 147,202 (679 B) |
| yes-head | **4.6** | **6.1** | **33.29 s** | **3.38 s** | 12,213,228 (8 B) | 390,750 (256 B) |
| mixed | 142.0 | 142.9 | 0.98 s | 0.96 s | 99,174 (1008 B) | 99,172 (1008 B) |

Raw lines (runs are MB/s per run):

```text
before
session/long-line median   151.6 MB/s  wall   0.66s  session cpu   0.30s  reader cpu  0.41s  process cpu   0.71s  frames    81  chunks   97658 (avg 1024 B)  runs 135.5 / 155.4 / 151.6
session/yes       median    60.7 MB/s  wall   1.65s  session cpu   1.64s  reader cpu  0.93s  process cpu   2.58s  frames   202  chunks  147319 (avg 679 B)  runs 60.7 / 58.3 / 63.1
session/yes-head  median     4.6 MB/s  wall  21.97s  session cpu  12.24s  reader cpu 21.02s  process cpu  33.29s  frames  2674  chunks 12213228 (avg 8 B)  runs 4.6 / 3.8 / 4.6
session/mixed     median   142.0 MB/s  wall   0.70s  session cpu   0.54s  reader cpu  0.42s  process cpu   0.98s  frames    86  chunks   99174 (avg 1008 B)  runs 141.7 / 142.0 / 144.5
after
session/long-line median   158.6 MB/s  wall   0.63s  session cpu   0.28s  reader cpu  0.39s  process cpu   0.68s  frames    78  chunks   97658 (avg 1024 B)  runs 158.6 / 160.5 / 143.0
session/yes       median    61.3 MB/s  wall   1.63s  session cpu   1.62s  reader cpu  0.90s  process cpu   2.53s  frames   200  chunks  147202 (avg 679 B)  runs 61.1 / 61.7 / 61.3
session/yes-head  median     6.1 MB/s  wall  16.29s  session cpu   2.01s  reader cpu  1.35s  process cpu   3.38s  frames  1984  chunks  390750 (avg 256 B)  runs 5.9 / 6.1 / 6.1
session/mixed     median   142.9 MB/s  wall   0.70s  session cpu   0.54s  reader cpu  0.42s  process cpu   0.96s  frames    86  chunks   99172 (avg 1008 B)  runs 142.0 / 143.1 / 142.9
```

`long-line`, `yes` and `mixed` did not change beyond run to run noise; this change targets
output that arrives in small pieces.

### Engine alone (unchanged by this change)

| Workload | 64 KiB writes | 1 KiB writes |
|----------|--------------:|-------------:|
| long-line | 1431 MB/s | 1417 MB/s |
| yes | 108 MB/s | 109 MB/s |
| mixed | 301 MB/s | 298 MB/s |

Write size does not matter to the engine. `yes` is slow because every line feed scrolls: once a
scrollback page (215 rows) is full, Ghostty recycles the oldest page and zeroes it with
`@memset` (`PageList.grow`), which was 30% of the session thread's samples on `yes`.

### Frame build (unchanged)

About 0.2 ms per 200x50 frame (184 to 234 us depending on content). Frames are published at
most every 8 ms, so at most about 2.5% of one core.

### Echo latency (unchanged)

Five alternating rounds of 150 samples each: median 1.36 ms before and 1.36 ms after (p95
1.46 to 1.52 ms both). The pause added by this change follows a read and never delays the
first bytes of a burst, and the numbers agree.

### In the real app

The `debug-script` build, a 200x50 config with `shell = { program = "/bin/sh" }`, typing
`/usr/bin/time -p sh -c 'yes | head -c 20000000'` into the pane. Wall time is the pipeline's
own `real` time; CPU is the app process's own CPU time from `ps` at exit (children excluded),
for the whole run including startup. Three alternating runs, median. An idle run of the same
script (typing `true` instead) used 0.23 s with either build.

| Build | Pipeline wall | MB/s | App CPU (whole run) |
|-------|--------------:|-----:|--------------------:|
| `main` at `a28c847` | 4.45 s (4.31, 4.45, 4.52) | 4.5 | 7.02 s (6.83, 7.02, 7.11) |
| this change merged onto it | 3.24 s (3.22, 3.24, 3.26) | 6.2 | 0.90 s (0.89, 0.90, 0.96) |

`a28c847` includes the per-pane signal observer (#22), which scans every chunk on the session
thread, so fewer chunks also means fewer scans. Before that merge the same comparison
(`456cc80` against this change) gave 4.37 s and 6.90 s against 3.44 s and 0.96 s.

This reproduces the original report (4.7 MB/s and about 32 s of Halyard CPU per 100 MB) and
matches the headless numbers. The window did not repaint during these runs (its render count
stayed at 3 because the window server was not drawing it), so the cost of drawing is not in
them.

### Reference points

Same machine, same day.

- `script -q /dev/null <cmd> > /dev/null`, a minimal C program that relays a PTY to a file,
  shows what the macOS PTY allows: `cat` of 500 MB of `a` took 2.96 s (169 MB/s), `cat` of
  500 MB of `y` lines 5.18 s (97 MB/s), and `yes | head -c 20000000` 4.54 s (4.4 MB/s).
  A macOS PTY read returns at most 1 KiB, and the program blocks once roughly that much is
  buffered, so every KiB costs a round trip between the program, the kernel and the reader.
- Terminal.app 2.14 (macOS 15.3.1), driven with `osascript` (`do script`), ran
  `LC_ALL=C /usr/bin/time -p sh -c 'yes | head -c 20000000'` in a new 80x24 window, which was
  then closed. Three runs: pipeline wall 13.88, 16.80 and 17.91 s (median 1.2 MB/s), Terminal's
  own CPU (from `ps`) grew by 14.56, 17.68 and 18.77 s. This is not a like-for-like comparison:
  the window size differs and it is not known whether Terminal.app's window drew while
  Halyard's did not. iTerm2, Ghostty, kitty, WezTerm and Alacritty are not installed here and
  were not run.

## What the profile showed

`sample <pid> 5` during a 40 MB `yes-head` run on `main`:

- The reader thread was on CPU for 8.18 s of 8.66 s wall. Of its samples, 62% were in `poll`,
  29% in `read`, the rest in the channel send and its wakeup. Each PTY read returned 8 bytes on
  average: `head` writes one 2-byte line per `write`, and the reader picked each up at once.
- The session thread spent its time in `select!` parking and waking for each 8-byte chunk.
- Nothing in the frame path, the engine or the UI wakeups was a top cost.

## What changed

`read_loop` (`crates/halyard-term/src/session.rs`), **macOS only**: after a read shorter than 512 bytes, the
reader hands the bytes over as before, then sleeps 50 us before it polls again, so output that
trickles in collects in the PTY. A burst's first bytes (a key echo) are never delayed; later
bytes wait at most 50 us. Large reads never pause. Pause lengths tried on `yes-head`, 20 MB,
one run each, pausing after reads under 1024 bytes:

| Pause | MB/s | Process CPU | Avg chunk |
|------:|-----:|------------:|----------:|
| none | 4.6 | 6.64 s | 8 B |
| 50 us | 6.1 | 0.63 s | 495 B |
| 100 us | 4.2 | 0.83 s | 352 B |
| 250 us | 1.8 | 1.63 s | 351 B |
| 1 ms | 0.5 | 5.11 s | 386 B |

A longer pause lets the PTY fill (it holds a few hundred bytes of small writes), and the program
then waits for the reader, so 50 us it is. The 1024-byte threshold also paused after many
`mixed` reads (1008 bytes on average) and cost that workload 13% (117.6 vs 135.6 MB/s); with
512 it measured 134.4 MB/s, and the 3-run table above shows no change.

Proof: `trickling_output_is_read_in_batches` (`crates/halyard-term/tests/session.rs`) fails on
`main` ("12 bytes per chunk (48596 chunks)") and passes with the change. `--quick` exits 1 on
`main` ("session/yes-head read 8 bytes per chunk, below 64") and 0 with the change.

**Linux: not measured, pause off.** The pause is compiled in only on macOS
(`PAUSE_AFTER_SMALL_READ`). On the GitHub Ubuntu runner, with the pause on, `cat` of `yes`
lines arrived in 275 to 784 byte chunks and measured 4.5 and 12.4 MB/s in two runs of the same
code, and there is no Linux run without the pause to compare. So it is not known whether the
pause helps, hurts or does nothing on Linux. A real Linux A/B on the NixOS box is pending; the
pause stays off on Linux until that is measured. `trickling_output_is_read_in_batches` and the
`--quick` chunk size check run on macOS only for the same reason.

## Suspects ruled out (measured)

- **Debug Zig build of libghostty-vt.** The release profile builds it `ReleaseFast`: the
  workspace's `[profile.*.package.libghostty-vt-sys] debug = false` keeps its build script from
  choosing `Debug`. Control: forcing `LIBGHOSTTY_VT_SYS_OPTIMIZE=Debug` (separate target
  directory) makes the engine parse `mixed` at 0.1 MB/s instead of about 300 MB/s and a frame
  take 1.7 ms instead of 0.23 ms (1 MB, one run), so a Debug build would stand out in these
  numbers by three orders of magnitude.
- **Per-chunk channel handoff.** Draining every queued chunk in one session-thread wakeup (up to
  1 MiB or 2 ms) changed nothing measurable: `yes` 60.3 vs 59.6 MB/s, `yes-head` 5.9 vs 5.9,
  `mixed` 139.1 vs 140.5 (three alternating 50 MB runs each). It was not kept.
- **Building a frame every 8 ms.** 0.2 ms per frame, at most about 2.5% of a core. Skipping
  frames the UI has not fetched yet would save little and was not done.
- **Per-chunk UI wakeups.** There are none: the UI is notified per published frame (at most
  every 8 ms), bell or title change, never per chunk.
- **The output tap.** The benchmark runs with a counting tap. The app's per-pane signal
  observer (#22) scans each chunk; it is included in the in-app numbers above but not in the
  headless benchmark.
- **Small reads in general.** Every macOS PTY read is at most 1 KiB; the engine parses 1 KiB
  writes as fast as 64 KiB ones (table above), and `long-line` reaches 90% of what `script(1)`
  gets through the same PTY.

## Not measured yet

- Startup time and memory (resident size per pane, scrollback growth).
- Input latency end to end (key press to pixels). Only the headless echo above is measured.
- GPU render time and frame pacing in a visible window.
- Many panes at once.
- Linux and NixOS, beyond CI smoke runs (A/B on the NixOS box pending, see above). On the
  `ubuntu-24.04` runner (5 MB, one run, with the pause still on there) `--quick` gave long-line 245.6, yes 12.4, yes-head 8.3 and mixed 127.0 MB/s,
  with Linux PTY reads averaging 3253 bytes on long-line, so the 1 KiB read limit is a macOS
  property. On the `macos-15` runner: 20.1, 7.9, 1.3 and 49.1 MB/s. Shared runners are slow
  and noisy; these show only that the floors hold there.
- Throughput with the shell integration scanner installed as the output tap.

## Found along the way, not changed here

`SessionConfig::scrollback_lines` (default 10,000, from `[terminal] scrollback_lines`) is passed
to libghostty-vt's `max_scrollback`, which the C header documents as lines but Ghostty's
`Screen.init` treats as **bytes** ("max_scrollback is the amount of scrollback to keep in
bytes"). Measured: an engine created with 10,000 keeps 247 rows at 200 columns and 1,089 rows at
80 columns after 20,000 lines of output; 10,000,000 keeps 5,483 rows at 200 columns. So panes
keep far less scrollback than configured. Fixing it changes memory use per pane and needs its
own decision and test.
