//! Behaviour spec for shell tokenizing.
//!
//! Rules from oh-my-pi's `src/tools/shell-tokenize.ts`. The property that
//! matters most is the bail-out: anything this cannot read confidently yields
//! no segments, so the caller falls back rather than acting on a guess.

use super::*;

fn texts(command: &str) -> Vec<String> {
    segments(command)
        .into_iter()
        .map(|segment| segment.text)
        .collect()
}

#[test]
fn a_plain_command_is_one_segment() {
    assert_eq!(texts("cat file.txt"), vec!["cat file.txt"]);
}

#[test]
fn semicolons_and_newlines_separate_segments() {
    assert_eq!(texts("cd src; ls"), vec!["cd src", "ls"]);
    assert_eq!(texts("cd src\nls"), vec!["cd src", "ls"]);
}

#[test]
fn logical_operators_separate_segments() {
    assert_eq!(texts("make && ./run"), vec!["make", "./run"]);
    assert_eq!(texts("make || echo failed"), vec!["make", "echo failed"]);
}

/// The first stage of a pipeline reads a file, so it is redirectable. The later
/// stages read stdin, which no path-based tool can replace.
#[test]
fn only_later_pipeline_stages_are_marked_as_piped() {
    let parsed = segments("cat file.txt | grep needle");

    assert_eq!(parsed.len(), 2);
    assert!(!parsed[0].piped_stdin, "the first stage reads a file");
    assert!(parsed[1].piped_stdin, "the second stage reads stdin");
}

/// `||` is not a pipe: the right side runs on its own, with its own stdin.
#[test]
fn a_logical_or_does_not_mark_the_next_segment_as_piped() {
    let parsed = segments("make || grep needle file");
    assert!(!parsed[1].piped_stdin);
}

#[test]
fn a_stderr_pipe_still_marks_the_next_segment() {
    let parsed = segments("build |& grep error");
    assert_eq!(parsed.len(), 2);
    assert!(parsed[1].piped_stdin);
}

/// Separators inside quotes are text, not structure. Splitting there would cut
/// a command in half and match a fragment.
#[test]
fn separators_inside_quotes_do_not_split() {
    assert_eq!(texts("echo 'a; b'"), vec!["echo 'a; b'"]);
    assert_eq!(texts("echo \"a | b\""), vec!["echo \"a | b\""]);
    assert_eq!(texts("echo 'a && b'"), vec!["echo 'a && b'"]);
}

/// Redirection operators are not separators. `>|`, `>&`, `<&` and `&>` all
/// belong to the command they sit in.
#[test]
fn redirection_operators_are_not_separators() {
    assert_eq!(texts("echo hi >| out.txt"), vec!["echo hi >| out.txt"]);
    assert_eq!(texts("cmd >& out.txt"), vec!["cmd >& out.txt"]);
    assert_eq!(texts("cmd 2>&1"), vec!["cmd 2>&1"]);
    assert_eq!(texts("cmd &> out.txt"), vec!["cmd &> out.txt"]);
}

/// Bailing out is the safe answer: the contents of a substitution are a
/// different command, and interpreting them as text would match the wrong
/// thing.
#[test]
fn anything_unreadable_yields_no_segments() {
    for command in [
        "echo $(cat file)",
        "echo `cat file`",
        "(cd src && ls)",
        "cat <<EOF\nbody\nEOF",
        "{ ls; }",
        "echo ${VAR}",
        "echo \"$(cat file)\"",
    ] {
        assert_eq!(
            segments(command),
            Vec::new(),
            "{command:?} should bail out rather than be guessed at"
        );
    }
}

#[test]
fn an_unterminated_quote_yields_no_segments() {
    assert_eq!(segments("echo 'unclosed"), Vec::new());
    assert_eq!(segments("echo \"unclosed"), Vec::new());
    assert_eq!(segments("echo trailing\\"), Vec::new());
}

#[test]
fn comments_are_not_commands() {
    assert_eq!(texts("ls # list things"), vec!["ls"]);
    assert_eq!(texts("# just a comment\nls"), vec!["ls"]);
}

/// A `#` mid-word is part of the word, as in a filename or a URL fragment.
#[test]
fn a_hash_inside_a_word_is_not_a_comment() {
    assert_eq!(texts("cat file#1.txt"), vec!["cat file#1.txt"]);
}

#[test]
fn empty_and_blank_commands_yield_nothing() {
    assert_eq!(segments(""), Vec::new());
    assert_eq!(segments("   "), Vec::new());
    assert_eq!(segments(";;"), Vec::new());
}

#[test]
fn skipping_a_word_stops_at_whitespace() {
    assert_eq!(skip_word("one two", 0), Some(3));
    assert_eq!(skip_word("one", 0), Some(3));
}

#[test]
fn skipping_a_word_treats_a_quoted_run_as_one_word() {
    assert_eq!(skip_word("'a b' c", 0), Some(5));
    assert_eq!(skip_word("\"a b\" c", 0), Some(5));
}

#[test]
fn skipping_an_unterminated_word_fails() {
    assert_eq!(skip_word("'unclosed", 0), None);
    assert_eq!(skip_word("trailing\\", 0), None);
}

/// `FOO=1 cat x` is a `cat` call. A rule matching on the leading word would
/// miss it entirely, which is the whole reason this exists.
#[test]
fn leading_assignments_are_stripped() {
    assert_eq!(
        without_leading_assignments("FOO=1 cat file.txt").as_deref(),
        Some("cat file.txt")
    );
    assert_eq!(
        without_leading_assignments("A=1 B=2 grep x f").as_deref(),
        Some("grep x f")
    );
}

#[test]
fn a_quoted_assignment_value_is_one_word() {
    assert_eq!(
        without_leading_assignments("FOO='a b' cat file").as_deref(),
        Some("cat file")
    );
}

/// No assignments means nothing to strip, and the caller already has the
/// original.
#[test]
fn a_command_without_assignments_yields_nothing() {
    assert_eq!(without_leading_assignments("cat file.txt"), None);
    assert_eq!(without_leading_assignments(""), None);
}

/// Assignments with no command after them run nothing that could be
/// intercepted.
#[test]
fn assignments_with_no_command_yield_nothing() {
    assert_eq!(without_leading_assignments("FOO=1"), None);
    assert_eq!(without_leading_assignments("FOO=1 BAR=2"), None);
}

/// A name that is not a valid assignment ends the prefix rather than being
/// consumed, or `FOO=1 not-an-assignment cat x` would lose the middle word.
#[test]
fn a_non_assignment_word_ends_the_prefix() {
    assert_eq!(
        without_leading_assignments("FOO=1 some-command arg").as_deref(),
        Some("some-command arg")
    );
}

/// An unterminated value means the command cannot be read.
#[test]
fn an_unterminated_assignment_value_yields_nothing() {
    assert_eq!(without_leading_assignments("FOO='unclosed cat x"), None);
}
