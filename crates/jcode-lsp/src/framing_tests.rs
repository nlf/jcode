//! Framing tests.
//!
//! These are the tests that justify not using a mock for the client: every case
//! here is a way a real pipe delivers bytes that a "read a line, then read N
//! bytes" implementation gets wrong.

use super::*;

fn frame(body: &str) -> Vec<u8> {
    encode(body.as_bytes())
}

/// The next message as text, or `None` for incomplete.
///
/// Panics on a resync, so a test that expects a message and gets junk says so
/// rather than reporting "incomplete" and looking like a buffering problem.
fn take(framer: &mut MessageFramer) -> Option<String> {
    match framer.next_message().expect("framing should succeed") {
        Framed::Message(body) => Some(String::from_utf8(body).expect("utf-8")),
        Framed::Incomplete => None,
        Framed::Resync { headers } => panic!("unexpected resync on {headers:?}"),
    }
}

#[test]
fn a_whole_message_in_one_chunk_round_trips() {
    let mut framer = MessageFramer::new();
    framer.push(&frame(r#"{"jsonrpc":"2.0"}"#));
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"jsonrpc":"2.0"}"#));
    assert_eq!(take(&mut framer), None);
    assert_eq!(framer.buffered(), 0, "a consumed message must not linger");
}

/// The case the module exists for. A pipe can split anywhere, including inside
/// the header, and an implementation that reads a line first will block or
/// misparse.
#[test]
fn a_header_split_across_reads_still_parses() {
    let framed = frame(r#"{"id":1}"#);
    let split = 8; // mid-way through "Content-Length"
    let mut framer = MessageFramer::new();

    framer.push(&framed[..split]);
    assert_eq!(
        take(&mut framer),
        None,
        "a partial header is not yet a message"
    );

    framer.push(&framed[split..]);
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":1}"#));
}

/// The other half of the same problem, and the one that silently corrupts
/// rather than blocking: the header says 20 bytes, only 12 have arrived, and
/// slicing anyway yields truncated JSON.
#[test]
fn a_body_split_across_reads_still_parses() {
    let body = r#"{"id":1,"result":null}"#;
    let framed = frame(body);
    let header_len = framed.len() - body.len();
    let split = header_len + 5;

    let mut framer = MessageFramer::new();
    framer.push(&framed[..split]);
    assert_eq!(take(&mut framer), None, "a partial body is not a message");
    assert!(
        framer.buffered() > 0,
        "the partial body must be retained, not dropped"
    );

    framer.push(&framed[split..]);
    assert_eq!(take(&mut framer).as_deref(), Some(body));
}

/// Byte-at-a-time is the pathological version of the two cases above, and it
/// catches any place that assumes a read yields something useful.
#[test]
fn a_message_delivered_one_byte_at_a_time_still_parses() {
    let body = r#"{"method":"initialized"}"#;
    let framed = frame(body);
    let mut framer = MessageFramer::new();

    for (index, byte) in framed.iter().enumerate() {
        framer.push(&[*byte]);
        let last = index == framed.len() - 1;
        match take(&mut framer) {
            Some(message) => {
                assert!(last, "a message completed early, at byte {index}");
                assert_eq!(message, body);
            }
            None => assert!(!last, "the final byte did not complete the message"),
        }
    }
}

/// Several messages commonly arrive in one read, and an implementation that
/// handles one per read leaves the rest sitting in the buffer until the server
/// happens to send more. That presents as intermittent hangs.
#[test]
fn several_messages_in_one_chunk_all_come_out_in_order() {
    let mut chunk = Vec::new();
    for body in [r#"{"id":1}"#, r#"{"id":2}"#, r#"{"id":3}"#] {
        chunk.extend_from_slice(&frame(body));
    }

    let mut framer = MessageFramer::new();
    framer.push(&chunk);

    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":1}"#));
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":2}"#));
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":3}"#));
    assert_eq!(take(&mut framer), None);
    assert_eq!(framer.buffered(), 0);
}

/// A chunk ending mid-message must yield what it can and keep the tail, or the
/// partial message is lost and the stream desynchronises.
#[test]
fn a_chunk_holding_one_whole_message_and_part_of_another_keeps_the_tail() {
    let second = frame(r#"{"id":2}"#);
    let mut chunk = frame(r#"{"id":1}"#);
    chunk.extend_from_slice(&second[..6]);

    let mut framer = MessageFramer::new();
    framer.push(&chunk);
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":1}"#));
    assert_eq!(take(&mut framer), None);

    framer.push(&second[6..]);
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":2}"#));
}

/// `Content-Length` counts bytes. A body of multi-byte characters has more
/// bytes than characters, and an implementation counting characters cuts the
/// body short, leaving the tail to be misread as the next header. Every
/// subsequent message is then misframed, so this single mistake breaks the
/// session rather than one message.
#[test]
fn a_non_ascii_body_is_measured_in_bytes_not_characters() {
    let body = r#"{"message":"héllo → wörld 日本語"}"#;
    assert_ne!(
        body.len(),
        body.chars().count(),
        "the fixture must actually be multi-byte or it proves nothing"
    );

    let framed = encode(body.as_bytes());
    let header = String::from_utf8_lossy(&framed[..framed.len() - body.len()]);
    assert!(
        header.contains(&format!("Content-Length: {}", body.len())),
        "the header must carry the byte length, got: {header:?}"
    );

    let mut framer = MessageFramer::new();
    framer.push(&framed);
    assert_eq!(take(&mut framer).as_deref(), Some(body));
    assert_eq!(framer.buffered(), 0, "a byte/char mismatch leaves a tail");
}

/// Two non-ASCII messages back to back: the first mismeasurement would make the
/// second unreadable, which is what makes this stronger than the single-message
/// case above.
#[test]
fn consecutive_non_ascii_messages_stay_synchronised() {
    let first = r#"{"a":"日本語"}"#;
    let second = r#"{"b":"→→→"}"#;
    let mut chunk = encode(first.as_bytes());
    chunk.extend_from_slice(&encode(second.as_bytes()));

    let mut framer = MessageFramer::new();
    framer.push(&chunk);
    assert_eq!(take(&mut framer).as_deref(), Some(first));
    assert_eq!(take(&mut framer).as_deref(), Some(second));
}

/// Header names are case-insensitive per RFC 7230 and real servers vary. An
/// exact match on `Content-Length` rejects a legal frame.
#[test]
fn a_lowercase_header_name_is_accepted() {
    let mut framer = MessageFramer::new();
    framer.push(b"content-length: 8\r\n\r\n{\"id\":1}");
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":1}"#));
}

/// The spec defines `Content-Type` and some servers send it. Order is not
/// guaranteed, so both orderings must work.
#[test]
fn an_extra_content_type_header_is_ignored_in_either_order() {
    let mut before = MessageFramer::new();
    before.push(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 8\r\n\r\n{\"id\":1}");
    assert_eq!(take(&mut before).as_deref(), Some(r#"{"id":1}"#));

    let mut after = MessageFramer::new();
    after.push(b"Content-Length: 8\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{\"id\":1}");
    assert_eq!(take(&mut after).as_deref(), Some(r#"{"id":1}"#));
}

#[test]
fn surrounding_whitespace_in_the_length_value_is_tolerated() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Length:   8  \r\n\r\n{\"id\":1}");
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":1}"#));
}

/// An empty body is legal framing. It is not valid JSON-RPC, but that is the
/// layer above's problem: conflating the two would make a framing bug and a
/// protocol bug indistinguishable.
#[test]
fn a_zero_length_body_is_a_message_not_an_error() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Length: 0\r\n\r\n");
    assert_eq!(take(&mut framer).as_deref(), Some(""));
}

/// **A header block with no `Content-Length` is noise, not a fatal error.**
///
/// Corrected from omp, whose framer calls an `onResync` callback and drops past
/// the offending terminator. This was fatal in my first draft, which would tear
/// down a healthy server because a launcher script echoed a line to stdout.
#[test]
fn a_header_block_without_content_length_resyncs_rather_than_failing() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Type: application/json\r\n\r\n");
    match framer.next_message().expect("must not be fatal") {
        Framed::Resync { headers } => {
            // The header text is the only clue about what is polluting stdout.
            assert!(headers.contains("Content-Type"), "{headers:?}");
        }
        other => panic!("expected a resync, got {other:?}"),
    }
}

/// The point of resyncing: a real message behind the junk must still arrive.
/// Failing fatally, or looping on the same header, both lose it.
#[test]
fn a_real_message_after_junk_is_still_delivered() {
    let mut framer = MessageFramer::new();
    // A wrapper script announcing itself, then a genuine message.
    framer.push(b"Starting language server v1.2.3\r\n\r\n");
    framer.push(&frame(r#"{"id":1}"#));

    match framer.next_message().expect("not fatal") {
        Framed::Resync { .. } => {}
        other => panic!("expected a resync first, got {other:?}"),
    }
    assert_eq!(
        take(&mut framer).as_deref(),
        Some(r#"{"id":1}"#),
        "the message behind the noise must survive"
    );
}

/// A non-numeric length is the same situation as a missing one: we cannot locate
/// a body either way, and the caller's only option is to skip.
#[test]
fn a_non_numeric_content_length_resyncs_too() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Length: banana\r\n\r\n");
    framer.push(&frame(r#"{"id":2}"#));

    match framer.next_message().expect("not fatal") {
        Framed::Resync { headers } => assert!(headers.contains("banana"), "{headers:?}"),
        other => panic!("expected a resync, got {other:?}"),
    }
    assert_eq!(take(&mut framer).as_deref(), Some(r#"{"id":2}"#));
}

/// Resyncing must consume the junk. If it does not, the caller loops on the same
/// header forever, which presents as a hang rather than as an error — strictly
/// worse than the fatal behaviour it replaced.
#[test]
fn resyncing_consumes_the_junk_so_the_caller_cannot_loop() {
    let mut framer = MessageFramer::new();
    framer.push(b"junk: yes\r\n\r\n");
    let before = framer.buffered();

    assert!(matches!(
        framer.next_message().expect("not fatal"),
        Framed::Resync { .. }
    ));
    assert!(
        framer.buffered() < before,
        "the junk header must be consumed, or the caller spins on it"
    );
    assert_eq!(
        framer.next_message().expect("not fatal"),
        Framed::Incomplete,
        "with the junk gone and nothing behind it, the answer is incomplete"
    );
}

/// A corrupted length would otherwise have us wait forever for bytes that are
/// never coming, holding a reservation for the claimed size. This is the one
/// genuinely fatal case: we cannot skip a body whose length we do not trust.
#[test]
fn a_body_over_the_cap_is_refused_rather_than_buffered() {
    let mut framer = MessageFramer::new();
    framer.push(format!("Content-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1).as_bytes());
    let error = framer.next_message().expect_err("must not be accepted");
    assert!(
        matches!(error, FramingError::BodyTooLarge { .. }),
        "got {error:?}"
    );
}

/// A `\n\n` separator is not a legal LSP header terminator and must not be
/// treated as one. Accepting it would make a body containing a blank line
/// parseable as a frame boundary, which desynchronises the stream on the first
/// message carrying formatted text — and hover contents are formatted text.
#[test]
fn a_bare_lf_separator_is_not_a_header_terminator() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Length: 8\n\n{\"id\":1}");
    assert_eq!(
        take(&mut framer),
        None,
        "\\n\\n must not terminate a header block"
    );
}

/// A body containing the header terminator must survive. The framer must locate
/// the body by *length*, never by searching for the next `\r\n\r\n`, because
/// hover and diagnostic payloads legitimately contain CRLF pairs.
///
/// Note the `buffered()` assertion, which is doing more work than it looks. An
/// earlier version of this test checked only the returned body and **passed
/// against an implementation that consumed up to the embedded terminator** —
/// because the body slice was still taken by length, and only the *consumption*
/// was wrong. That leaves the tail of this message in the buffer to be misread
/// as the next header. Found by mutation testing; the lesson is that a framing
/// test must assert what was consumed, not only what was produced.
#[test]
fn a_body_containing_the_header_terminator_is_read_by_length() {
    let body = "{\"text\":\"line\\r\\n\\r\\nnext\"}";
    let framed = encode(body.as_bytes());
    let mut framer = MessageFramer::new();
    framer.push(&framed);
    assert_eq!(take(&mut framer).as_deref(), Some(body));
    assert_eq!(
        framer.buffered(),
        0,
        "the whole frame must be consumed, not just up to the embedded terminator"
    );
}

/// The same hazard with *real* CRLF bytes rather than escaped ones, which is
/// what a server sending a raw multi-line string produces. A boundary-searching
/// implementation cuts this message in half.
///
/// A second message follows, because that is what actually exposes the bug: with
/// only one message in flight a mis-consumed tail is invisible, and here it gets
/// misparsed as the next frame's header.
#[test]
fn a_body_with_literal_crlf_crlf_bytes_is_read_by_length() {
    let first = b"raw\r\n\r\nbody".to_vec();
    let second = br#"{"id":2}"#.to_vec();
    let mut chunk = encode(&first);
    chunk.extend_from_slice(&encode(&second));

    let mut framer = MessageFramer::new();
    framer.push(&chunk);
    assert_eq!(
        framer.next_message().expect("framing"),
        Framed::Message(first),
        "the body must be taken by length, not by scanning for a terminator"
    );
    assert_eq!(
        framer.next_message().expect("framing"),
        Framed::Message(second),
        "a CRLF inside the first body must not desynchronise the next message"
    );
    assert_eq!(framer.buffered(), 0);
}

#[test]
fn pushing_nothing_changes_nothing() {
    let mut framer = MessageFramer::new();
    framer.push(&[]);
    assert_eq!(take(&mut framer), None);
    assert_eq!(framer.buffered(), 0);
}

/// **A banner terminated by a bare LF must not eat the message after it.**
///
/// Found by an adversarial reviewer, not by me. Servers print to stdout before they
/// start framing (`rust-analyzer` and `gopls` both have modes that do), and a banner
/// ending in `\n` rather than `\r\n` used to take the following real message with it:
/// the header block was split on `\r\n` only, so the banner and the `Content-Length`
/// became one unparseable "line", the whole block was resynced past, and the body was
/// then read as the *next* header block and resynced past too.
///
/// Measured before the fix: `messages=0 resyncs=3` for one banner and one message.
///
/// This is the difference between "a language server prints a banner" being a
/// harmless log line and being a lost `initialize` response, which is a hang.
#[test]
fn a_bare_lf_banner_does_not_consume_the_following_message() {
    let body = br#"{"jsonrpc":"2.0"}"#;
    let mut framer = MessageFramer::new();
    let mut wire = Vec::new();
    wire.extend_from_slice(b"Starting language server...\n");
    wire.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    wire.extend_from_slice(body);
    framer.push(&wire);

    // The banner and the header arrive in one block, and the header wins.
    match framer.next_message().expect("no framing error") {
        Framed::Message(message) => assert_eq!(message, body),
        other => panic!("the message was lost to the banner prefix: {other:?}"),
    }
}

/// The same, with the banner in its own read.
///
/// Chunk boundaries are not under our control, so a banner that arrives separately
/// must resync cleanly and leave the next message intact.
#[test]
fn a_banner_in_its_own_chunk_resyncs_without_eating_the_next_message() {
    let body = br#"{"id":1}"#;
    let mut framer = MessageFramer::new();

    // A banner with a proper header terminator: this is noise, and resyncing is right.
    framer.push(b"Some notice\r\n\r\n");
    assert!(
        matches!(
            framer.next_message().expect("no error"),
            Framed::Resync { .. }
        ),
        "a header block with no Content-Length must resync"
    );

    framer.push(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    framer.push(body);
    match framer.next_message().expect("no error") {
        Framed::Message(message) => assert_eq!(message, body),
        other => panic!("expected the message, got {other:?}"),
    }
}

/// A header whose name merely *ends* with `content-length` is not the real header.
///
/// Substring scanning is what fixes the bare-LF case, and this is its cost: without a
/// boundary check, `X-Content-Length: 5` would be read as a body length of 5 and the
/// stream would desynchronise. omp's regex would accept it; a boundary check costs
/// nothing.
#[test]
fn a_header_name_ending_in_content_length_is_not_the_real_header() {
    let body = br#"{"ok":true}"#;
    let mut framer = MessageFramer::new();
    let mut wire = Vec::new();
    wire.extend_from_slice(b"X-Content-Length: 5\r\n");
    wire.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    wire.extend_from_slice(body);
    framer.push(&wire);

    match framer.next_message().expect("no error") {
        Framed::Message(message) => {
            assert_eq!(message, body, "a decoy header name was used as the length")
        }
        other => panic!("expected the message, got {other:?}"),
    }
}

/// Case and surrounding whitespace do not matter, per RFC 7230.
#[test]
fn the_length_header_is_case_and_whitespace_insensitive() {
    for header in [
        "content-length: 8\r\n\r\n",
        "CONTENT-LENGTH: 8\r\n\r\n",
        "Content-Length:8\r\n\r\n",
        "Content-Length:   8\r\n\r\n",
    ] {
        let mut framer = MessageFramer::new();
        framer.push(header.as_bytes());
        framer.push(br#"{"a":1}!"#);
        match framer.next_message().expect("no error") {
            Framed::Message(message) => assert_eq!(message.len(), 8, "for {header:?}"),
            other => panic!("{header:?} did not parse: {other:?}"),
        }
    }
}

/// The known hole: a banner containing a decoy header at a line start.
///
/// Documented on [`super::parse_content_length`] as accepted risk shared with omp. This
/// test **pins the current behaviour** rather than asserting it is right, so that
/// anyone who fixes it sees this fail and finds the reasoning instead of discovering a
/// surprise.
#[test]
fn a_decoy_header_in_a_banner_wins_the_scan_as_omp_does() {
    let mut framer = MessageFramer::new();
    let mut wire = Vec::new();
    wire.extend_from_slice(b"content-length: 5\nContent-Length: 17\r\n\r\n");
    wire.extend_from_slice(br#"{"jsonrpc":"2.0"}"#);
    framer.push(&wire);

    match framer.next_message().expect("no error") {
        Framed::Message(message) => assert_eq!(
            message.len(),
            5,
            "if this is now 17, the hole was fixed: update the doc comment on \
             parse_content_length, which records it as accepted"
        ),
        other => panic!("expected the decoy to win, got {other:?}"),
    }
}
