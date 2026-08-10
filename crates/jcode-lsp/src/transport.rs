//! The transport: a spawned language server, its pipes, and the reader task.
//!
//! This layer owns exactly one thing — moving framed bytes to and from a child
//! process — and knows nothing about LSP methods. The split matters because the
//! failures here are process failures, and they are the ones that hang a turn.
//!
//! # The four ways a language server goes wrong
//!
//! Ported from what omp's regression suite covers, because none of these are
//! things you invent unprompted:
//!
//! 1. **It exits.** Bad args, a missing dylib, a version mismatch. Every pending
//!    request must be rejected, and the rejection must carry the **stderr tail**
//!    or the failure reads as a timeout with no cause. This is why stderr is
//!    captured rather than inherited.
//! 2. **Its stdout closes but the process lives.** Nothing will ever route a
//!    response again, so a client that keeps waiting hangs forever. The reader
//!    detecting EOF must tear the whole thing down, not just stop reading.
//! 3. **It stops reading its stdin.** Our writes fill the pipe buffer and then
//!    block, in the kernel, with no timeout. **This is the failure a naive
//!    implementation cannot recover from at all**, and the reason every write
//!    here takes a deadline.
//! 4. **It answers nothing.** The request is accepted and no reply comes. Handled
//!    a layer up by per-request timeouts, but the transport must not make it
//!    worse by also blocking the writer.
//!
//! # Why the reader is a task rather than a poll
//!
//! Diagnostics are *pushed*: `publishDiagnostics` arrives when the server feels
//! like it, including long after the request that provoked it. A client that only
//! reads while awaiting a response drops them, so something must always be
//! draining stdout. Hence a background task, and hence the channel out of it.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use tokio::time::Duration;

use crate::framing::{Framed, MessageFramer, encode};

/// How much stderr to keep.
///
/// Kept because a server that dies on startup explains itself there and nowhere
/// else. Bounded because a chatty server would otherwise grow this forever, and
/// the useful part of a crash is the tail. 16 KiB holds a Rust panic with a
/// backtrace, which is the longest thing worth keeping.
const MAX_STDERR_BYTES: usize = 16 * 1024;

/// What the reader task saw.
#[derive(Debug)]
pub enum FromServer {
    /// A whole message body, still unparsed.
    ///
    /// Deliberately bytes rather than a decoded message: framing has no opinion
    /// about JSON-RPC, and keeping the split means a JSON error cannot be
    /// mistaken for a transport error.
    Message(Vec<u8>),
    /// Non-protocol bytes on stdout, skipped. Worth surfacing rather than
    /// swallowing: it usually means a wrapper script is printing.
    Junk { headers: String },
    /// The reader stopped. Nothing further will arrive on this transport.
    ///
    /// `stderr` is the captured tail, which is the only explanation a server that
    /// died at startup ever gives.
    Closed { reason: String, stderr: String },
}

