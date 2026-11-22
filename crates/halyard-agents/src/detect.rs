//! Recognizing coding agents from process information.
//!
//! Detection looks at, in order:
//!
//! 1. the program name: the basename of `argv[0]`, then of the executable path;
//! 2. for interpreters and launchers (`node`, `bun`, `deno`, `python3`, `npx`, ...),
//!    the first non-option argument, which is the script or package being run, by
//!    its name or by an npm or PyPI package path in it (`@anthropic-ai/claude-code`,
//!    `@openai/codex`, `aider-chat`, ...);
//! 3. Claude Code's native installer layout (`.../claude/versions/<version>`).
//!
//! Arguments of programs that are not launchers are never searched, so
//! `grep claude` or `vim claude.md` is not an agent.
//!
//! Names are compared case-insensitively after removing a script extension
//! (`.js`, `.mjs`, `.cjs`, `.ts`, `.py`, `.exe`, `.cmd`).

use std::path::{Path, PathBuf};

use halyard_shell::sanitize::{limits, sanitize};

/// A coding agent Halyard knows by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Anthropic's Claude Code (`claude`).
    ClaudeCode,
    /// OpenAI's Codex CLI (`codex`).
    Codex,
    /// OpenCode (`opencode`).
    OpenCode,
    /// Google's Gemini CLI (`gemini`).
    GeminiCli,
    /// Aider (`aider`).
    Aider,
    /// Block's Goose (`goose`).
    Goose,
    /// Sourcegraph's Amp (`amp`).
    Amp,
    /// Any other agent, by the name it reported through the Halyard protocol.
    Other(String),
}

impl AgentKind {
    /// Human readable name.
    pub fn display_name(&self) -> &str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::GeminiCli => "Gemini CLI",
            AgentKind::Aider => "Aider",
            AgentKind::Goose => "Goose",
            AgentKind::Amp => "Amp",
            AgentKind::Other(name) => name,
        }
    }

    /// Maps the `agent=` field of a protocol report to a kind. Known names and
    /// aliases map to their variant; anything else becomes [`AgentKind::Other`]
    /// with the name sanitized and capped.
    pub fn from_reported_name(name: &str) -> AgentKind {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
            "claude" | "claude-code" | "claude code" | "claudecode" => AgentKind::ClaudeCode,
            "codex" | "codex-cli" => AgentKind::Codex,
            "opencode" => AgentKind::OpenCode,
            "gemini" | "gemini-cli" | "gemini cli" => AgentKind::GeminiCli,
            "aider" => AgentKind::Aider,
            "goose" => AgentKind::Goose,
            "amp" => AgentKind::Amp,
            _ => AgentKind::Other(sanitize(name.trim(), limits::AGENT_NAME)),
        }
    }
}

/// What the caller knows about a process. Strings are expected to be lossily
/// decoded already; [`crate::platform::process_info`] produces this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Process id.
    pub pid: i32,
    /// Resolved executable path, when readable.
    pub exe: Option<PathBuf>,
    /// Argument vector, `argv[0]` first. May be empty when unreadable.
    pub argv: Vec<String>,
}

/// Identifies the agent a process is running, if any.
pub fn detect_agent(info: &ProcessInfo) -> Option<AgentKind> {
    let argv0 = info.argv.first().map(String::as_str);
    let exe = info.exe.as_deref().and_then(Path::to_str);

    for prog in [argv0, exe].into_iter().flatten() {
        let name = stem(prog);
        if let Some(kind) = by_program_name(&name, &info.argv) {
            return Some(kind);
        }
        if is_launcher(&name)
            && let Some(kind) = by_launched_script(&info.argv)
        {
            return Some(kind);
        }
    }
    // Claude Code's native installer runs `~/.local/share/claude/versions/<v>`.
    if exe.is_some_and(|e| e.contains("/claude/versions/")) {
        return Some(AgentKind::ClaudeCode);
    }
    None
}

/// Lowercased basename without a script or binary extension.
fn stem(prog: &str) -> String {
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    let base = base.trim_start_matches('-');
    let lower = base.to_ascii_lowercase();
    for ext in [".js", ".mjs", ".cjs", ".ts", ".py", ".exe", ".cmd"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_string();
        }
    }
    lower
}

fn by_program_name(name: &str, argv: &[String]) -> Option<AgentKind> {
    Some(match name {
        "claude" => AgentKind::ClaudeCode,
        "codex" => AgentKind::Codex,
        "opencode" => AgentKind::OpenCode,
        "gemini" => AgentKind::GeminiCli,
        "aider" => AgentKind::Aider,
        "amp" => AgentKind::Amp,
        "goose" if !is_goose_migration_tool(argv) => AgentKind::Goose,
        // The npm `codex` wrapper runs a native binary named after the target triple.
        n if n.starts_with("codex-")
            && (n.contains("-apple-darwin")
                || n.contains("-linux-")
                || n.contains("-windows-")) =>
        {
            AgentKind::Codex
        }
        _ => return None,
    })
}

/// `goose` is also the name of the pressly/goose database migration tool, whose
/// first argument is a database driver or a migration command.
fn is_goose_migration_tool(argv: &[String]) -> bool {
    let Some(first) = argv.get(1) else {
        return false;
    };
    matches!(
        first.as_str(),
        "postgres"
            | "mysql"
            | "sqlite3"
            | "mssql"
            | "redshift"
            | "tidb"
            | "clickhouse"
            | "vertica"
            | "ydb"
            | "turso"
            | "starrocks"
            | "up"
            | "up-by-one"
            | "up-to"
            | "down"
            | "down-to"
            | "redo"
            | "reset"
            | "status"
            | "create"
            | "fix"
            | "validate"
    )
}

fn is_launcher(name: &str) -> bool {
    matches!(
        name,
        "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "npx"
            | "bunx"
            | "pnpx"
            | "tsx"
            | "uv"
            | "uvx"
            | "pipx"
    ) || name == "python"
        || name
            .strip_prefix("python")
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Looks at the first non-option argument after the launcher, skipping the `run`
/// subcommand of `deno`, `bun`, `uv` and `pipx`.
fn by_launched_script(argv: &[String]) -> Option<AgentKind> {
    let mut args = argv.iter().skip(1).filter(|a| !a.starts_with('-'));
    let mut first = args.next()?;
    if first == "run" {
        first = args.next()?;
    }
    by_program_name(&stem(first), &[]).or_else(|| by_package_path(first))
}

fn by_package_path(arg: &str) -> Option<AgentKind> {
    const PACKAGES: [(&str, AgentKind); 6] = [
        ("@anthropic-ai/claude-code", AgentKind::ClaudeCode),
        ("@openai/codex", AgentKind::Codex),
        ("@google/gemini-cli", AgentKind::GeminiCli),
        ("opencode-ai", AgentKind::OpenCode),
        ("@sourcegraph/amp", AgentKind::Amp),
        ("aider-chat", AgentKind::Aider),
    ];
    let a = arg.replace('\\', "/");
    PACKAGES.iter().find_map(|(pkg, kind)| {
        let hit = a == *pkg
            || a.starts_with(&format!("{pkg}@"))
            || a.contains(&format!("/{pkg}/"))
            || a.ends_with(&format!("/{pkg}"));
        hit.then(|| kind.clone())
    })
}

#[cfg(test)]
mod tests;
