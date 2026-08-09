//! LSP message framing: `Content-Length: N\r\n\r\n<body>`.
//!
//! This is the smallest module in the crate and the one most able to break
//! everything above it, so it is written as a pure incremental parser over a
//! byte buffer and tested directly rather than through a client.
//!
//! # Why a buffer rather than reading N bytes
//!
//! The obvious implementation reads a line, parses the length, then reads
//! exactly that many bytes. It is wrong in the way that matters: a pipe
//! delivers whatever the kernel has, so one `read` can return half a header, or
//! a header plus two whole messages plus the first byte of a third. Anything
//! that assumes message boundaries align with read boundaries works on a
//! loopback test and fails against a real server under load.
//!
//! So `MessageFramer` accepts arbitrary chunks and yields whole messages when
//! and only when it has them, keeping the remainder for next time.
//!
//! # Why the length is in bytes
//!
//! `Content-Length` counts **bytes**, not characters. omp writes
//! `Buffer.byteLength(content, "utf-8")` for the same reason. A body containing
//! any non-ASCII character has a byte length larger than its character count,
//! and slicing by characters desynchronises the stream permanently: every
//! subsequent message is misframed, so the failure looks like a server that
//! went mad rather than a client that miscounted.
//!
//! # Why a bad header resyncs instead of failing
//!
//! **This was wrong in the first draft and is corrected from omp's
//! implementation, which is the specification here.** A header block with no
//! usable `Content-Length` is not a protocol violation to die on — it is
//! non-protocol noise on stdout, and it happens: a launcher shell script echoing
//! a line, a server printing a deprecation warning, a Node wrapper announcing a
//! version. Their `drain` calls an `onResync` callback, drops past the offending
//! terminator, and carries on. Treating it as fatal would tear down a healthy
//! server because something printed to the wrong stream.
//!
//! So `next_message` returns [`Framed::Resync`] for junk, letting the caller log
//! it and continue. The only genuinely fatal case left is a body larger than
//! [`MAX_BODY_BYTES`], where we would otherwise wait forever for bytes that are
//! never coming.

/// Incremental framer over a byte stream.
///
/// Feed it chunks with [`MessageFramer::push`]; take whole message bodies with
/// [`MessageFramer::next_message`].
#[derive(Debug, Default)]
pub struct MessageFramer {
    buffer: Vec<u8>,
}

/// What the framer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Framed {
    /// A whole message body.
    Message(Vec<u8>),
    /// A header block carrying no usable `Content-Length`, dropped as noise.
    ///
    /// The caller should log this and call again: there may be a real message
    /// behind it. Carrying the header text because it is the only clue about
    /// what is polluting stdout, and truncated because it could be anything.
    Resync { headers: String },
    /// Not enough bytes yet. The common case, and not an error.
    Incomplete,
}

/// A frame that cannot be recovered from.
///
/// Only one case remains fatal. A body claiming to be enormous cannot be
/// resynced past, because we would have to know where it ends to skip it, and the
/// claimed length is exactly what we do not trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramingError {
    /// A body claiming to be larger than the cap.
    BodyTooLarge { length: usize, cap: usize },
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyTooLarge { length, cap } => write!(
                f,
                "LSP frame declares a {length}-byte body, over the {cap}-byte cap"
            ),
        }
    }
}

impl std::error::Error for FramingError {}

/// Largest body we will buffer for a single message.
///
/// A cap is needed because the length is attacker-or-bug controlled: a
/// corrupted header claiming 4 GB would otherwise have us reserve it and wait
/// forever for bytes that never come. 64 MiB is far above any real LSP payload
/// (the largest in practice are `workspace/symbol` results and whole-file
/// `documentSymbol` trees, both orders of magnitude smaller) while staying
/// small enough that hitting it is a clear defect rather than a resource
/// question.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Header text kept when reporting a resync.
///
/// Bounded because the "header" is by definition not something we understand, so
/// it could be a whole minified line. omp truncates to 200 for the same reason.
const MAX_RESYNC_HEADER_CHARS: usize = 200;

