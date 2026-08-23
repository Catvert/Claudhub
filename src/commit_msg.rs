//! Proposing a commit message from what is staged.
//!
//! **No API, a subprocess.** This is the same framing decision as everywhere
//! else: Claudhub's AI goes through a program the user has already installed
//! and authenticated — `claude -p`, or whatever they prefer — never through an
//! API key and an HTTP client of our own. The price of a `fork` bears no
//! comparison to that of a dependency with its own authentication, its own
//! quotas and its own error format.
//!
//! The diff goes out on **standard input** and never as an argument: a command
//! line has a maximum length — around two megabytes on Linux, but it is the
//! total that counts, environment included — and an agent-review diff brushes
//! against it. A pipe has no edge.
//!
//! Nothing here knows gpui, and both `prompt` and `clean` are free of I/O:
//! that is what makes them testable.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};

/// Beyond this, the diff is truncated.
///
/// This is not the model's limit but common sense: a commit message follows
/// from what changes, and the first hundred kilobytes of a diff already say it
/// all. Sending the five megabytes of a regenerated `composer.lock` would cost
/// tokens for one line of summary.
pub const MAX_DIFF: usize = 100_000;

/// How many recent messages serve as examples.
pub const RECENT: usize = 10;

/// An agent that thinks takes ten to thirty seconds; past two minutes it is
/// stuck, and the worker with it.
const TIMEOUT: Duration = Duration::from_secs(120);

/// What we ask the agent for.
///
/// The recent messages are in there because a repository's convention cannot
/// be guessed: the language first, but also the person of the verb and the
/// prefixes the team has given itself. An instruction written here would
/// impose them on every repository, which is exactly what we do not want.
pub fn prompt(recent: &[String], diff: &str) -> String {
    let (diff, truncated) = truncate(diff, MAX_DIFF);
    let mut out = String::with_capacity(diff.len() + 1024);
    out.push_str(
        "You are writing the message of the commit these staged changes will produce.\n\n\
         Answer with the message alone: no introductory sentence, no code block, \
         no surrounding quotes.\n\
         A summary line under 72 characters; then, if and only if the change \
         calls for it, a blank line and a body saying why rather than what.\n",
    );
    if recent.is_empty() {
        out.push_str(
            "\nThis repository has no commit yet: write the message in the language of the code.\n",
        );
    } else {
        out.push_str(
            "\nFollow the language and the conventions of this repository's recent messages:\n\n",
        );
        for subject in recent {
            out.push_str("  ");
            out.push_str(subject);
            out.push('\n');
        }
    }
    out.push_str("\nStaged diff:\n\n");
    out.push_str(diff);
    if truncated {
        out.push_str("\n\n[diff truncated: only the beginning is shown]");
    }
    out.push('\n');
    out
}

/// Cuts at `max` **bytes, on a character boundary**.
///
/// On raw bytes, an accented diff would be cut in the middle of a character
/// and the slice would no longer be valid UTF-8.
fn truncate(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// What the agent answered, brought back to a commit message.
///
/// A model readily wraps its answer in a code block or in quotes despite the
/// instruction, and those are characters that would end up as they are in the
/// repository's history. The cleanup lives here, pure and tested, rather than
/// in the view.
pub fn clean(output: &str) -> String {
    let mut text = output.trim();

    // A code block: the first line carries the fence and sometimes a language
    // name, the last one closes it.
    if text.starts_with("```") {
        if let Some(rest) = text.split_once('\n').map(|(_, rest)| rest) {
            text = rest.trim_end();
            if let Some(cut) = text.rfind("```") {
                text = text[..cut].trim_end();
            }
        }
    }

    let text = text.trim();
    // Quotes around the whole thing, and only if they really enclose it: a
    // message that both starts *and* ends with a quotation would keep its own.
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|q| {
            let inner = text.strip_prefix(*q)?.strip_suffix(*q)?;
            (!inner.contains(*q)).then_some(inner)
        })
        .unwrap_or(text);

    unquoted.trim().to_string()
}

/// Asks the configured agent for a message. **Never from the interface
/// thread**: this call takes seconds.
pub fn suggest(worktree: &Path, command_line: &str) -> Result<String> {
    let diff = crate::git::diff::staged_text(worktree)?;
    if diff.trim().is_empty() {
        bail!("nothing is staged: tick what should go into the commit");
    }
    let recent = crate::git::history::recent_subjects(worktree, RECENT);
    let answer = ask(worktree, command_line, &prompt(&recent, &diff))?;
    let message = clean(&answer);
    if message.is_empty() {
        bail!("the agent answered nothing");
    }
    Ok(message)
}

/// Runs the configured program, gives it the prompt on standard input, and
/// returns its standard output.
///
/// The wait is the git layer's (`wait_feeding`): all three streams go through
/// threads because a full pipe blocks whoever writes, and both the prompt and
/// the answer go well past a pipe's sixty-four kilobytes. Writing first and
/// then waiting would give the classic deadlock — the process waits for us to
/// read, we wait for it to finish. What differs here is only the ceiling, an
/// agent taking seconds where git takes milliseconds.
fn ask(worktree: &Path, command_line: &str, prompt: &str) -> Result<String> {
    let mut parts = crate::cmdline::split_command(command_line).into_iter();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("no message-generation command in the settings"))?;
    let args: Vec<String> = parts.collect();

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crate::git::wait_feeding(cmd, Some(prompt.as_bytes().to_vec()), TIMEOUT, || {
        program.clone()
    })?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        bail!(
            "{program} failed: {}",
            if message.is_empty() {
                "no message".into()
            } else {
                message.to_string()
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_shows_the_conventions_of_the_repository() {
        let text = prompt(&["Remove the seams".into()], "diff --git a/x b/x\n");
        assert!(text.contains("Remove the seams"), "{text}");
        assert!(text.contains("diff --git a/x b/x"), "{text}");
        assert!(!text.contains("truncated"), "{text}");
    }

    #[test]
    fn a_repository_without_commits_is_not_an_error() {
        let text = prompt(&[], "diff");
        assert!(text.contains("no commit yet"), "{text}");
    }

    /// The cut happens on a character boundary: on raw bytes, an accented diff
    /// would produce a slice that is not UTF-8.
    #[test]
    fn a_long_diff_is_cut_on_a_character_boundary() {
        let diff = "é".repeat(MAX_DIFF);
        let text = prompt(&[], &diff);
        assert!(text.contains("[diff truncated"), "the cut is not announced");
    }

    #[test]
    fn a_code_fence_never_reaches_the_history() {
        assert_eq!(clean("```\nAdd the button\n```"), "Add the button");
        assert_eq!(
            clean("```text\nAdd it\n\nBecause.\n```"),
            "Add it\n\nBecause."
        );
    }

    #[test]
    fn surrounding_quotes_are_dropped_but_not_the_others() {
        assert_eq!(clean("\"Add the button\""), "Add the button");
        assert_eq!(
            clean("Add « the button » and \"the box\""),
            "Add « the button » and \"the box\""
        );
    }

    #[test]
    fn a_plain_message_comes_out_untouched() {
        assert_eq!(
            clean("  Add the button\n\nBecause.\n"),
            "Add the button\n\nBecause."
        );
    }
}
