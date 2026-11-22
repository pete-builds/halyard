//! Command blocks in a pane: prompt navigation, the last command's output, failure
//! marks, and the shell integration line. Pure decisions over a
//! `halyard_shell::BlockTracker`; rows are counted from the top of scrollback, as
//! the session reports them.

use std::io;
use std::path::{Path, PathBuf};

use halyard_shell::{BlockTracker, Clock, Shell};

/// Which way to jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    /// The prompt above the top of the viewport.
    Previous,
    /// The prompt below the top of the viewport.
    Next,
}

/// The row to put at the top of the viewport for a prompt jump from a viewport
/// whose top row is `top`: the start of the nearest block above (or below) it.
/// `None` when there is none; for [`Jump::Next`] the caller then returns to the
/// live screen.
pub fn prompt_target<C: Clock>(blocks: &BlockTracker<C>, top: u64, jump: Jump) -> Option<u64> {
    let block = match jump {
        Jump::Previous => blocks.previous_block(top),
        Jump::Next => blocks.next_block(top),
    }?;
    block.start_row()
}

/// Rows `first..=last` holding the output of the most recently finished command,
/// or `None` when it printed nothing. The row of the closing D mark is left out:
/// shells draw the next prompt on that row. So a command whose C and D marks
/// share a row printed no full line (`true`, or text without a newline that the
/// prompt now shares) and gives `None`, and a last line printed without a
/// trailing newline is not copied (the mark's column is not recorded).
pub fn last_output_rows<C: Clock>(blocks: &BlockTracker<C>) -> Option<(u64, u64)> {
    let b = blocks.history().filter(|b| b.output_start_row.is_some()).last()?;
    let first = b.output_start_row?;
    let end = b.output_end_row?;
    (end > first).then(|| (first, end - 1))
}

/// Trims trailing line breaks from the copied text.
pub fn tidy_output(text: &str) -> String {
    text.trim_end_matches(['\n', '\r']).to_owned()
}

/// Spans of viewport rows (`first..=last`, 0 at the top of the viewport) covered
/// by blocks that finished with a non-zero status, for the failure marks in the
/// margin. `top` is the viewport's first row, `rows` its height.
pub fn failure_spans<C: Clock>(blocks: &BlockTracker<C>, top: u64, rows: u16) -> Vec<(u16, u16)> {
    let bottom = top + u64::from(rows);
    let mut spans = Vec::new();
    for b in blocks.history() {
        if !b.failed() {
            continue;
        }
        let (Some(start), Some(end)) = (b.start_row(), b.output_end_row) else { continue };
        // The D mark's row holds the next prompt (see `last_output_rows`).
        let end = if end > start { end - 1 } else { end };
        if end < top || start >= bottom {
            continue;
        }
        let first = start.max(top) - top;
        let last = end.min(bottom - 1) - top;
        // In range of a u16 viewport by construction.
        spans.push((u16::try_from(first).unwrap_or(0), u16::try_from(last).unwrap_or(rows)));
    }
    spans
}

