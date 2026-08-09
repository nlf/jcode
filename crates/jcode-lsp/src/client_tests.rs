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
