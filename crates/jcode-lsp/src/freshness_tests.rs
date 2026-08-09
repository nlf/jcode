//! Freshness tests.
//!
//! Group C. The decision logic is pure, so every branch is reachable without a
//! server, a clock, or a sleep — which is why it was written as a fed-observations
//! state machine rather than as a loop with a `select!` in it.

use super::*;
use serde_json::json;

fn diagnostic(message: &str) -> Value {
    json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        "message": message,
        "severity": 1
    })
}

fn seen(message: &str, version: Option<i64>, generation: u64) -> Observation {
    Observation {
        diagnostics: Some(vec![diagnostic(message)]),
        version,
        generation,
    }
}

fn nothing(generation: u64) -> Observation {
    Observation {
        diagnostics: None,
        version: None,
        generation,
    }
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// The reliable path: a publish naming our version describes our content, so there
/// is nothing to wait for. This is why the client advertises `versionSupport`.
#[test]
fn a_publish_matching_our_version_is_accepted_immediately() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: Some(4),
        ..Default::default()
    });

    assert_eq!(
        wait.observe(&seen("real error", Some(4), 1), ms(0)),
        Decision::Accept(Freshness::VersionMatched),
        "an exact version match needs no settle window"
    );
}

/// **A publish for an older version is definitively stale**, and settling on it
/// would report diagnostics for content the model already changed. Provable, so
/// there is no reason to guess.
#[test]
fn a_publish_for_an_older_version_is_never_settled_on() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: Some(4),
        settle: ms(50),
        timeout: ms(1_000),
    });

    // Stale, and it stays stale no matter how quiet the stream goes.
    assert_eq!(
        wait.observe(&seen("stale", Some(3), 1), ms(0)),
        Decision::Wait
    );
    assert_eq!(
        wait.observe(&seen("stale", Some(3), 1), ms(500)),
        Decision::Wait,
        "a provably stale publish must not be accepted by quiescence"
    );

    // The real one arrives and is taken at once.
    assert_eq!(
        wait.observe(&seen("real", Some(4), 2), ms(600)),
        Decision::Accept(Freshness::VersionMatched)
    );
}

/// **The unversioned path, which is the common one.** Many servers never echo a
/// version, so freshness cannot be matched and has to be settled: take the latest
/// publish once nothing newer has arrived for the settle window.
#[test]
fn an_unversioned_publish_is_accepted_once_the_stream_goes_quiet() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: None,
        settle: ms(100),
        timeout: ms(1_000),
    });

    // First sight starts the window.
    assert_eq!(wait.observe(&seen("error", None, 1), ms(0)), Decision::Wait);
    // Still inside it.
    assert_eq!(
        wait.observe(&seen("error", None, 1), ms(50)),
        Decision::Wait
    );
    // The window has passed with nothing newer.
    assert_eq!(
        wait.observe(&seen("error", None, 1), ms(100)),
        Decision::Accept(Freshness::Settled)
    );
}

/// **The case the settle window exists for.** omp's test has a stale publish at
/// +10ms and the real one at +150ms. A new publish must restart the window, or the
/// stale one is accepted and the model is told about errors from before its edit.
#[test]
fn a_newer_publish_restarts_the_settle_window() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: None,
        settle: ms(100),
        timeout: ms(2_000),
    });

    // The stale publish, at +10ms.
    assert_eq!(
        wait.observe(&seen("stale error", None, 1), ms(10)),
        Decision::Wait
    );
    assert_eq!(
        wait.observe(&seen("stale error", None, 1), ms(100)),
        Decision::Wait
    );

    // The real one, at +150ms. A *different generation*, so the window restarts and
    // the stale one is never accepted.
    assert_eq!(
        wait.observe(&seen("real error", None, 2), ms(150)),
        Decision::Wait,
        "a fresh publish must restart the window rather than inheriting the old one"
    );
    assert_eq!(
        wait.observe(&seen("real error", None, 2), ms(200)),
        Decision::Wait
    );
    assert_eq!(
        wait.observe(&seen("real error", None, 2), ms(250)),
        Decision::Accept(Freshness::Settled),
        "the window is measured from the newest publish"
    );
}

