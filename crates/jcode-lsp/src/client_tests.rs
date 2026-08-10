//! Unit tests for the client's private helpers.
//!
//! The client's behaviour is tested end-to-end in `tests/client.rs` against the real fake
//! server, which is where it belongs. This file is for the pure functions that an
//! integration test can only reach indirectly, and where a differential comparison against
//! omp is the point.

use super::*;

/// **A path with a `#` in it must not truncate the root URI.**
///
/// `path_to_uri` produces `rootUri` and the workspace folders, both sent in `initialize`.
/// It used to interpolate the path raw, with a comment deferring encoding to "the document
/// work, where a path with a `#` in it actually matters" -- but a `#` in the *root* starts
/// a URI fragment, so the root was silently truncated at the handshake and every later
/// import resolved against the wrong tree. The deferral was right about the case it named
/// and wrong about the case it was in.
///
/// Values are differential: printed from node's `pathToFileURL`, which is what omp uses
/// through Bun. Two of them contradicted my first draft -- `~` is escaped even though
/// RFC 3986 calls it unreserved, and `*` is not -- which is why the set was taken from the
/// output rather than reasoned about.
#[test]
fn path_to_uri_matches_path_to_file_url() {
    let cases: &[(&str, &str)] = &[
        ("/tmp/plain", "file:///tmp/plain"),
        // The two that silently truncate a raw path.
        ("/tmp/with#hash", "file:///tmp/with%23hash"),
        ("/tmp/with?q", "file:///tmp/with%3Fq"),
        ("/tmp/with space", "file:///tmp/with%20space"),
        // An existing escape must be escaped again, or it is ambiguous on the way back.
        ("/tmp/pct%20already", "file:///tmp/pct%2520already"),
        // Non-ASCII goes out as its UTF-8 bytes.
        ("/tmp/uni_café", "file:///tmp/uni_caf%C3%A9"),
        // Literal in a path segment, and left alone.
        ("/tmp/a+b", "file:///tmp/a+b"),
        ("/tmp/at@sign", "file:///tmp/at@sign"),
        ("/tmp/semi;colon", "file:///tmp/semi;colon"),
        ("/tmp/paren(s)", "file:///tmp/paren(s)"),
        ("/tmp/amp&and", "file:///tmp/amp&and"),
        ("/tmp/eq=x", "file:///tmp/eq=x"),
        ("/tmp/comma,x", "file:///tmp/comma,x"),
        ("/tmp/quote'x", "file:///tmp/quote'x"),
        ("/tmp/dollar$x", "file:///tmp/dollar$x"),
        ("/tmp/star*x", "file:///tmp/star*x"),
        // Escaped despite being unreserved in RFC 3986. Surprising, and pinned for it.
        ("/tmp/tilde~x", "file:///tmp/tilde%7Ex"),
        ("/tmp/back\\slash", "file:///tmp/back%5Cslash"),
        ("/tmp/pipe|x", "file:///tmp/pipe%7Cx"),
        ("/tmp/lt<x", "file:///tmp/lt%3Cx"),
        ("/tmp/quotes\"x", "file:///tmp/quotes%22x"),
        ("/tmp/caret^x", "file:///tmp/caret%5Ex"),
        ("/tmp/brace{x}", "file:///tmp/brace%7Bx%7D"),
        ("/tmp/bracket[x]", "file:///tmp/bracket%5Bx%5D"),
    ];
    for (path, expected) in cases {
        assert_eq!(
            path_to_uri(std::path::Path::new(path)),
            *expected,
            "for {path:?}, which node's pathToFileURL maps to {expected:?}"
        );
    }
}