/// Writes the integration script for `shell` into `dir` (created if needed) and
/// returns its path. The file is written next to its final name and renamed over
/// it, so a symlink planted at the final name is replaced, never followed.
pub fn write_script(dir: &Path, shell: Shell) -> io::Result<PathBuf> {
    use std::io::Write;

    std::fs::create_dir_all(dir)?;
    let path = dir.join(shell.script_file_name());
    let tmp = dir.join(format!(".{}.tmp", shell.script_file_name()));
    // The temporary name is predictable: remove whatever sits there (a symlink
    // is removed, not followed) and create the file only if it is new.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(halyard_shell::integration_script(shell).as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The shell a pane runs, from the configured program or the login shell.
pub fn shell_of(program: Option<&Path>) -> Option<Shell> {
    let p = program.map_or_else(halyard_term::default_shell, Path::to_path_buf);
    Shell::from_program(p.to_str()?)
}

/// The line to add to the shell's rc file, after writing the script into
/// `dir`. The error is a message for the banner.
pub fn integration_line(program: Option<&Path>, dir: &Path) -> Result<(Shell, String), String> {
    let shell = shell_of(program).ok_or_else(|| {
        "shell integration is available for zsh, bash and fish; this shell is none of them"
            .to_owned()
    })?;
    let path = write_script(dir, shell)
        .map_err(|e| format!("could not write the shell integration script: {e}"))?;
    Ok((shell, halyard_shell::install_line(shell, &path)))
}

#[cfg(test)]
mod tests {
    use halyard_shell::{ShellEvent, SystemClock};

    use super::*;

    /// A tracker with finished blocks whose prompts start at `starts`, each with
    /// output on the next row and its D two rows below the prompt; statuses from
    /// `codes`.
    fn tracker(starts: &[u64], codes: &[i32]) -> BlockTracker<SystemClock> {
        let mut t = BlockTracker::new(SystemClock, 100);
        for (&row, &code) in starts.iter().zip(codes) {
            t.on_event(&ShellEvent::PromptStart { aid: None }, row);
            t.on_event(&ShellEvent::CommandInputStart, row);
            t.on_event(&ShellEvent::CommandExecuted, row + 1);
            t.on_event(&ShellEvent::CommandFinished { exit_code: Some(code), aid: None }, row + 2);
        }
        t
    }

    #[test]
    fn prompt_jumps_go_to_the_nearest_block_start() {
        let t = tracker(&[10, 20, 30], &[0, 0, 0]);
        assert_eq!(prompt_target(&t, 25, Jump::Previous), Some(20));
        assert_eq!(prompt_target(&t, 20, Jump::Previous), Some(10), "strictly above the top");
        assert_eq!(prompt_target(&t, 10, Jump::Previous), None);
        assert_eq!(prompt_target(&t, 0, Jump::Next), Some(10));
        assert_eq!(prompt_target(&t, 20, Jump::Next), Some(30), "strictly below the top");
        assert_eq!(prompt_target(&t, 30, Jump::Next), None);
        let empty = BlockTracker::new(SystemClock, 10);
        assert_eq!(prompt_target(&empty, 5, Jump::Previous), None);
    }

    #[test]
    fn last_output_is_the_newest_finished_block() {
        let t = tracker(&[10, 20], &[0, 1]);
        // Output from row 21; the D mark (and the next prompt) on row 22.
        assert_eq!(last_output_rows(&t), Some((21, 21)));
        assert_eq!(last_output_rows(&BlockTracker::new(SystemClock, 10)), None);
        // A command with no output: C and D on the same row, which holds the
        // next prompt. Nothing to copy, rather than the prompt.
        let mut t = BlockTracker::new(SystemClock, 10);
        t.on_event(&ShellEvent::PromptStart { aid: None }, 30);
        t.on_event(&ShellEvent::CommandInputStart, 30);
        t.on_event(&ShellEvent::CommandExecuted, 31);
        t.on_event(&ShellEvent::CommandFinished { exit_code: Some(0), aid: None }, 31);
        assert_eq!(last_output_rows(&t), None);
        assert_eq!(tidy_output("a\nb\n\n"), "a\nb");
        assert_eq!(tidy_output("a"), "a");
    }

    #[test]
    fn failure_spans_are_clipped_to_the_viewport() {
        let t = tracker(&[2, 10, 20], &[1, 0, 2]);
        // Viewport rows 0..12: the first block (prompt 2, output 3, D and the
        // next prompt on 4) fails, the second passes.
        assert_eq!(failure_spans(&t, 0, 12), vec![(2, 3)]);
        // Viewport rows 15..25: the third block (20..=21) fails.
        assert_eq!(failure_spans(&t, 15, 10), vec![(5, 6)]);
        // A block cut by the top edge starts at viewport row 0.
        assert_eq!(failure_spans(&t, 21, 10), vec![(0, 0)]);
        // Nothing visible.
        assert_eq!(failure_spans(&t, 5, 4), Vec::<(u16, u16)>::new());
    }

    #[test]
    fn integration_line_writes_the_script_and_never_follows_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("victim");
        std::fs::write(&target, "keep me").unwrap();
        let scripts = dir.path().join("shell");
        std::fs::create_dir_all(&scripts).unwrap();
        std::os::unix::fs::symlink(&target, scripts.join("halyard.zsh")).unwrap();
        // A symlink planted at the temporary name is not followed either.
        let other = dir.path().join("other-victim");
        std::fs::write(&other, "keep me too").unwrap();
        std::os::unix::fs::symlink(&other, scripts.join(".halyard.zsh.tmp")).unwrap();
        let (shell, line) = integration_line(Some(Path::new("/bin/zsh")), &scripts).unwrap();
        assert_eq!(shell, Shell::Zsh);
        let path = scripts.join("halyard.zsh");
        assert_eq!(line, halyard_shell::install_line(Shell::Zsh, &path));
        assert!(!std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            halyard_shell::integration_script(Shell::Zsh)
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "keep me too");
        assert!(!scripts.join(".halyard.zsh.tmp").exists());
        // Other shells: a message, nothing written.
        let err = integration_line(Some(Path::new("/bin/dash")), &scripts).unwrap_err();
        assert!(err.contains("zsh, bash and fish"), "{err}");
        assert_eq!(shell_of(Some(Path::new("/usr/bin/fish"))), Some(Shell::Fish));
    }
}