/// Why a write failed.
#[derive(Debug)]
pub enum WriteError {
    /// The server stopped reading its stdin and the pipe filled.
    ///
    /// **The important one.** Without a deadline this is an unrecoverable hang
    /// inside a kernel write, and the caller cannot tell it from a slow server.
    ///
    /// `partial` says whether bytes of this frame reached the pipe before the
    /// deadline. When true the stream is **desynchronised** and the transport must
    /// be torn down: see [`write_framed`].
    Blocked { after: Duration, partial: bool },
    /// The pipe is gone, which usually means the process is too.
    Closed { source: std::io::Error },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked {
                after,
                partial: true,
            } => write!(
                f,
                "the language server stopped reading its stdin (write blocked for \
                 {after:?}) and a partial frame reached the pipe, so this connection \
                 is desynchronised and must be restarted"
            ),
            Self::Blocked {
                after,
                partial: false,
            } => write!(
                f,
                "the language server stopped reading its stdin (write blocked for {after:?})"
            ),
            Self::Closed { source } => write!(f, "writing to the language server failed: {source}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl WriteError {
    /// Whether this failure left the byte stream unusable.
    ///
    /// A caller that sees `true` must not send anything else on this transport: the
    /// server is mid-frame and every later message would be misparsed.
    pub fn desynchronised(&self) -> bool {
        matches!(self, Self::Blocked { partial: true, .. })
    }
}

/// A send-only handle to a transport's stdin.
///
/// Exists so the router task can answer the server without holding a reference
/// into the `Client` that owns it. Cloning is cheap and every clone serialises
/// through the same mutex, so two writers cannot interleave a frame.
#[derive(Clone)]
pub struct Writer {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Shared with the transport, so a partial write on either path stops both.
    poisoned: Arc<AtomicBool>,
}

impl Writer {
    /// Mark the connection unusable, without writing anything.
    ///
    /// For the case where the *failure to write* is itself fatal: an unanswered server
    /// request leaves the server waiting forever, whether or not any bytes of the answer
    /// landed. Without this, a small answer that failed cleanly left the transport looking
    /// healthy and the next caller discovered otherwise at its own expense.
    pub fn poison(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
    }

