//! Waiting for diagnostics that describe the content we just sent.
//!
//! # Why this is not "read the cache"
//!
//! Diagnostics are pushed, asynchronously, and a server publishes them whenever it
//! finishes analysing — which may be before it has seen your edit. So the obvious
//! implementation, "sync the file then read the cache", returns diagnostics for the
//! **previous** content. That is worse than returning nothing, because it looks
//! authoritative: the model is told its edit introduced no errors when the errors
//! simply have not been computed yet.
//!
//! omp's `waitForDiagnostics` exists to solve this and their freshness suite is
//! the specification. Two mechanisms, because servers differ:
//!
//! 1. **Version matching.** A server that echoes the document version in its
//!    publish can be believed immediately: a publish for version 4 describes
//!    version 4. This is the reliable path and the reason our client capabilities
//!    advertise `versionSupport`.
//!
//! 2. **Settling on quiescence.** Many servers never echo a version. For those,
//!    freshness cannot be established by matching, so instead we watch the stream
//!    go quiet: take the latest publish, and accept it once nothing newer has
//!    arrived for `settle`. A publish for the pre-edit content is usually
//!    superseded within milliseconds, so the settle window is what lets the real
//!    one overtake it.
//!
//! The second is a heuristic and is deliberately so. There is no protocol-level
//! way to tell a stale unversioned publish from a fresh one, and omp's test for it
//! is explicitly about a stale publish at +10ms being replaced by a real one at
//! +150ms.
//!
//! # The URI trap
//!
//! A server may publish under a **differently spelled** URI for the same file —
//! percent-encoded where we sent raw, or a different drive-letter case on Windows.
//! Matching URIs as strings then misses the publish entirely and the wait times
//! out on a server that answered correctly. Comparison goes through
//! [`equivalent_uris`].

use std::time::Duration;

use serde_json::Value;

/// How often to re-check while waiting.
///
/// Polling rather than a notification because the alternative is a channel per
/// waiter, and the wait is bounded in hundreds of milliseconds: at 10ms the
/// overhead is invisible and the latency cost is a rounding error.
pub const POLL: Duration = Duration::from_millis(10);

/// How long the stream must be quiet before an unversioned publish is trusted.
///
/// omp uses 250ms. Their stale-publish test has the bad one at +10ms and the real
/// one at +150ms, so the window has to exceed the gap between a server's first
/// guess and its considered answer. Shorter risks the stale one; much longer is
/// latency the user waits through on every diagnostics call.
pub const SETTLE: Duration = Duration::from_millis(250);

/// What we are waiting for.
#[derive(Debug, Clone)]
pub struct FreshnessRequest {
    /// The document version we expect described, when we know it.
    ///
    /// `Some` enables the reliable path. `None` means version matching is
    /// impossible and only quiescence can decide.
    pub expected_version: Option<i64>,
    /// How long the stream must be quiet to accept an unversioned publish.
    pub settle: Duration,
    /// Total budget.
    pub timeout: Duration,
}

impl Default for FreshnessRequest {
    fn default() -> Self {
        Self {
            expected_version: None,
            settle: SETTLE,
            // 3 seconds, matching omp's single-file wait. Long enough for a warm
            // server to answer, short enough that a server which will never answer
            // does not hold the turn.
            timeout: Duration::from_secs(3),
        }
    }
}

/// Why a wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The publish matched the version we asked about. Authoritative.
    VersionMatched,
    /// Unversioned, and the stream went quiet. A heuristic, and the common case.
    Settled,
    /// The budget expired with a real publish in hand, but it never settled.
    ///
    /// Usable: there *are* diagnostics, they just cannot be called authoritative. omp
    /// returns the cached publish in this case rather than discarding it, and so should a
    /// caller here -- reporting them with a caveat beats reporting nothing.
    TimedOutWithPublish,
    /// The budget expired and nothing was ever published.
    ///
    /// **Not the same as "no problems".** A caller must not report this as a clean file:
    /// the distinction between "the server says it is clean" and "the server did not
    /// answer" is the whole point of this module.
    ///
    /// # Why this is two variants and not one
    ///
    /// It was a single `TimedOut`, which conflated the two cases above -- and they call for
    /// opposite behaviour. A cautious caller refusing to report anything on a timeout
    /// throws away a real publish; a careless one reports an empty result as a clean file.
    /// Neither could do better, because the label did not carry which case it was, and the
    /// only way to tell was to inspect the observation the caller had just been handed and
    /// re-derive it.
    ///
    /// The reviewer raised this as "TimedOut discards the cached publish where omp returns
    /// it" and called it a judgement call. It is, but the judgement was unmakeable by the
    /// caller, which is the part worth fixing.
    TimedOutWithNothing,
}

