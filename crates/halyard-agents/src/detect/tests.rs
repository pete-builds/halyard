use super::*;

fn info(exe: Option<&str>, argv: &[&str]) -> ProcessInfo {
    ProcessInfo {
        pid: 1,
        exe: exe.map(PathBuf::from),
        argv: argv.iter().map(|s| s.to_string()).collect(),
    }
}

fn kind(exe: Option<&str>, argv: &[&str]) -> Option<AgentKind> {
    detect_agent(&info(exe, argv))
}

#[test]
fn table() {
    use AgentKind::*;
    let cases: &[(Option<&str>, &[&str], Option<AgentKind>)] = &[
        // Plain program names.
        (None, &["claude"], Some(ClaudeCode)),
        (None, &["/opt/homebrew/bin/claude", "--resume"], Some(ClaudeCode)),
        (None, &["codex"], Some(Codex)),
        (None, &["opencode"], Some(OpenCode)),
        (None, &["gemini", "-p", "x"], Some(GeminiCli)),
        (None, &["aider", "--model", "x"], Some(Aider)),
        (None, &["goose", "session"], Some(Goose)),
        (None, &["goose"], Some(Goose)),
        (None, &["amp"], Some(Amp)),
        (None, &["-claude"], Some(ClaudeCode)),
        (None, &["CLAUDE.EXE"], Some(ClaudeCode)),
        // Node, Bun, Deno, Python wrapped invocations.
        (Some("/usr/bin/node"), &["node", "/usr/local/bin/claude"], Some(ClaudeCode)),
        (Some("/usr/bin/node"), &["node", "--no-warnings", "/x/bin/gemini"], Some(GeminiCli)),
        (Some("/usr/bin/node"), &["node", "/x/bin/codex.js"], Some(Codex)),
        (
            Some("/usr/bin/node"),
            &["node", "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"],
            Some(ClaudeCode),
        ),
        (
            Some("/usr/bin/node"),
            &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"],
            Some(Codex),
        ),
        (None, &["npx", "-y", "@google/gemini-cli"], Some(GeminiCli)),
        (None, &["npx", "@anthropic-ai/claude-code@latest"], Some(ClaudeCode)),
        (None, &["bunx", "opencode-ai"], Some(OpenCode)),
        (None, &["bun", "run", "/x/amp.ts"], Some(Amp)),
        (None, &["deno", "run", "-A", "/x/claude.ts"], Some(ClaudeCode)),
        (Some("/usr/bin/python3.12"), &["python3.12", "/home/u/.local/bin/aider"], Some(Aider)),
        (None, &["python", "-m", "aider"], Some(Aider)),
        (
            None,
            &["/usr/bin/python3", "/venv/lib/python3.12/site-packages/aider-chat/x.py"],
            Some(Aider),
        ),
        (None, &["uvx", "aider-chat"], Some(Aider)),
        // The executable path wins when argv is unreadable.
        (Some("/usr/local/bin/opencode"), &[], Some(OpenCode)),
        // Codex's native binary behind the npm wrapper.
        (None, &["/x/codex-aarch64-apple-darwin"], Some(Codex)),
        (None, &["codex-x86_64-unknown-linux-musl"], Some(Codex)),
        // Claude Code's native installer: version-named binary, argv[0] `claude`.
        (Some("/Users/u/.local/share/claude/versions/2.1.0"), &["claude"], Some(ClaudeCode)),
        (Some("/Users/u/.local/share/claude/versions/2.1.0"), &["2.1.0"], Some(ClaudeCode)),
        // Not agents.
        (Some("/bin/zsh"), &["-zsh"], None),
        (Some("/usr/bin/vim"), &["vim", "claude.md"], None),
        (Some("/usr/bin/node"), &["node", "server.js"], None),
        (Some("/usr/bin/node"), &["node"], None),
        (Some("/usr/bin/node"), &["node", "-e", "1"], None),
        (None, &["python3", "-c", "print(1)"], None),
        (None, &["codex-helper"], None),
        (None, &["claudette"], None),
        (None, &["grep", "claude"], None),
        (None, &["git", "commit", "-m", "@openai/codex"], None),
        (None, &[], None),
        // pressly/goose, the migration tool, is not Block's agent.
        (None, &["goose", "postgres", "dbstring", "up"], None),
        (None, &["goose", "status"], None),
    ];
    for (exe, argv, want) in cases {
        assert_eq!(&kind(*exe, argv), want, "exe {exe:?} argv {argv:?}");
    }
}

#[test]
fn python_versions_are_launchers() {
    assert!(is_launcher("python"));
    assert!(is_launcher("python3"));
    assert!(is_launcher("python3.13"));
    assert!(!is_launcher("pythonista"));
    assert!(!is_launcher("bash"));
}

#[test]
fn reported_names() {
    use AgentKind::*;
    assert_eq!(AgentKind::from_reported_name("claude"), ClaudeCode);
    assert_eq!(AgentKind::from_reported_name(" Claude-Code "), ClaudeCode);
    assert_eq!(AgentKind::from_reported_name("codex"), Codex);
    assert_eq!(AgentKind::from_reported_name("gemini-cli"), GeminiCli);
    assert_eq!(AgentKind::from_reported_name("Aider"), Aider);
    assert_eq!(AgentKind::from_reported_name("goose"), Goose);
    assert_eq!(AgentKind::from_reported_name("amp"), Amp);
    assert_eq!(AgentKind::from_reported_name("opencode"), OpenCode);
    assert_eq!(AgentKind::from_reported_name("my-bot\u{202e}"), Other("my-bot".into()));
    let long = "x".repeat(500);
    match AgentKind::from_reported_name(&long) {
        Other(n) => assert_eq!(n.len(), limits::AGENT_NAME),
        k => panic!("{k:?}"),
    }
}

#[test]
fn display_names() {
    assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
    assert_eq!(AgentKind::GeminiCli.display_name(), "Gemini CLI");
    assert_eq!(AgentKind::Other("bot".into()).display_name(), "bot");
}