    /// Send one framed message, bounded by `deadline`.
    pub async fn send(&self, body: &[u8], deadline: Duration) -> Result<(), WriteError> {
        write_framed(&self.stdin, &self.poisoned, body, deadline).await
    }
}

/// The shared write path, used by both [`Transport::send`] and [`Writer::send`].
///
/// One implementation rather than two, because the deadline handling is the part
/// that must not diverge: a second copy that forgot it would reintroduce the
/// wedged-stdin hang on whichever path used it.
///
/// # Why this writes in chunks rather than calling `write_all` once
///
/// **Found by probing, after the first version was written and tested.**
/// `AsyncWriteExt::write_all` is cancel-*unsafe* in the way that matters here: when
/// the future is dropped at a timeout, the bytes it already handed to the kernel
/// stay on the pipe. Measured directly — a 1 MiB `write_all` cancelled after 300ms
/// against a non-reading child left **65,537 bytes** in the pipe.
///
/// That is not a lost message, it is a *corrupted stream*. The server has half a
/// frame; the next frame we send is appended to it; `Content-Length` then measures
/// the wrong bytes and **every subsequent message is misframed**. The failure
/// surfaces far from its cause, as a server that appears to go mad.
///
/// So the write is done in bounded chunks and the count of bytes that reached the
/// pipe is tracked. On a timeout the caller is told whether anything landed, and if
/// it did the transport is marked unusable rather than being left to corrupt
/// silently. omp gets the same protection differently: their `LspDrainAbortError`
/// path tears the client down on an aborted drain, with a comment saying an abort
/// that raced an in-flight drain is the only case that leaves the sink pending.
/// Marker for the post-lock poison check, mapped back to the entry-refusal shape below.
///
/// Never reaches a caller: [`write_framed`] translates it.
const POISONED_WHILE_QUEUED: &str = "jcode-lsp: transport poisoned while this write queued";

async fn write_framed(
    stdin: &Arc<Mutex<Option<ChildStdin>>>,
    poisoned: &Arc<AtomicBool>,
    body: &[u8],
    deadline: Duration,
) -> Result<(), WriteError> {
    // Refuse before writing: appending to a half-written frame is exactly the
    // corruption this guard exists to prevent.
    if poisoned.load(Ordering::SeqCst) {
        return Err(WriteError::Blocked {
            after: Duration::ZERO,
            partial: true,
        });
    }

    let framed = encode(body);
    let stdin = Arc::clone(stdin);
    // Shared with the write future so the count survives its cancellation. The
    // future is dropped on timeout, so anything it owned is lost with it.
    let written = Arc::new(AtomicUsize::new(0));

    let write = {
        let written = Arc::clone(&written);
        // `framed` is moved rather than cloned: nothing else touches it after this point. It
        // was a clone, which copied the whole frame on every write for no reason -- a reviewer
        // spotted it while checking the rest of the function. Harmless at 100-byte requests and
        // not at a 20,000-section configuration answer.
        async move {
            let mut guard = stdin.lock().await;
            // **Re-checked after acquiring the lock, not only at entry.**
            //
            // The entry check happens before the mutex, so a writer that passes it and then
            // queues behind a blocked write is holding stale information: by the time it gets
            // the lock, the writer ahead of it may have timed out and poisoned the stream.
            //
            // Measured, with a writer A blocking mid-frame and a writer C arriving 50ms later:
            // C passed the entry check, waited for the lock, and then wrote its frame onto A's
            // half-frame -- paying its own full 5-second deadline and reporting
            // `Blocked { partial: false }`, with no mention of desynchronisation, against a
            // transport that was already poisoned. Two harms: C's bytes land on a half-frame,
            // so a server that is merely slow rather than dead parses garbage; and C's caller
            // is told "blocked, not partial" one call before being told "partial" by the entry
            // check on its retry -- two contradictory diagnoses of the same connection.
            //
            // `Blocked` with a zero duration rather than an error, matching the entry refusal,
            // because from the caller's side this *is* the entry refusal: it simply learned the
            // truth a moment later than it asked.
            //
            // Found by an adversarial reviewer on the sixth pass, in a function rewritten to
            // fix the fifth pass's finding.
            if poisoned.load(Ordering::SeqCst) {
                // `Other` with a marker message, recognised below. A dedicated error type for
                // the write future would be cleaner, but the future's whole point is to be a
                // plain `io` operation the timeout can wrap; a sentinel that never escapes this
                // function is the smaller change.
                return Err(std::io::Error::other(POISONED_WHILE_QUEUED));
            }
            let Some(pipe) = guard.as_mut() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "stdin already closed",
                ));
            };
            // **Byte-granular, using `write` rather than `write_all`.**
            //
            // This was `write_all` over 8 KiB chunks, with the counter bumped after each
            // chunk *completed*. A `write_all` cancelled midway through a chunk has already
            // handed the kernel every byte that fit, but the counter had not moved -- so the
            // caller was told `partial: false`, the poison flag stayed clear, and the next
            // write appended to a half-frame. Exactly the corruption the chunking was added
            // to prevent, surviving inside the chunk granularity.
            //
            // Measured: with the pipe filled, an 8 KiB frame blocked and reported
            // `partial: false` with `desynchronised == false`.
            //
            // `write` returns the count for each successful call, so the total is exact and
            // a cancellation between calls loses nothing. The loop is what `write_all` does
            // internally; the only difference is that the progress is ours to see.
            let mut offset = 0usize;
            while offset < framed.len() {
                let landed = pipe.write(&framed[offset..]).await?;
                if landed == 0 {
                    // A zero-length write on a pipe means the far end is gone. Reported as
                    // an error rather than looping, since retrying cannot make progress.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "the language server stopped accepting input",
                    ));
                }
                offset += landed;
                written.fetch_add(landed, Ordering::SeqCst);
            }
            // Flush explicitly: a buffered frame the server never sees is
            // indistinguishable from a server that never answers.
            pipe.flush().await
        }
    };

    match tokio::time::timeout(deadline, write).await {
        Ok(Ok(())) => Ok(()),
        // The post-lock poison check. Reported exactly as the entry check would have, since
        // that is what it is: the same refusal, learned after waiting for the lock.
        Ok(Err(source))
            if source.kind() == std::io::ErrorKind::Other
                && source.to_string() == POISONED_WHILE_QUEUED =>
        {
            Err(WriteError::Blocked {
                after: Duration::ZERO,
                partial: true,
            })
        }
        Ok(Err(source)) => Err(WriteError::Closed { source }),
        Err(_) => {
            // The lock is **released** here, not held.
            //
            // This comment used to claim the opposite -- that the abandoned write kept the
            // guard, so later writes would queue behind the wedge rather than proceeding.
            // That is false: the timeout drops the write future, the future owns the
            // `MutexGuard`, and dropping it releases the lock. A reviewer disproved it by
            // showing `close_stdin()` completes in under two seconds after an abandoned
            // write, where a held lock would deadlock.
            //
            // What actually gates later writes is the `poisoned` flag below. The comment
            // described a mechanism that does not exist, next to the one that does, which is
            // worse than no comment: it would have survived a refactor that removed the real
            // protection.
            //
            // Any progress at all means the server has a partial frame, because a write
            // boundary is not a frame boundary.
            let landed = written.load(Ordering::SeqCst);
            let partial = landed > 0;
            if partial {
                poisoned.store(true, Ordering::SeqCst);
            }
            Err(WriteError::Blocked {
                after: deadline,
                partial,
            })
        }
    }
}

