//! `halyard-msg`: report agent state and notifications to Halyard from hooks.
//!
//! Designed to be safe in any hook: outside Halyard it exits 0 at once and prints nothing,
//! it never runs longer than about three seconds, and it never exits with status 2
//! (Claude Code treats 2 from some hooks as "block the action").

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use halyard_ipc::text::{
    MAX_BODY_CHARS, MAX_MSG_CHARS, MAX_TITLE_CHARS, check_ident, sanitize_display,
};
use halyard_ipc::{AgentState, Client, ENV_SESSION_ID, ENV_SESSION_TOKEN, ENV_SOCKET, osc};
use serde_json::{Value, json};

const USAGE: &str = "\
usage:
  halyard-msg state <working|waiting|idle|done|error> [--agent NAME] [--msg TEXT] [--strict]
  halyard-msg notify <TITLE> [BODY] [--strict]
  halyard-msg info [--strict]
  halyard-msg --help | --version

Reads HALYARD_SOCKET, HALYARD_SESSION_ID and HALYARD_SESSION_TOKEN from the environment.
Outside Halyard it does nothing and exits 0. If the socket is unreachable, state and
notify fall back to an escape sequence written to /dev/tty.

Exit status: 0 on success or when not inside Halyard; 1 on failure with --strict
(without --strict failures print a warning and still exit 0); 64 on a usage error.";

const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 64;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
const WATCHDOG: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum Command {
    State { state: AgentState, agent: Option<String>, msg: Option<String> },
    Notify { title: String, body: String },
    Info,
    Help,
    Version,
}

fn parse_args(args: &[String]) -> Result<(Command, bool), String> {
    let mut strict = false;
    let mut agent = None;
    let mut msg = None;
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--strict" => strict = true,
            "--agent" => agent = Some(it.next().ok_or("--agent needs a value")?.clone()),
            "--msg" => msg = Some(it.next().ok_or("--msg needs a value")?.clone()),
            "-h" | "--help" => return Ok((Command::Help, strict)),
            "-V" | "--version" => return Ok((Command::Version, strict)),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => positional.push(other.to_owned()),
        }
    }
    let has_state_options = agent.is_some() || msg.is_some();
    let mut positional = positional.into_iter();
    let cmd = match positional.next().as_deref() {
        Some("state") => {
            let raw = positional.next().ok_or("state needs a value")?;
            let state = AgentState::parse(&raw).ok_or_else(|| format!("unknown state {raw:?}"))?;
            if let Some(agent) = &agent {
                check_ident("--agent", agent, 64)?;
            }
            let msg = msg.map(|m| sanitize_display(&m, MAX_MSG_CHARS));
            Command::State { state, agent, msg }
        }
        Some("notify") => {
            let title = positional.next().ok_or("notify needs a title")?;
            let title = sanitize_display(&title, MAX_TITLE_CHARS);
            if title.trim().is_empty() {
                return Err("the title must not be empty".into());
            }
            let body = sanitize_display(&positional.next().unwrap_or_default(), MAX_BODY_CHARS);
            Command::Notify { title, body }
        }
        Some("info") => Command::Info,
        Some(other) => return Err(format!("unknown command {other:?}")),
        None => return Err("missing command".into()),
    };
    if let Some(extra) = positional.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    if !matches!(cmd, Command::State { .. }) && has_state_options {
        return Err("--agent and --msg only apply to state".into());
    }
    Ok((cmd, strict))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn fail(strict: bool, message: &str) -> ExitCode {
    eprintln!("halyard-msg: {message}");
    if strict { ExitCode::from(EXIT_FAILURE) } else { ExitCode::SUCCESS }
}

fn call_socket(mut client: Client, cmd: &Command) -> Result<Value, String> {
    let session_id = env_nonempty(ENV_SESSION_ID).unwrap_or_default();
    let token = env_nonempty(ENV_SESSION_TOKEN).unwrap_or_default();
    client.hello("halyard-msg").map_err(|e| e.to_string())?;
    client
        .call("session.attach", json!({ "session_id": session_id, "token": token }))
        .map_err(|e| e.to_string())?;
    let (method, params) = match cmd {
        Command::State { state, agent, msg } => {
            ("session.report_state", json!({ "state": state, "agent": agent, "msg": msg }))
        }
        Command::Notify { title, body } => {
            ("session.notify", json!({ "title": title, "body": body }))
        }
        Command::Info => ("session.info", json!({})),
        Command::Help | Command::Version => return Ok(Value::Null),
    };
    client.call(method, params).map_err(|e| e.to_string())
}

fn write_tty(sequence: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut tty =
        std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NOCTTY).open("/dev/tty")?;
    tty.write_all(sequence.as_bytes())?;
    tty.flush()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, strict) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("halyard-msg: {e}\n\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match cmd {
        Command::Help => {
            // Best effort: a closed stdout is not worth failing a hook over.
            let _ = writeln!(std::io::stdout().lock(), "{USAGE}");
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            let _ = writeln!(std::io::stdout().lock(), "halyard-msg {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Hooks must never hang: whatever happens, stop after WATCHDOG.
    std::thread::spawn(move || {
        std::thread::sleep(WATCHDOG);
        eprintln!("halyard-msg: timed out");
        std::process::exit(if strict { i32::from(EXIT_FAILURE) } else { 0 });
    });

    if env_nonempty(ENV_SESSION_ID).is_none() {
        // Not inside Halyard: do nothing, quietly.
        return if strict { fail(true, "not inside a Halyard session") } else { ExitCode::SUCCESS };
    }

    let socket = env_nonempty(ENV_SOCKET).map(PathBuf::from);
    // An unreachable socket (app gone, stale env) falls through to the escape sequence.
    let client = socket.and_then(|s| Client::connect(&s, Some(SOCKET_TIMEOUT)).ok());
    if let Some(client) = client {
        return match call_socket(client, &cmd) {
            Ok(result) => {
                if matches!(cmd, Command::Info) {
                    let _ = writeln!(std::io::stdout().lock(), "{result}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(strict, &e),
        };
    }

    let sequence = match &cmd {
        Command::State { state, agent, msg } => {
            let session = env_nonempty(ENV_SESSION_ID);
            osc::state_sequence(*state, agent.as_deref(), msg.as_deref(), session.as_deref())
        }
        Command::Notify { title, body } => osc::notify_sequence(title, body),
        _ => return fail(strict, "the Halyard socket is not reachable"),
    };
    match write_tty(&sequence) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(strict, &format!("no socket and no terminal: {e}")),
    }
}
