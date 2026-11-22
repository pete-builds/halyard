use super::*;

fn scan(data: &[u8]) -> Vec<ShellEvent> {
    Scanner::new().feed_collect(data)
}

fn one(data: &[u8]) -> ShellEvent {
    let evs = scan(data);
    assert_eq!(evs.len(), 1, "expected one event from {data:?}, got {evs:?}");
    evs.into_iter().next().unwrap()
}

/// Feeds `data` split into chunks at the given cut points.
fn scan_split(data: &[u8], cuts: &[usize], max: usize) -> Vec<ShellEvent> {
    let mut s = Scanner::with_max_osc_len(max);
    let mut out = Vec::new();
    let mut start = 0;
    for &c in cuts.iter().chain(std::iter::once(&data.len())) {
        s.feed(&data[start..c], |_, e| out.push(e));
        start = c;
    }
    out
}

fn pa(aid: Option<&str>) -> ShellEvent {
    ShellEvent::PromptStart { aid: aid.map(str::to_string) }
}

fn fin(code: Option<i32>) -> ShellEvent {
    ShellEvent::CommandFinished { exit_code: code, aid: None }
}

/// A corpus with every kind of sequence, mixed terminators, text and UTF-8.
fn corpus() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"hello \xc3\xa9t\xc3\xa9\x07");
    v.extend_from_slice(b"\x1b]7;file://box/home/p%20q\x1b\\");
    v.extend_from_slice(b"\x1b]133;A;aid=42\x07$ \x1b]133;B\x07");
    v.extend_from_slice(b"\x1b]633;E;ls\\x3b echo \\\\x;nonce\x07");
    v.extend_from_slice(b"\x1b]133;C\x1b\\out\r\n\x1b[31mred\x1b[0m\r\n");
    v.extend_from_slice(b"\x1b[?1049hfull\x07screen\x1b[?1049l");
    v.extend_from_slice(b"\x1bP+q544e\x07\x1b\\");
    v.extend_from_slice(b"\x1b_Gf=1;AAAA\x07\x1b\\");
    v.extend_from_slice(b"\x1b]133;D;1;aid=42\x07");
    v.extend_from_slice(b"\x1b]9;build done\x07\x1b]9;4;1;50\x07");
    v.extend_from_slice(b"\x1b]777;notify;Claude;needs you\x1b\\");
    v.extend_from_slice(b"\x1b]7717;state=waiting;agent=claude;msg=a%3Bb\x07");
    v.extend_from_slice(b"\x1b]2;win \xe2\x9c\x93\x07\x1b(B\x1b]8;;http://x\x07link\x1b]8;;\x07");
    v.extend_from_slice(b"\x1b]0;aborted by esc\x1b[1mbold\x07");
    v
}

fn corpus_events() -> Vec<ShellEvent> {
    vec![
        ShellEvent::Bell,
        ShellEvent::Cwd { host: Some("box".into()), path: "/home/p q".into() },
        pa(Some("42")),
        ShellEvent::CommandInputStart,
        ShellEvent::CommandLine("ls; echo \\x".into()),
        ShellEvent::CommandExecuted,
        ShellEvent::AltScreen { active: true },
        ShellEvent::Bell,
        ShellEvent::AltScreen { active: false },
        ShellEvent::CommandFinished { exit_code: Some(1), aid: Some("42".into()) },
        ShellEvent::Notification {
            source: NotificationSource::Osc9,
            title: None,
            body: "build done".into(),
        },
        ShellEvent::Progress { state: ProgressState::Normal, percent: Some(50) },
        ShellEvent::Notification {
            source: NotificationSource::Osc777,
            title: Some("Claude".into()),
            body: "needs you".into(),
        },
        ShellEvent::Agent(AgentReport {
            state: Some(ReportedState::Waiting),
            agent: Some("claude".into()),
            msg: Some("a;b".into()),
            ..Default::default()
        }),
        ShellEvent::Title { kind: TitleKind::Window, text: "win ✓".into() },
        // The OSC 0 was aborted by ESC [; the trailing BEL after "bold" rings.
        ShellEvent::Bell,
    ]
}

#[test]
fn corpus_whole() {
    assert_eq!(scan(&corpus()), corpus_events());
}