/// The generation counter is what distinguishes "the same publish" from "a fresh
/// publish of identical content". Comparing the diagnostics cannot: an unchanged
/// file republishes an identical list, and that republish means the server has
/// re-analysed.
#[test]
fn an_identical_republish_still_restarts_the_window() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: None,
        settle: ms(100),
        timeout: ms(2_000),
    });

    assert_eq!(wait.observe(&seen("same", None, 1), ms(0)), Decision::Wait);
    assert_eq!(wait.observe(&seen("same", None, 1), ms(90)), Decision::Wait);
    // Byte-identical diagnostics, new generation: the server published again.
    assert_eq!(
        wait.observe(&seen("same", None, 2), ms(95)),
        Decision::Wait,
        "identical content from a new publish is still a new publish"
    );
    assert_eq!(
        wait.observe(&seen("same", None, 2), ms(195)),
        Decision::Accept(Freshness::Settled)
    );
}

/// **An absent publish must never settle.** Treating "nothing yet" as quiescence
/// would report a clean file the instant the settle window passed, which is the
/// silent-wrong-answer this whole module exists to prevent.
#[test]
fn nothing_published_never_settles_into_a_clean_result() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: None,
        settle: ms(50),
        timeout: ms(500),
    });

    assert_eq!(wait.observe(&nothing(0), ms(0)), Decision::Wait);
    assert_eq!(
        wait.observe(&nothing(0), ms(100)),
        Decision::Wait,
        "silence is not a clean file, however long it lasts"
    );
    assert_eq!(
        wait.observe(&nothing(0), ms(200)),
        Decision::Wait,
        "still not clean"
    );
    // Only the budget ends it, and it ends as a timeout rather than as a result.
    assert_eq!(
        wait.observe(&nothing(0), ms(500)),
        Decision::Accept(Freshness::TimedOut)
    );
}

/// A timeout is **not** "no problems". The caller must be able to tell "the server
/// says it is clean" from "the server did not answer", because reporting the second
/// as the first tells the model its broken edit compiled.
#[test]
fn a_timeout_is_reported_as_a_timeout_not_as_a_result() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: Some(9),
        settle: ms(1_000),
        timeout: ms(100),
    });

    // A publish exists, but for the wrong version and with no time to settle.
    assert_eq!(
        wait.observe(&seen("old", Some(2), 1), ms(100)),
        Decision::Accept(Freshness::TimedOut),
        "out of budget with only a stale publish is a timeout, not a success"
    );
}

/// With no expected version, an arbitrary published version is not evidence of
/// anything and must not short-circuit the settle. A client that matched on
/// "any version present" would accept the first publish it saw.
#[test]
fn a_published_version_without_an_expected_one_does_not_short_circuit() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: None,
        settle: ms(100),
        timeout: ms(1_000),
    });

    assert_eq!(
        wait.observe(&seen("error", Some(7), 1), ms(0)),
        Decision::Wait,
        "without an expectation there is nothing to match against"
    );
    assert_eq!(
        wait.observe(&seen("error", Some(7), 1), ms(100)),
        Decision::Accept(Freshness::Settled)
    );
}

/// And the converse: expecting a version but receiving none falls back to
/// settling. A server that ignores `versionSupport` must still be usable.
#[test]
fn an_expected_version_with_unversioned_publishes_falls_back_to_settling() {
    let mut wait = FreshnessWait::new(FreshnessRequest {
        expected_version: Some(3),
        settle: ms(100),
        timeout: ms(1_000),
    });

    assert_eq!(wait.observe(&seen("error", None, 1), ms(0)), Decision::Wait);
    assert_eq!(
        wait.observe(&seen("error", None, 1), ms(100)),
        Decision::Accept(Freshness::Settled),
        "a server that ignores versionSupport must still work"
    );
}

// =============================================================================
// URI equivalence
// =============================================================================

/// A server may publish under a percent-encoded spelling of the URI we sent.
/// Matching as strings misses the publish, and the wait then times out against a
/// server that answered correctly. omp's test encodes the `r` of `renormalized.ts`.
#[test]
fn a_percent_encoded_uri_matches_its_raw_spelling() {
    assert!(equivalent_uris(
        "file:///tmp/renormalized.ts",
        "file:///tmp/%72enormalized.ts"
    ));
    // And the same in reverse, since either side may be the encoded one.
    assert!(equivalent_uris(
        "file:///tmp/%72enormalized.ts",
        "file:///tmp/renormalized.ts"
    ));
}