/// The URI we produce round-trips through our own equivalence check.
///
/// Two functions in this crate have to agree about encoding: `path_to_uri` writes it and
/// `freshness::equivalent_uris` compares it. If they disagreed, the client would fail to
/// match a publish for a file whose own path it had encoded, which is a self-inflicted
/// version of the bug equivalence exists to prevent.
#[test]
fn an_encoded_root_matches_its_raw_spelling() {
    for path in [
        "/tmp/with space/src/main.rs",
        "/tmp/with#hash/src/main.rs",
        "/tmp/uni_café/src/main.rs",
    ] {
        let encoded = path_to_uri(std::path::Path::new(path));
        let raw = format!("file://{path}");
        assert!(
            crate::freshness::equivalent_uris(&encoded, &raw),
            "{encoded} and {raw} must be the same file"
        );
    }
}

/// A workspace folder carries the encoded URI and the plain name.
///
/// The name is for display and is not a URI, so it must *not* be encoded; the uri must be.
/// Sending them the same way is the mistake this rules out.
#[test]
fn a_workspace_folder_encodes_the_uri_but_not_the_name() {
    let folder = workspace_folder(std::path::Path::new("/tmp/my project"));
    assert_eq!(folder["uri"], "file:///tmp/my%20project");
    assert_eq!(folder["name"], "my project");
}

/// **A relative root must not become a URI authority.**
///
/// `pathToFileURL` resolves against the working directory first. This did not, so
/// `path_to_uri(".")` produced `file://.` -- and in a `file:` URI everything between `//`
/// and the next `/` is the *authority*. A server parsing that sees a host named `.` and an
/// empty path.
///
/// Not hypothetical: this crate's own integration helper passes `root: "."`, so every
/// `tests/client.rs` case had been handshaking with `rootUri: "file://."`. Nothing
/// complained because the fake server does not resolve imports, which is exactly how a real
/// server's "cannot find crate" would have been blamed on the server.
///
/// Found by an adversarial reviewer on the fifth pass.
#[test]
fn a_relative_root_is_absolutised_like_path_resolve() {
    let cwd = std::env::current_dir().expect("a working directory");
    let expected_prefix = format!("file://{}", cwd.display());

    let dot = path_to_uri(std::path::Path::new("."));
    assert_eq!(
        dot, expected_prefix,
        "a relative root must resolve against the working directory"
    );
    assert!(
        !dot.starts_with("file://."),
        "`.` became a URI authority: {dot}"
    );

    let nested = path_to_uri(std::path::Path::new("relative/x"));
    assert_eq!(nested, format!("{expected_prefix}/relative/x"));

    // An absolute path is untouched by the absolutising step.
    assert_eq!(path_to_uri(std::path::Path::new("/abs/x")), "file:///abs/x");
}

/// Interior `.` and `..` segments are resolved rather than passed through.
///
/// `path.resolve` collapses them, and a server comparing URIs by string would otherwise see
/// `/a/./b` and `/a/b` as different files. Shares the lexical normaliser with
/// `freshness::equivalent_uris`, so the two cannot disagree about what a path means.
#[test]
fn redundant_segments_are_collapsed_in_the_uri() {
    assert_eq!(path_to_uri(std::path::Path::new("/a/./b")), "file:///a/b");
    assert_eq!(
        path_to_uri(std::path::Path::new("/a/c/../b")),
        "file:///a/b"
    );
    assert_eq!(path_to_uri(std::path::Path::new("/a//b")), "file:///a/b");
}

/// Symlinks are **not** resolved, which is deliberate and differs from `canonicalize`.
///
/// A server told a symlink-resolved root reports diagnostics against paths the caller never
/// mentioned, and the caller then cannot match them to the file it asked about. `path.resolve`
/// does not resolve symlinks either, so following it is both simpler and right.
#[test]
fn the_uri_keeps_the_path_as_spelled_rather_than_resolving_symlinks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("real");
    std::fs::create_dir(&target).expect("mkdir");
    let link = temp.path().join("link");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    #[cfg(not(unix))]
    return;

    let uri = path_to_uri(&link);
    assert!(
        uri.ends_with("/link"),
        "the symlink was resolved to its target: {uri}"
    );
}