#[test]
fn corpus_every_single_split_point() {
    let data = corpus();
    for cut in 0..=data.len() {
        assert_eq!(
            scan_split(&data, &[cut], DEFAULT_MAX_OSC_LEN),
            corpus_events(),
            "split at {cut}"
        );
    }
}

#[test]
fn corpus_every_pair_of_split_points() {
    let data = corpus();
    for a in 0..=data.len() {
        for b in a..=data.len() {
            assert_eq!(
                scan_split(&data, &[a, b], DEFAULT_MAX_OSC_LEN),
                corpus_events(),
                "split at {a},{b}"
            );
        }
    }
}

#[test]
fn corpus_byte_at_a_time() {
    let data = corpus();
    let cuts: Vec<usize> = (1..data.len()).collect();
    assert_eq!(scan_split(&data, &cuts, DEFAULT_MAX_OSC_LEN), corpus_events());
}

#[test]
fn offsets_point_past_the_completing_byte() {
    let data = corpus();
    let mut s = Scanner::new();
    let mut offs = Vec::new();
    s.feed(&data, |off, ev| offs.push((off, ev)));
    assert_eq!(offs.len(), corpus_events().len());
    for (off, ev) in offs {
        let last = data[off - 1];
        match ev {
            ShellEvent::Bell => assert_eq!(last, BEL),
            ShellEvent::AltScreen { .. } => assert!(last == b'h' || last == b'l'),
            _ => assert!(last == BEL || last == b'\\', "{ev:?} ended at {last:#x}"),
        }
    }
    // Offsets are relative to the chunk in which the event completed.
    let mut s = Scanner::new();
    let mut seen = Vec::new();
    s.feed(b"xx\x1b]133;", |o, e| seen.push((o, e)));
    s.feed(b"A\x07yy", |o, e| seen.push((o, e)));
    assert_eq!(seen, vec![(2, pa(None))]);
}

#[test]
fn osc_133_marks() {
    assert_eq!(one(b"\x1b]133;A\x07"), pa(None));
    assert_eq!(one(b"\x1b]133;A;k=s;aid=abc\x1b\\"), pa(Some("abc")));
    assert_eq!(one(b"\x1b]133;B\x07"), ShellEvent::CommandInputStart);
    assert_eq!(one(b"\x1b]133;C;x=y\x07"), ShellEvent::CommandExecuted);
    assert_eq!(one(b"\x1b]133;D\x07"), fin(None));
    assert_eq!(one(b"\x1b]133;D;0\x07"), fin(Some(0)));
    assert_eq!(one(b"\x1b]133;D;130\x1b\\"), fin(Some(130)));
    assert_eq!(one(b"\x1b]133;D;-1\x07"), fin(Some(-1)));
    assert_eq!(one(b"\x1b]133;D;nope\x07"), fin(None));
    assert_eq!(one(b"\x1b]133;D;99999999999999\x07"), fin(None));
    assert_eq!(
        one(b"\x1b]133;D;aid=7\x07"),
        ShellEvent::CommandFinished { exit_code: None, aid: Some("7".into()) }
    );
    assert_eq!(
        one(b"\x1b]133;D;2;aid=7;err=x\x07"),
        ShellEvent::CommandFinished { exit_code: Some(2), aid: Some("7".into()) }
    );
    // Unknown and future letters are ignored, not misread.
    assert!(scan(b"\x1b]133;P;k=i\x07\x1b]133;L\x07\x1b]133;\x07\x1b]133\x07").is_empty());
    // 133;E is a VS Code extension only.
    assert!(scan(b"\x1b]133;E;ls\x07").is_empty());
}

#[test]
fn osc_133_kitty_cmdline_url() {
    assert_eq!(
        scan(b"\x1b]133;C;cmdline_url=git%20log%20-1\x07"),
        vec![ShellEvent::CommandLine("git log -1".into()), ShellEvent::CommandExecuted]
    );
}