const HEADER_TERMINATOR: &[u8] = b"\r\n\r\n";
const CONTENT_LENGTH: &str = "content-length";

impl MessageFramer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append received bytes. Any size, including zero, is fine.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Bytes held but not yet forming a whole message.
    ///
    /// Exposed for tests and diagnostics: a client that hangs with a non-empty
    /// buffer is waiting on a server that stopped mid-message, which is a
    /// different fault from one waiting with an empty buffer.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Take the next whole message body, if one is fully buffered.
    ///
    /// Returns [`Framed::Incomplete`] when more bytes are needed. That is the
    /// common case and is not an error: it is what "the message has not arrived
    /// yet" looks like.
    pub fn next_message(&mut self) -> Result<Framed, FramingError> {
        let Some(terminator) = find(&self.buffer, HEADER_TERMINATOR) else {
            // No complete header block yet. Note we do not cap the header
            // length: a server sending an unterminated multi-megabyte header
            // would grow this buffer, but that is indistinguishable from a slow
            // header until the terminator arrives, and real servers send two
            // headers totalling well under 100 bytes.
            return Ok(Framed::Incomplete);
        };

        let header_bytes = &self.buffer[..terminator];
        let Some(length) = parse_content_length(header_bytes) else {
            // Junk on stdout rather than a protocol violation. Drop past the
            // terminator so the next call can find a real message, instead of
            // stalling on the same noise forever.
            let headers = String::from_utf8_lossy(header_bytes);
            let headers = headers.chars().take(MAX_RESYNC_HEADER_CHARS).collect();
            self.buffer.drain(..terminator + HEADER_TERMINATOR.len());
            return Ok(Framed::Resync { headers });
        };

        if length > MAX_BODY_BYTES {
            return Err(FramingError::BodyTooLarge {
                length,
                cap: MAX_BODY_BYTES,
            });
        }

        let body_start = terminator + HEADER_TERMINATOR.len();
        let body_end = body_start + length;
        if self.buffer.len() < body_end {
            // Header complete, body still arriving. Deliberately do not consume
            // the header: re-parsing it next time costs nothing and keeps this
            // function a pure "is there a message" question with no partial
            // state to get wrong.
            return Ok(Framed::Incomplete);
        }

        let body = self.buffer[body_start..body_end].to_vec();
        // `drain` rather than a fresh allocation: several messages commonly
        // arrive in one chunk, and this keeps the tail in place for the next
        // call without copying it per message.
        self.buffer.drain(..body_end);
        Ok(Framed::Message(body))
    }
}

/// Frame a body for sending.
///
/// Byte length, not character count. See the module comment.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut framed = Vec::with_capacity(header.len() + body.len());
    framed.extend_from_slice(header.as_bytes());
    framed.extend_from_slice(body);
    framed
}

/// Parse `Content-Length` out of a header block.
///
/// Header names are case-insensitive per RFC 7230, and real servers do vary
/// (`Content-Length` and `content-length` both occur), so matching must not be
/// exact. `Content-Type` is accepted and ignored: the LSP spec defines it, some
/// servers send it, and nothing we do depends on it.
///
/// `None` means "this is not an LSP header block", which the caller resyncs past
/// rather than dying on. A non-numeric value is treated the same way as a missing
/// one: both mean we cannot locate a body, and neither is worth distinguishing to
/// a caller whose only option is to skip.
fn parse_content_length(headers: &[u8]) -> Option<usize> {
    // Headers are ASCII by spec. `from_utf8_lossy` rather than a hard error so
    // a stray byte in an otherwise-parseable header block does not lose a
    // message we could have read.
    let text = String::from_utf8_lossy(headers);
    for line in text.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(CONTENT_LENGTH) {
            continue;
        }
        return value.trim().parse::<usize>().ok();
    }
    None
}

/// First index of `needle` in `haystack`.
///
/// Hand-written rather than a dependency: the buffer is small and this is
/// called once per `next_message`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod framing_tests;