#[test]
fn identical_uris_match_without_decoding() {
    assert!(equivalent_uris("file:///a/b.rs", "file:///a/b.rs"));
}

/// Windows drive letters differ in case between clients and servers, and
/// `file:///C:/x` and `file:///c:/x` are the same file.
#[test]
fn windows_drive_letter_case_is_folded() {
    assert!(equivalent_uris(
        "file:///C:/src/main.rs",
        "file:///c:/src/main.rs"
    ));
    assert!(equivalent_uris(
        "file:///c:/src/main.rs",
        "file:///C:/src/main.rs"
    ));
}

/// **Only the drive letter folds.** Folding the whole path would make two
/// genuinely different files compare equal on the platforms we care about, which is
/// worse than a missed publish: it would apply one file's diagnostics to another.
#[test]
fn the_rest_of_the_path_stays_case_sensitive() {
    assert!(
        !equivalent_uris("file:///C:/src/Main.rs", "file:///C:/src/main.rs"),
        "case-folding the path would confuse two different files"
    );
    assert!(!equivalent_uris("file:///a/B.rs", "file:///a/b.rs"));
}

#[test]
fn different_files_do_not_match() {
    assert!(!equivalent_uris("file:///a/b.rs", "file:///a/c.rs"));
    assert!(!equivalent_uris("file:///a/b.rs", "file:///x/a/b.rs"));
}

/// Percent-encoded characters that actually need encoding must round-trip. A path
/// containing a `#` is the case that breaks naive URI handling, because unencoded
/// it parses as a fragment and truncates the path.
#[test]
fn encoded_special_characters_decode_to_the_same_path() {
    assert!(equivalent_uris(
        "file:///tmp/we%23ird.rs",
        "file:///tmp/we#ird.rs"
    ));
    assert!(equivalent_uris(
        "file:///tmp/a%20space.rs",
        "file:///tmp/a space.rs"
    ));
}

/// An invalid escape must be left alone rather than rejected or mangled. A server
/// sending a stray `%` should not make us stop matching its publishes entirely.
#[test]
fn an_invalid_percent_escape_is_left_as_written() {
    assert!(equivalent_uris(
        "file:///tmp/100%.rs",
        "file:///tmp/100%.rs"
    ));
    assert!(!equivalent_uris(
        "file:///tmp/100%.rs",
        "file:///tmp/100.rs"
    ));
}

/// A truncated escape at the very end must not panic. Reached by a server
/// misencoding a path, and an index-out-of-bounds here would kill the reader task
/// and hang every pending request.
#[test]
fn a_truncated_escape_at_the_end_does_not_panic() {
    assert!(equivalent_uris("file:///tmp/x%", "file:///tmp/x%"));
    assert!(equivalent_uris("file:///tmp/x%4", "file:///tmp/x%4"));
    assert!(!equivalent_uris("file:///tmp/x%4", "file:///tmp/x"));
}

/// A bare `%` followed by two non-hex characters is not an escape.
#[test]
fn a_non_hex_escape_is_not_decoded() {
    assert!(equivalent_uris("file:///tmp/%zz.rs", "file:///tmp/%zz.rs"));
    assert!(!equivalent_uris("file:///tmp/%zz.rs", "file:///tmp/zz.rs"));
}

/// A non-`file:` URI has no drive letter to fold and must be left alone rather
/// than mangled by the Windows path.
#[test]
fn a_non_file_uri_is_compared_as_written() {
    assert!(equivalent_uris(
        "untitled:Untitled-1",
        "untitled:Untitled-1"
    ));
    assert!(!equivalent_uris(
        "untitled:Untitled-1",
        "untitled:Untitled-2"
    ));
}

/// A path whose first character happens to be a letter followed by a colon, but
/// which is not a Windows drive, must not be folded. `file:///ab:c` has no drive.
#[test]
fn only_a_single_letter_followed_by_a_colon_is_treated_as_a_drive() {
    // Two letters before the colon: not a drive, so no folding, so case matters.
    assert!(!equivalent_uris("file:///AB:/x", "file:///ab:/x"));
}