#[test]
fn osc_633_marks_and_escaping() {
    assert_eq!(one(b"\x1b]633;A\x07"), pa(None));
    assert_eq!(one(b"\x1b]633;B\x07"), ShellEvent::CommandInputStart);
    assert_eq!(one(b"\x1b]633;C\x07"), ShellEvent::CommandExecuted);
    assert_eq!(one(b"\x1b]633;D;3\x07"), fin(Some(3)));
    assert_eq!(
        one(b"\x1b]633;E;echo a\\x3bb \\\\ c\\x0ad\\q\\x4\x07"),
        // \x0a (newline) is decoded then stripped as a control character.
        ShellEvent::CommandLine("echo a;b \\ cd\\q\\x4".into())
    );
    assert_eq!(
        one(b"\x1b]633;E;printf \\xc3\\xa9;abc123\x07"),
        ShellEvent::CommandLine("printf é".into())
    );
    assert_eq!(one(b"\x1b]633;E\x07"), ShellEvent::CommandLine(String::new()));
    assert_eq!(
        one(b"\x1b]633;P;Cwd=/srv/a\\x3bb\x07"),
        ShellEvent::Cwd { host: None, path: "/srv/a;b".into() }
    );
    assert!(scan(b"\x1b]633;P;Cwd=relative\x07\x1b]633;P;Other=1\x07").is_empty());
}

#[test]
fn osc_7_cwd() {
    assert_eq!(
        one(b"\x1b]7;file://host.local/Users/me/My%20Dir/%C3%A9\x07"),
        ShellEvent::Cwd { host: Some("host.local".into()), path: "/Users/me/My Dir/é".into() }
    );
    assert_eq!(
        one(b"\x1b]7;FILE:///tmp\x1b\\"),
        ShellEvent::Cwd { host: None, path: "/tmp".into() }
    );
    assert_eq!(
        one(b"\x1b]7;kitty-shell-cwd://h/a b\x07"),
        ShellEvent::Cwd { host: Some("h".into()), path: "/a b".into() }
    );
    // Encoded control characters are stripped after decoding.
    assert_eq!(
        one(b"\x1b]7;file://h/a%1b%5D0;x%07b\x07"),
        ShellEvent::Cwd { host: Some("h".into()), path: "/a]0;xb".into() }
    );
    assert!(scan(b"\x1b]7;http://h/x\x07\x1b]7;file://hostonly\x07\x1b]7;/plain\x07").is_empty());
}

#[test]
fn osc_9_notifications_and_progress() {
    assert_eq!(
        one(b"\x1b]9;Task finished\x07"),
        ShellEvent::Notification {
            source: NotificationSource::Osc9,
            title: None,
            body: "Task finished".into()
        }
    );
    // A message that merely starts with digits is still a message.
    assert_eq!(
        one(b"\x1b]9;10 files done\x07"),
        ShellEvent::Notification {
            source: NotificationSource::Osc9,
            title: None,
            body: "10 files done".into()
        }
    );
    assert_eq!(
        one(b"\x1b]9;4;3\x07"),
        ShellEvent::Progress { state: ProgressState::Indeterminate, percent: None }
    );
    assert_eq!(
        one(b"\x1b]9;4;2;250\x07"),
        ShellEvent::Progress { state: ProgressState::Error, percent: Some(100) }
    );
    assert_eq!(
        one(b"\x1b]9;4;0;0\x07"),
        ShellEvent::Progress { state: ProgressState::Remove, percent: Some(0) }
    );
    assert_eq!(
        one(b"\x1b]9;4;4;12\x07"),
        ShellEvent::Progress { state: ProgressState::Paused, percent: Some(12) }
    );
    // ConEmu subcommands other than 4 are not notifications, nor is a bad progress.
    assert!(
        scan(b"\x1b]9;1;500\x07\x1b]9;2;msgbox\x07\x1b]9;5\x07\x1b]9;12\x07\x1b]9;4;9\x07\x1b]9;4\x07\x1b]9;\x07")
            .is_empty()
    );
}

#[test]
fn osc_777_notify() {
    assert_eq!(
        one(b"\x1b]777;notify;Title;Body; with; semicolons\x07"),
        ShellEvent::Notification {
            source: NotificationSource::Osc777,
            title: Some("Title".into()),
            body: "Body; with; semicolons".into()
        }
    );
    assert_eq!(
        one(b"\x1b]777;notify;;just body\x07"),
        ShellEvent::Notification {
            source: NotificationSource::Osc777,
            title: None,
            body: "just body".into()
        }
    );
    assert!(scan(b"\x1b]777;notify\x07\x1b]777;notify;;\x07\x1b]777;preexec\x07").is_empty());
}