/// One observation of the diagnostics cache.
///
/// Modelled as a plain input so the decision logic is pure and testable without a
/// server, a clock, or a task. The polling loop is the only part that needs those,
/// and it is trivial once the decision is separable.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// The diagnostics currently cached for the URI, if any.
    pub diagnostics: Option<Vec<Value>>,
    /// The version the cached publish claimed, if it claimed one.
    pub version: Option<i64>,
    /// A counter that changes whenever *any* publish arrives, for any URI.
    ///
    /// Needed because "the same diagnostics" and "a fresh publish of the same
    /// diagnostics" are different events, and only the second means the server has
    /// re-analysed. Comparing the diagnostics themselves cannot tell them apart:
    /// an unchanged file republishes an identical list.
    pub generation: u64,
}

/// The decision, given what we can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Accept what is cached.
    Accept(Freshness),
    /// Keep waiting.
    Wait,
}

/// Tracks a wait in progress.
///
/// Deliberately holds no clock and no cache: it is fed observations and elapsed
/// time, so every branch is reachable from a test without sleeping.
#[derive(Debug)]
pub struct FreshnessWait {
    request: FreshnessRequest,
    /// The generation of the publish we are currently considering, and when we
    /// first saw it.
    settling: Option<(u64, Duration)>,
}

impl FreshnessWait {
    pub fn new(request: FreshnessRequest) -> Self {
        Self {
            request,
            settling: None,
        }
    }

    /// Decide, given an observation and how long we have been waiting.
    pub fn observe(&mut self, observation: &Observation, elapsed: Duration) -> Decision {
        let Some(_) = observation.diagnostics.as_ref() else {
            // Nothing published yet. Note we do not start settling: there is
            // nothing to settle on, and treating "absent" as "quiet" would accept
            // an empty result as a clean file.
            return if elapsed >= self.request.timeout {
                Decision::Accept(Freshness::TimedOutWithNothing)
            } else {
                Decision::Wait
            };
        };

        // The reliable path. A publish that names our version describes our
        // content, so there is nothing to wait for.
        if let (Some(expected), Some(published)) =
            (self.request.expected_version, observation.version)
            && published == expected
        {
            return Decision::Accept(Freshness::VersionMatched);
        }

        // A publish for a version *older* than the one we sent is definitively
        // stale, and saying so is better than settling on it. Only meaningful when
        // both versions are known.
        let definitely_stale = matches!(
            (self.request.expected_version, observation.version),
            (Some(expected), Some(published)) if published < expected
        );

        match self.settling {
            // A new publish. Restart the settle window: this is what lets a real
            // answer at +150ms supersede a stale one at +10ms.
            Some((generation, _)) if generation != observation.generation => {
                self.settling = Some((observation.generation, elapsed));
            }
            None => {
                self.settling = Some((observation.generation, elapsed));
            }
            // Same publish as last time. The window continues.
            Some(_) => {}
        }

        if elapsed >= self.request.timeout {
            // Out of budget, but a publish is in hand. Reporting it as settled would
            // overstate it, and discarding it would waste a real answer, so the label says
            // exactly what happened and lets the caller decide.
            return Decision::Accept(Freshness::TimedOutWithPublish);
        }

        if definitely_stale {
            // Known stale: keep waiting for the real one rather than settling on
            // something we can prove describes old content.
            return Decision::Wait;
        }

        let quiet_since = self.settling.map(|(_, at)| at).unwrap_or(elapsed);
        if elapsed.saturating_sub(quiet_since) >= self.request.settle {
            Decision::Accept(Freshness::Settled)
        } else {
            Decision::Wait
        }
    }
}