/// **Redundant path segments must not hide a publish.**
///
/// A server is free to echo the URI it was given in a different but equivalent
/// spelling. omp keys its diagnostics map by `path.normalize(uriToFile(uri))`, so all
/// of these are one file to them. This function said they were three different files,
/// which means the freshness wait never sees the publish, times out, and the caller
/// reports no diagnostics for a file the server analysed correctly.
///
/// Found by an adversarial reviewer pointing at omp's `EquivalentUriMap`. The old doc
/// comment claimed the omission was deliberate, on the grounds that resolving `..`
/// would be slow and unsafe -- conflating lexical normalization with canonicalization.
/// omp does the former and not the latter, and so do we now.
#[test]
fn equivalent_spellings_of_one_path_compare_equal() {
    assert!(equivalent_uris("file:///a/b.rs", "file:///a/./b.rs"));
    assert!(equivalent_uris("file:///a/b.rs", "file:///a//b.rs"));
    assert!(equivalent_uris("file:///a/b.rs", "file:///a/c/../b.rs"));
    assert!(equivalent_uris("file:///a/b.rs", "file:///a/b.rs"));

    // And genuinely different files still differ.
    assert!(!equivalent_uris("file:///a/b.rs", "file:///a/c.rs"));
    assert!(!equivalent_uris("file:///a/b.rs", "file:///x/b.rs"));
    // `..` must not be discarded rather than applied: /a/x/../b is /a/b, not /a/x/b.
    assert!(!equivalent_uris("file:///a/x/b.rs", "file:///a/x/../b.rs"));
}

/// The lexical normalizer matches Node's `path.normalize`, case for case.
///
/// A differential test against values printed by the real `path.normalize`, rather
/// than against what I expected it to do. omp keys their URI map with that exact
/// function, so matching my own guess would prove nothing about matching them.
///
/// The last three are the ones a hand-rolled version gets wrong: `..` above the root
/// is dropped, `..` in a relative path is kept because there is nothing to cancel
/// against, and a path that cancels itself out becomes `.` rather than empty.
#[test]
fn lexical_normalization_matches_nodes_path_normalize() {
    // Printed by `node -e` from path.normalize; see the commit message.
    let cases: &[(&str, &str)] = &[
        ("/a/b.rs", "/a/b.rs"),
        ("/a/./b.rs", "/a/b.rs"),
        ("/a//b.rs", "/a/b.rs"),
        ("/a/c/../b.rs", "/a/b.rs"),
        ("/", "/"),
        ("/..", "/"),
        ("/../a", "/a"),
        ("/a/../../b", "/b"),
        ("/a/b/..", "/a"),
        ("a/../b", "b"),
        ("../a", "../a"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            lexically_normalize(input),
            *expected,
            "for {input:?}, which node's path.normalize maps to {expected:?}"
        );
    }
}

/// A trailing slash is dropped, which is a deliberate divergence from
/// `path.normalize`.
///
/// Node keeps it (`/a/` stays `/a/`), so `file:///a/` and `file:///a` would be
/// different keys. They are the same directory, and this function's only job is
/// deciding whether two URIs mean the same thing. The root's own slash is kept,
/// because dropping it would leave an empty path.
///
/// This falls out of segment joining rather than being implemented: an explicit
/// trailing-slash pop was written, and a mutation deleting it changed no output for
/// any input, so it was dead code and was removed. The assertions stay, because the
/// *behaviour* is load-bearing even though nothing special implements it -- they now
/// guard against a future rewrite that reintroduces the slash.
#[test]
fn a_trailing_slash_does_not_make_a_different_path() {
    assert_eq!(lexically_normalize("/a/"), "/a");
    assert_eq!(lexically_normalize("/"), "/");
    assert!(equivalent_uris("file:///a/b/", "file:///a/b"));
}

/// Percent-encoding and redundant segments compose.
///
/// omp's own freshness test has a server renormalizing `/renormalized.ts` to
/// `/%72enormalized.ts`, so a server that does both at once is not hypothetical.
#[test]
fn percent_encoding_and_redundant_segments_are_both_handled() {
    assert!(equivalent_uris(
        "file:///a/renormalized.ts",
        "file:///a/./%72enormalized.ts"
    ));
}
