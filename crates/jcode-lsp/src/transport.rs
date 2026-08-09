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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::time::Duration;

use crate::framing::{encode, Framed, MessageFramer};

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
    Blocked { after: Duration },
    /// The pipe is gone, which usually means the process is too.
    Closed { source: std::io::Error },
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked { after } => write!(
                f,
                "the language server stopped reading its stdin (write blocked for {after:?})"
            ),
            Self::Closed { source } => write!(f, "writing to the language server failed: {source}"),
        }
    }
}

impl std::error::Error for WriteError {}

/// A running language server process.
pub struct Transport {
    child: Child,
    /// Behind a mutex so concurrent senders cannot interleave halves of two
    /// frames on the wire, which would desynchronise the server permanently.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// The captured stderr tail, shared with the collector task.
    stderr: Arc<Mutex<String>>,
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
        let framed = encode(body);
        let stdin = Arc::clone(&self.stdin);

        let write = async move {
            let mut guard = stdin.lock().await;
            let Some(pipe) = guard.as_mut() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "stdin already closed",
                ));
            };
            pipe.write_all(&framed).await?;
            // Flush explicitly: a buffered frame the server never sees is
            // indistinguishable from a server that never answers.
            pipe.flush().await
        };

        match tokio::time::timeout(deadline, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(WriteError::Closed { source }),
            // Note the lock is still held by the abandoned write. That is
            // deliberate: the caller's correct response to a blocked write is to
            // tear the transport down, and letting a later write queue behind a
            // wedge would just hang that one too.
            Err(_) => Err(WriteError::Blocked { after: deadline }),
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
        tokio::time::timeout(deadline, self.child.wait()).await.is_ok()
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
