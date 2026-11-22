//! Validate Halyard config and theme files with the real parser.
//!
//! Usage: `check_files [--config <path>] [--theme <path>]...`
//!
//! Exits 0 when every file loads without an error or a warning, 1 otherwise,
//! and 2 on bad usage. The Nix flake's `hm-module` check runs it on the files
//! the Home Manager module writes, so a module change that produces a file
//! Halyard would reject fails `nix flake check`.

use std::path::Path;
use std::process::ExitCode;

use halyard_config::{Config, Theme};

fn check_config(path: &str) -> bool {
    match Config::load(Path::new(path)) {
        Ok(loaded) if loaded.warnings.is_empty() => {
            eprintln!("ok: config {path}");
            true
        }
        Ok(loaded) => {
            for w in &loaded.warnings {
                eprintln!("{path}: warning: {w}");
            }
            false
        }
        Err(e) => {
            eprintln!("{path}: error: {e}");
            false
        }
    }
}

fn check_theme(path: &str) -> bool {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{path}: cannot read: {e}");
            return false;
        }
    };
    match Theme::from_toml(&text) {
        Ok(theme) => {
            eprintln!("ok: theme {path} ({})", theme.name);
            true
        }
        Err(e) => {
            eprintln!("{path}: error: {e}");
            false
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || !args.len().is_multiple_of(2) {
        eprintln!("usage: check_files [--config <path>] [--theme <path>]...");
        return ExitCode::from(2);
    }
    let mut all_ok = true;
    for pair in args.chunks(2) {
        let ok = match pair[0].as_str() {
            "--config" => check_config(&pair[1]),
            "--theme" => check_theme(&pair[1]),
            other => {
                eprintln!("unknown flag {other}");
                return ExitCode::from(2);
            }
        };
        all_ok &= ok;
    }
    if all_ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