/// Whether two URIs name the same file.
///
/// A server may echo a differently spelled URI for the file we sent: percent-encoded
/// where we sent raw, a different drive-letter case on Windows, or a path spelled
/// with redundant segments. String equality then misses the publish and the wait
/// times out against a server that answered perfectly well.
///
/// # What normalization happens, and a correction
///
/// Decodes percent-escapes, folds a Windows drive letter, and normalizes the path
/// **lexically**: collapsing `//`, dropping `.`, and resolving `..` against the
/// preceding segment.
///
/// The lexical pass was missing, and the doc comment here previously justified its
/// absence by saying that resolving `..` "would be slower and would let two genuinely
/// different files compare equal". That conflated two different operations. omp calls
/// `path.normalize`, which is pure string arithmetic: no `stat`, no symlink
/// resolution, so neither objection applies. What I described avoiding was
/// canonicalization, which omp does not do either.
///
/// Measured: `file:///a/b.rs` against `file:///a/./b.rs`, `file:///a//b.rs`, and
/// `file:///a/c/../b.rs` — omp's key function called all three equal, this function
/// called all three different. A missed publish means the freshness wait times out and
/// the caller reports no diagnostics for a file the server had already analysed.
///
/// Symlinks are still deliberately not resolved. `a/../b` and `b` are the same file by
/// the *spelling* of the path, which is all a lexical pass claims; whether `a` is a
/// symlink would change the answer, but that needs the filesystem and omp does not
/// consult it either. Following omp here is also the safer direction: a wrong `..`
/// resolution through a symlink would compare two different files equal, and this way
/// we can only ever be as wrong as they are.
pub fn equivalent_uris(ours: &str, theirs: &str) -> bool {
    if ours == theirs {
        return true;
    }
    normalize_uri(ours) == normalize_uri(theirs)
}

/// The canonical form of a URI, for use as a map key.
///
/// `pub(crate)` so the client can key its diagnostics map by it. See
/// [`equivalent_uris`] for what normalization happens and why.
pub(crate) fn normalize_uri(uri: &str) -> String {
    let decoded = percent_decode(uri);
    // Windows drive letters differ in case between clients and servers, and
    // `file:///C:/x` and `file:///c:/x` are the same file. Only the drive letter
    // is folded: the rest of the path is case-sensitive on the platforms we care
    // about, and folding it would make two distinct files compare equal.
    if let Some(rest) = decoded.strip_prefix("file:///")
        && let Some((drive, tail)) = split_drive(rest)
    {
        return format!(
            "file:///{}{}",
            drive.to_ascii_lowercase(),
            lexically_normalize(tail)
        );
    }
    match decoded.strip_prefix("file://") {
        Some(path) => format!("file://{}", lexically_normalize(path)),
        None => decoded,
    }
}

/// Collapse `//`, drop `.`, and resolve `..` without touching the filesystem.
///
/// Matches Node's `path.normalize`, which is what omp keys its URI map by. A leading
/// `..` is kept rather than discarded: it cannot be resolved without knowing what it
/// is relative to, and dropping it would make `../a` and `a` the same file.
fn lexically_normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            // Empty from `//` or a leading/trailing slash; `.` adds nothing.
            "" | "." => {}
            ".." => {
                // Pop only a real segment. At the root, `..` has nowhere to go and is
                // dropped, as `path.normalize` does; in a relative path it is kept,
                // because there is nothing yet to cancel against.
                match segments.last() {
                    Some(&last) if last != ".." => {
                        segments.pop();
                    }
                    _ if absolute => {}
                    _ => segments.push(".."),
                }
            }
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    // Joining the surviving segments already drops trailing and repeated slashes,
    // since both produce empty segments that are skipped above. An explicit
    // "pop a trailing slash" step was here and was **dead code**: a mutation deleting
    // it changed no output for any input, including "/a/", "//" and "/a//". Removed
    // rather than kept as reassurance.
    //
    // This diverges from `path.normalize`, which preserves a trailing slash ("/a/"
    // stays "/a/"). Deliberate: `file:///a/` and `file:///a` name the same directory,
    // and this function's only question is whether two URIs mean the same thing.
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Split a leading `C:` off a path, if there is one.
fn split_drive(path: &str) -> Option<(&str, &str)> {
    let mut chars = path.char_indices();
    let (_, letter) = chars.next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let (colon_at, colon) = chars.next()?;
    if colon != ':' {
        return None;
    }
    Some((&path[..colon_at], &path[colon_at..]))
}

/// Decode `%XX` escapes.
///
/// Hand-written rather than a dependency: this is the only place the crate needs
/// it, and an invalid escape must be left alone rather than rejected, which most
/// libraries will not do.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    // Lossy rather than failing: a URI with invalid UTF-8 after decoding is a
    // server bug, and comparing it as replacement characters is still better than
    // refusing to compare at all.
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod freshness_tests;