#[test]
fn halyard_protocol() {
    assert_eq!(
        one(b"\x1b]7717;state=done;exit=3;session=abc;agent=codex;v=1;unknown=z\x1b\\"),
        ShellEvent::Agent(AgentReport {
            state: Some(ReportedState::Done),
            agent: Some("codex".into()),
            msg: None,
            session: Some("abc".into()),
            exit_code: Some(3),
        })
    );
    for (v, s) in [
        ("working", ReportedState::Working),
        ("waiting", ReportedState::Waiting),
        ("idle", ReportedState::Idle),
        ("done", ReportedState::Done),
        ("error", ReportedState::Error),
    ] {
        let seq = format!("\x1b]7717;state={v}\x07");
        assert_eq!(
            one(seq.as_bytes()),
            ShellEvent::Agent(AgentReport { state: Some(s), ..Default::default() })
        );
    }
    // The OSC 777 carrier, for hosts that only pass notification sequences.
    assert_eq!(
        one(b"\x1b]777;halyard;state=working;msg=Running%20tests\x07"),
        ShellEvent::Agent(AgentReport {
            state: Some(ReportedState::Working),
            msg: Some("Running tests".into()),
            ..Default::default()
        })
    );
    // Unknown state values are dropped; a report with nothing known is not emitted.
    assert!(
        scan(b"\x1b]7717;state=pwned\x07\x1b]7717;\x07\x1b]7717;msg=\x07\x1b]7717;exit=x\x07")
            .is_empty()
    );
    assert_eq!(
        one(b"\x1b]7717;state=WAITING;msg=hi\x07"),
        ShellEvent::Agent(AgentReport { msg: Some("hi".into()), ..Default::default() })
    );
    // A raw `;` ends the value: the rest is a separate (here unknown) field.
    assert_eq!(
        one(b"\x1b]7717;msg=a;b c\x07"),
        ShellEvent::Agent(AgentReport { msg: Some("a".into()), ..Default::default() })
    );
    // Encoded escape sequences in values are stripped, and values are capped.
    let long = "x".repeat(5000);
    let seq = format!("\x1b]7717;msg=%1b]0%3Bevil%07ok{long}\x07");
    match one(seq.as_bytes()) {
        ShellEvent::Agent(r) => {
            let msg = r.msg.unwrap();
            assert!(msg.starts_with("]0;evilok"), "{msg}");
            assert_eq!(msg.chars().count(), limits::AGENT_MESSAGE);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn titles() {
    assert_eq!(
        one(b"\x1b]0;both\x07"),
        ShellEvent::Title { kind: TitleKind::Both, text: "both".into() }
    );
    assert_eq!(
        one(b"\x1b]1;icon\x1b\\"),
        ShellEvent::Title { kind: TitleKind::Icon, text: "icon".into() }
    );
    assert_eq!(
        one(b"\x1b]2;\x07"),
        ShellEvent::Title { kind: TitleKind::Window, text: String::new() }
    );
    // C0 controls inside an OSC are ignored by the parser, C1 and bidi by sanitizing.
    assert_eq!(
        one("\x1b]2;a\x01b\u{9b}c\u{202e}d\x07".as_bytes()),
        ShellEvent::Title { kind: TitleKind::Window, text: "abcd".into() }
    );
    let long = format!("\x1b]2;{}\x07", "t".repeat(1000));
    match one(long.as_bytes()) {
        ShellEvent::Title { text, .. } => assert_eq!(text.len(), limits::TITLE),
        other => panic!("{other:?}"),
    }
}

#[test]
fn bell_rules() {
    assert_eq!(scan(b"a\x07b\x07"), vec![ShellEvent::Bell, ShellEvent::Bell]);
    // BEL terminating an OSC is not a bell.
    assert_eq!(scan(b"\x1b]8;;x\x07"), vec![]);
    // BEL inside DCS, SOS, PM, APC strings is not a bell.
    assert_eq!(scan(b"\x1bPq\x07\x1b\\\x1bXs\x07\x1b\\\x1b^p\x07\x1b\\\x1b_a\x07\x1b\\"), vec![]);
    // A BEL executed in the middle of CSI or ESC sequences rings, as in a VT parser.
    assert_eq!(scan(b"\x1b[3\x07m"), vec![ShellEvent::Bell]);
    assert_eq!(scan(b"\x1b\x07"), vec![ShellEvent::Bell]);
    assert_eq!(scan(b"\x1b(\x07B"), vec![ShellEvent::Bell]);
    // After a CAN-aborted string the next BEL is in ground state again.
    assert_eq!(scan(b"\x1b_junk\x18\x07"), vec![ShellEvent::Bell]);
}

#[test]
fn osc_aborts() {
    // ESC followed by something other than backslash aborts the OSC.
    assert_eq!(scan(b"\x1b]133;A\x1b]133;B\x07"), vec![ShellEvent::CommandInputStart]);
    // CAN and SUB abort it.
    assert_eq!(scan(b"\x1b]133;A\x18\x1b]133;B\x07"), vec![ShellEvent::CommandInputStart]);
    assert_eq!(scan(b"\x1b]133;A\x1a\x07"), vec![ShellEvent::Bell]);
    // A string terminator aborted by ESC ] starts a new OSC.
    assert_eq!(scan(b"\x1bPdcs\x1b]133;B\x07"), vec![ShellEvent::CommandInputStart]);
    // Non numeric or empty OSC numbers are ignored.
    assert!(scan(b"\x1b]x;y\x07\x1b];\x07\x1b]\x07\x1b]1234567;a\x07").is_empty());
}

#[test]
fn c1_bytes_in_text_are_not_controls() {
    // 0x9d is the 8-bit OSC, but in a UTF-8 stream it is a continuation byte.
    assert_eq!(scan("\u{75d}\u{9d}133;A\x07".as_bytes()), vec![ShellEvent::Bell]);
}

#[test]
fn alt_screen_modes() {
    assert_eq!(
        scan(b"\x1b[?1049h\x1b[?1047l\x1b[?47h\x1b[?1;1049l"),
        vec![
            ShellEvent::AltScreen { active: true },
            ShellEvent::AltScreen { active: false },
            ShellEvent::AltScreen { active: true },
            ShellEvent::AltScreen { active: false },
        ]
    );
    // Not private, other modes, other finals, oversized parameter strings.
    let long = format!("\x1b[?{}1049h", "1;".repeat(40));
    assert!(scan(b"\x1b[1049h\x1b[?25h\x1b[?10490h\x1b[?1049m").is_empty());
    assert!(scan(long.as_bytes()).is_empty());
    // After an oversized CSI the scanner is back in ground state.
    let mut s = Scanner::new();
    assert!(s.feed_collect(long.as_bytes()).is_empty());
    assert!(s.is_ground());
}

#[test]
fn overflow_drops_and_resyncs() {
    let max = 16;
    let mut data = Vec::new();
    data.extend_from_slice(b"\x1b]9;");
    data.extend_from_slice(&[b'x'; 100]);
    data.extend_from_slice(b"\x07");
    data.extend_from_slice(b"\x1b]133;A\x07");
    // ST terminated overflow followed by a bell.
    data.extend_from_slice(b"\x1b]2;");
    data.extend_from_slice(&[b'y'; 100]);
    data.extend_from_slice(b"\x1b\\\x07");
    // Overflow aborted by ESC [ then a valid mark.
    data.extend_from_slice(b"\x1b]2;");
    data.extend_from_slice(&[b'z'; 100]);
    data.extend_from_slice(b"\x1b[m\x1b]133;B\x07");
    let expected = vec![pa(None), ShellEvent::Bell, ShellEvent::CommandInputStart];
    for cut in 0..=data.len() {
        assert_eq!(scan_split(&data, &[cut], max), expected, "split at {cut}");
    }
    // Exactly at the cap is accepted, one byte more is dropped.
    let exact = b"\x1b]9;123456789012\x07"; // payload "9;123456789012" is 14 bytes
    assert_eq!(scan_split(exact, &[], 14).len(), 1);
    assert_eq!(scan_split(exact, &[], 13).len(), 0);
}

#[test]
fn scanner_stays_bounded_on_endless_osc() {
    let mut s = Scanner::with_max_osc_len(64);
    let chunk = [b'a'; 4096];
    s.feed(b"\x1b]2;", |_, _| {});
    for _ in 0..100 {
        s.feed(&chunk, |_, _| panic!("no event expected"));
    }
    assert!(s.osc.capacity() <= 4096);
    assert_eq!(s.feed_collect(b"\x07\x07"), vec![ShellEvent::Bell]);
}
