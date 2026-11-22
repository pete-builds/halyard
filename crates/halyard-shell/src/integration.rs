//! Shell integration scripts for zsh, bash and fish.
//!
//! The scripts are embedded in the binary. Halyard never edits a user's rc file: it
//! writes the script somewhere the user chooses and [`install_line`] returns the one
//! line to add by hand. Each script is inert unless `TERM_PROGRAM` is `Halyard` and
//! the shell is interactive, and is safe to source twice.

use std::path::Path;

/// Value of `TERM_PROGRAM` that Halyard sets in every pane.
pub const TERM_PROGRAM: &str = "Halyard";

/// Environment variable holding the pane's session id (see `docs/protocol.md`).
pub const SESSION_ID_ENV: &str = "HALYARD_SESSION_ID";

const ZSH: &str = include_str!("../scripts/halyard.zsh");
const BASH: &str = include_str!("../scripts/halyard.bash");
const FISH: &str = include_str!("../scripts/halyard.fish");

/// A shell Halyard ships integration for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shell {
    /// zsh.
    Zsh,
    /// GNU bash 3.2 or later.
    Bash,
    /// fish 3.0 or later.
    Fish,
}

impl Shell {
    /// All supported shells.
    pub const ALL: [Shell; 3] = [Shell::Zsh, Shell::Bash, Shell::Fish];

    /// Recognizes a shell from its program path or name, including login shell
    /// names with a leading dash (`-zsh`).
    pub fn from_program(program: &str) -> Option<Shell> {
        let name = Path::new(program).file_name()?.to_str()?;
        match name.trim_start_matches('-') {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }

    /// Conventional file name for the script.
    pub fn script_file_name(self) -> &'static str {
        match self {
            Shell::Zsh => "halyard.zsh",
            Shell::Bash => "halyard.bash",
            Shell::Fish => "halyard.fish",
        }
    }

    /// The rc file the install line belongs in.
    pub fn rc_file(self) -> &'static str {
        match self {
            Shell::Zsh => "~/.zshrc",
            Shell::Bash => "~/.bashrc",
            Shell::Fish => "~/.config/fish/config.fish",
        }
    }
}

/// The integration script for `shell`.
pub fn integration_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Zsh => ZSH,
        Shell::Bash => BASH,
        Shell::Fish => FISH,
    }
}

/// The line a user adds to [`Shell::rc_file`] to load the script saved at
/// `script_path`. The line checks `TERM_PROGRAM` itself, so other terminals never
/// read the file.
pub fn install_line(shell: Shell, script_path: &Path) -> String {
    let p = script_path.to_string_lossy();
    match shell {
        Shell::Zsh | Shell::Bash => {
            format!("[[ \"$TERM_PROGRAM\" == {TERM_PROGRAM} ]] && source {}", posix_quote(&p))
        }
        Shell::Fish => {
            format!("test \"$TERM_PROGRAM\" = {TERM_PROGRAM}; and source {}", fish_quote(&p))
        }
    }
}

/// Single quotes for POSIX shells: `'` becomes `'\''`.
fn posix_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single quotes for fish, where `\` and `'` are escaped inside them.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_embedded_and_guarded() {
        for shell in Shell::ALL {
            let s = integration_script(shell);
            assert!(s.contains("TERM_PROGRAM"), "{shell:?}");
            assert!(s.contains("Halyard"), "{shell:?}");
            assert!(s.contains("133;A"), "{shell:?}");
            assert!(s.contains("133;D"), "{shell:?}");
            assert!(s.contains("633;E"), "{shell:?}");
            assert!(s.contains("]7;file://"), "{shell:?}");
            assert!(s.contains("_loaded"), "{shell:?}");
        }
    }

    #[test]
    fn shell_from_program() {
        assert_eq!(Shell::from_program("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_program("-zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_program("/opt/homebrew/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_program("fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_program("/bin/sh"), None);
        assert_eq!(Shell::from_program(""), None);
    }

    #[test]
    fn install_lines_quote_paths() {
        let p = Path::new("/home/o'neil/halyard dir/halyard.zsh");
        assert_eq!(
            install_line(Shell::Zsh, p),
            r#"[[ "$TERM_PROGRAM" == Halyard ]] && source '/home/o'\''neil/halyard dir/halyard.zsh'"#
        );
        assert_eq!(
            install_line(Shell::Bash, Path::new("/x/halyard.bash")),
            r#"[[ "$TERM_PROGRAM" == Halyard ]] && source '/x/halyard.bash'"#
        );
        assert_eq!(
            install_line(Shell::Fish, Path::new(r"/a\b/o'n.fish")),
            r#"test "$TERM_PROGRAM" = Halyard; and source '/a\\b/o\'n.fish'"#
        );
        assert_eq!(Shell::Fish.rc_file(), "~/.config/fish/config.fish");
        assert_eq!(Shell::Bash.script_file_name(), "halyard.bash");
    }
}