/// A running language server process.
pub struct Transport {
    child: Child,
    /// Behind a mutex so concurrent senders cannot interleave halves of two
    /// frames on the wire, which would desynchronise the server permanently.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// The captured stderr tail, shared with the collector task.
    stderr: Arc<Mutex<String>>,
    /// Set when a partial frame reached the pipe, which makes the byte stream
    /// unusable. Shared with every `Writer`, since either path can poison it.
    poisoned: Arc<AtomicBool>,
}

impl Transport {
    /// Spawn a language server.
    ///
    /// stdout and stdin are pipes, and **stderr is a pipe too rather than
    /// inherited**. Inheriting is tempting and wrong twice over: the server's
    /// noise would land in the user's terminal mid-render, and the diagnostic we
    /// need when it dies at startup would be gone.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
        env: &[(String, String)],
    ) -> std::io::Result<(Self, mpsc::UnboundedReceiver<FromServer>)> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this the child outlives us on a hard exit, and an orphaned
            // rust-analyzer holds gigabytes with nothing to talk to.
            .kill_on_drop(true);
        for (key, value) in env {
            command.env(key, value);
        }

        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stdin = child.stdin.take().expect("stdin was piped");

        let captured = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = mpsc::unbounded_channel();

        // Two tasks: one draining stdout, one draining stderr. stderr needs its
        // own because a server that fills its stderr pipe while we are not
        // reading it blocks *the server*, which looks exactly like a hang.
        tokio::spawn(collect_stderr(stderr, Arc::clone(&captured)));
        tokio::spawn(read_stdout(stdout, tx, Arc::clone(&captured)));

        Ok((
            Self {
                child,
                stdin: Arc::new(Mutex::new(Some(stdin))),
                stderr: captured,
                poisoned: Arc::new(AtomicBool::new(false)),
            },
            rx,
        ))
    }

    /// Send one framed message, bounded by `deadline`.
    ///
    /// The deadline is not optional and not a nicety. A server that stops reading
    /// its stdin fills the pipe buffer, and the write then blocks in the kernel
    /// with no way out. Racing the write against a timer is the only thing that
    /// turns that into a reportable failure rather than a hung turn.
    pub async fn send(&self, body: &[u8], deadline: Duration) -> Result<(), WriteError> {
        write_framed(&self.stdin, &self.poisoned, body, deadline).await
    }

    /// Whether a partial frame left the byte stream unusable.
    ///
    /// A caller seeing `true` must restart the server: nothing can be sent that the
    /// server would parse correctly.
    pub fn desynchronised(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    /// A cloneable send-only handle.
    ///
    /// For the router task, which must answer the server's questions without
    /// borrowing the `Client` that owns this transport.
    pub fn writer(&self) -> Writer {
        Writer {
            stdin: Arc::clone(&self.stdin),
            poisoned: Arc::clone(&self.poisoned),
        }
    }

    /// Close the server's stdin, so it sees EOF.
    ///
    /// The polite teardown for a server that ignores `exit`, and it is what
    /// `Drop` alone would not do promptly enough to be observable.
    pub async fn close_stdin(&self) {
        self.stdin.lock().await.take();
    }

    /// The captured stderr tail.
    pub async fn stderr_tail(&self) -> String {
        self.stderr.lock().await.clone()
    }

    /// Whether the process has exited, without waiting.
    pub fn exited(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Wait for exit, up to `deadline`.
    ///
    /// Returns `false` when the process outlived it. **Callers reporting a
    /// restart must treat `false` as a failed teardown**, not a completed one:
    /// omp has a regression test for precisely that, because a server that
    /// survives its own shutdown is the daemon leak with no symptom.
    pub async fn wait_for_exit(&mut self, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, self.child.wait())
            .await
            .is_ok()
    }

    /// Kill the process and confirm it is gone.
    pub async fn kill(&mut self, deadline: Duration) -> bool {
        // `start_kill` rather than `kill`, because the latter also waits and we
        // want the confirmation to be the caller's explicit decision.
        let _ = self.child.start_kill();
        self.wait_for_exit(deadline).await
    }

    /// The process id, for diagnostics and for `initialize`.
    ///
    /// LSP servers use the client's pid to exit when their client dies. This is
    /// the *server's* pid, which is the one worth logging.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

/// Drain stdout, framing as we go, until EOF or a fatal framing error.
async fn read_stdout(
    mut stdout: tokio::process::ChildStdout,
    tx: mpsc::UnboundedSender<FromServer>,
    stderr: Arc<Mutex<String>>,
) {
    let mut framer = MessageFramer::new();
    let mut chunk = vec![0u8; 8192];

    let reason = loop {
        let read = match stdout.read(&mut chunk).await {
            // EOF. Either the server exited, or it closed stdout while alive —
            // and the second is worse, because nothing will route a response
            // again while the process looks healthy.
            Ok(0) => break "the language server closed its stdout".to_string(),
            Ok(read) => read,
            Err(error) => break format!("reading from the language server failed: {error}"),
        };
        framer.push(&chunk[..read]);

        loop {
            match framer.next_message() {
                Ok(Framed::Message(body)) => {
                    // A closed receiver means the client was dropped. Stop
                    // reading rather than looping on a dead channel.
                    if tx.send(FromServer::Message(body)).is_err() {
                        return;
                    }
                }
                Ok(Framed::Resync { headers }) => {
                    // Forwarded rather than swallowed: it is the only signal that
                    // something is printing to stdout, which is worth knowing
                    // even though it is survivable.
                    if tx.send(FromServer::Junk { headers }).is_err() {
                        return;
                    }
                }
                Ok(Framed::Incomplete) => break,
                Err(error) => {
                    let _ = tx.send(FromServer::Closed {
                        reason: format!("unrecoverable framing error: {error}"),
                        stderr: stderr.lock().await.clone(),
                    });
                    return;
                }
            }
        }
    };

    // Always announce the close, and always with the stderr tail. A silent
    // reader exit leaves every pending request waiting out its timeout with no
    // explanation, which is the "surfaces the process diagnostic" case.
    let _ = tx.send(FromServer::Closed {
        reason,
        stderr: stderr.lock().await.clone(),
    });
}

/// Drain stderr into a bounded tail.
async fn collect_stderr(mut stderr: tokio::process::ChildStderr, captured: Arc<Mutex<String>>) {
    let mut chunk = vec![0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let text = String::from_utf8_lossy(&chunk[..read]);
                let mut tail = captured.lock().await;
                tail.push_str(&text);
                // Keep the tail, not the head: a crash explains itself in its
                // last lines, and startup chatter is what would otherwise fill
                // the budget.
                if tail.len() > MAX_STDERR_BYTES {
                    let cut = tail.len() - MAX_STDERR_BYTES;
                    // Respect char boundaries, or this panics on a multi-byte
                    // character straddling the cut.
                    let cut = (cut..tail.len())
                        .find(|at| tail.is_char_boundary(*at))
                        .unwrap_or(tail.len());
                    *tail = tail[cut..].to_string();
                }
            }
        }
    }
}
