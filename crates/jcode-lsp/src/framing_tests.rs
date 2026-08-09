//! Framing tests.
//!
//! These are the tests that justify not using a mock for the client: every case
//! here is a way a real pipe delivers bytes that a "read a line, then read N
//! bytes" implementation gets wrong.

use super::*;

fn frame(body: &str) -> Vec<u8> {
    encode(body.as_bytes())
}

fn take(framer: &mut MessageFramer) -> Option<String> {
    framer
        .next_message()
        .expect("framing should succeed")
        .map(|body| String::from_utf8(body).expect("utf-8"))
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

#[test]
fn a_header_block_without_content_length_is_a_fatal_error() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Type: application/json\r\n\r\n{}");
    let error = framer.next_message().expect_err("must not be silent");
    assert!(
        matches!(error, FramingError::MissingContentLength { .. }),
        "got {error:?}"
    );
    // The message must name what was seen: this is the only diagnostic the
    // caller gets before tearing the connection down.
    assert!(error.to_string().contains("Content-Type"), "{error}");
}

#[test]
fn a_non_numeric_content_length_is_a_fatal_error() {
    let mut framer = MessageFramer::new();
    framer.push(b"Content-Length: banana\r\n\r\n{}");
    let error = framer.next_message().expect_err("must not be silent");
    assert!(
        matches!(error, FramingError::InvalidContentLength { .. }),
        "got {error:?}"
    );
    assert!(error.to_string().contains("banana"), "{error}");
}

/// A corrupted length would otherwise have us wait forever for bytes that are
/// never coming, holding a reservation for the claimed size.
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
        Some(first),
        "the body must be taken by length, not by scanning for a terminator"
    );
    assert_eq!(
        framer.next_message().expect("framing"),
        Some(second),
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
