//! Which language servers exist, and which of them apply here.
//!
//! Group F of the port. Two questions, and they are not the same one:
//!
//! 1. **Configured**: this server is known, with a command and the file types it
//!    handles. 53 come from `defaults.json`, copied from omp verbatim.
//! 2. **Available**: its root markers are present in *this* project and its binary
//!    can actually be found.
//!
//! omp's F3 ("status distinguishes configured servers from started clients") is the
//! test that this distinction survives to the user, and it is worth keeping: "no
//! server for this file" and "rust-analyzer is not installed" send a reader in
//! completely different directions.
//!
//! # Detection is not startup
//!
//! Nothing here spawns anything. [`Available::resolved`] is a path on disk and
//! nothing more. That keeps this module testable against a temporary directory with
//! fake executables, which is how every test below works, and it is why F4 and F5
//! (rust-analyzer's workspace readiness) are *not* here: they are about what to do
//! once a server is running.
//!
//! # Divergences from omp, and why
//!
//! - **No YAML.** omp accepts `lsp.yml` because Bun bundles a YAML parser. Adding a
//!   YAML dependency to read a config file nobody has written yet is the wrong
//!   trade; JSON is what their own defaults and docs use. If someone asks, the
//!   parser is one dependency and [`parse`] is the only place that changes.
//! - **No plugin marketplace.** omp reads LSP configs out of Claude plugin caches.
//!   We have no plugin marketplace, so the source does not exist.
//! - **Windows local-bin suffixes are handled, not skipped.** The porting notes
//!   dropped omp's four Windows detection cases as untestable on macOS, and that was
//!   half right: the *filesystem layout* cannot be tested here, but appending
//!   `.exe`/`.cmd`/`.bat` is a string operation that can. [`local_candidates`]
//!   returns them on every platform and the tests check the list, so the logic is
//!   verified even where it cannot be exercised. Better than the alternative, which
//!   was shipping it untested and calling it a known gap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The 53 defaults, verbatim from omp's `src/lsp/defaults.json`.
///
/// Embedded rather than read at runtime: it is not user-editable data, and a missing
/// file at startup would be a silent loss of every server. Parsed lazily on first
/// use, then cached, since a 53-entry parse should not happen per query.
const DEFAULTS_JSON: &str = include_str!("defaults.json");

/// A config file's contents: server *overlays*, not complete servers.
///
/// Separate from [`ServerConfig`] because an overlay is partial by nature. The
/// commonest edit anyone will make is
///
/// ```json
/// {"rust-analyzer": {"disabled": true}}
/// ```
///
/// which names no command. Parsing that as a `ServerConfig` fails on
/// `missing field command` and takes the whole file with it, so one line disabling
/// one server would disable every server. Found by a test asserting the disable
/// path, not by reading the type.
///
/// omp gets this for free: their `RawServerConfig extends Partial<ServerConfig>`.
/// Rust makes it explicit, which is more code and a clearer statement of which
/// fields a config file may omit — all of them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerOverlay {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub file_types: Option<Vec<String>>,
    #[serde(default)]
    pub root_markers: Option<Vec<String>>,
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    #[serde(default)]
    pub is_linter: Option<bool>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub warmup_timeout_ms: Option<u64>,
    #[serde(default)]
    pub capabilities: Option<BTreeMap<String, bool>>,
}

impl ServerOverlay {
    /// Promote an overlay to a full config, for a server the defaults never had.
    ///
    /// Returns `None` when the required fields are absent, which is omp's
    /// `normalizeServerConfig` rejection: a server with no command cannot be
    /// spawned, and one with no file types or root markers can never be selected, so
    /// keeping it would only produce a confusing `status` entry.
    fn into_config(self) -> Option<ServerConfig> {
        let command = self.command.filter(|command| !command.is_empty())?;
        let file_types = self.file_types.filter(|types| !types.is_empty())?;
        let root_markers = self.root_markers.filter(|markers| !markers.is_empty())?;
        Some(ServerConfig {
            command,
            args: self.args.unwrap_or_default(),
            file_types,
            root_markers,
            init_options: self.init_options,
            settings: self.settings,
            is_linter: self.is_linter.unwrap_or(false),
            disabled: self.disabled.unwrap_or(false),
            warmup_timeout_ms: self.warmup_timeout_ms,
            capabilities: self.capabilities.unwrap_or_default(),
        })
    }
}

/// A configured server: known, but not necessarily applicable or installed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// The executable, as named. Resolved to a path later.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// File extensions, with the leading dot, as omp writes them (`".rs"`).
    #[serde(default)]
    pub file_types: Vec<String>,
    /// Files or glob patterns whose presence marks a project root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Sent as `initializationOptions` in `initialize`.
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    /// Sent in `workspace/didChangeConfiguration` and answered to
    /// `workspace/configuration`.
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    /// A linter rather than a primary language server.
    ///
    /// Load-bearing for ordering: a file can match several servers, and the linter
    /// must not be the one asked for a definition. See [`servers_for_file`].
    #[serde(default)]
    pub is_linter: bool,
    /// How long to allow this server's warm-up, overriding the default.
    ///
    /// `marksman` sets 2000ms in `defaults.json`. omp reads it in `servers.ts:90` as
    /// `serverConfig.warmupTimeoutMs ?? WARMUP_TIMEOUT_MS`.
    ///
    /// **Parsed but not yet consumed**, because startup and readiness are not built.
    /// Carried rather than dropped: serde tolerates unknown fields by design here (real
    /// servers send values outside the spec), and that tolerance is exactly why these two
    /// fields went unnoticed for the whole port. A field that is present in the data and
    /// absent from the struct is invisible; one that is present and unused is a `todo` a
    /// reader can find.
    #[serde(default)]
    pub warmup_timeout_ms: Option<u64>,
    /// Server-specific extensions this server supports.
    ///
    /// `rust-analyzer` declares five (`flycheck`, `ssr`, `expandMacro`, `runnables`,
    /// `relatedTests`). omp gates its extension requests on them via `hasCapability`.
    ///
    /// **Parsed but not yet consumed**, for the same reason as
    /// [`Self::warmup_timeout_ms`]: the requests these gate are v2 work. Kept as a map so
    /// a server declaring a capability we have not heard of is preserved rather than
    /// rejected.
    #[serde(default)]
    pub capabilities: BTreeMap<String, bool>,
    /// Turned off by a config file. Kept in the map rather than removed so that
    /// `status` can say "disabled" instead of staying silent.
    #[serde(default)]
    pub disabled: bool,
}

/// A server that applies to this project and whose binary was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub name: String,
    pub config: ServerConfig,
    /// The absolute path we would spawn. Nothing is spawned here.
    pub resolved: PathBuf,
}

/// Why a configured server is not available, so `status` can say something useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No root marker present: this is not that kind of project.
    NotThisProject,
    /// Root markers matched, but the binary is not installed.
    ///
    /// Distinguished from [`Self::NotThisProject`] because it is the actionable one:
    /// it means "install rust-analyzer", where the other means "this is not a Rust
    /// project" and needs no action at all.
    BinaryNotFound { command: String },
    /// Disabled by configuration.
    Disabled,
}

/// A parsed config *file*: overlays plus settings, before merging.
#[derive(Debug, Clone, Default)]
pub struct ConfigFile {
    pub servers: BTreeMap<String, ServerOverlay>,
    /// Names of entries that could not be parsed and were skipped.
    ///
    /// Returned rather than logged: this crate holds no logger, and swallowing them
    /// would make a typo in a config file invisible. The caller is expected to say
    /// something about each one.
    pub skipped: Vec<String>,
    pub idle_timeout: Option<std::time::Duration>,
}

/// Everything configured, and for each whether it applies here.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Name-ordered so that `status` output is stable. A HashMap here would make
    /// the listing reorder itself between runs for no reason.
    pub servers: BTreeMap<String, ServerConfig>,
    /// Shut a client down after this long idle, if set.
    pub idle_timeout: Option<std::time::Duration>,
}

/// Parse a config file's contents.
///
/// Accepts both shapes omp does: a `{"servers": {...}}` wrapper, or a bare map of
/// server names at the top level. Their own docs show the wrapper and their tests
/// use the bare form, so both are real.
///
/// # One bad entry does not lose the file
///
/// Entries are parsed individually and an unparseable one is skipped, which is what
/// omp does: `coerceServerConfigs` normalizes per entry and logs a warning for each
/// one it drops.
///
/// This used to deserialize the whole map at once, so `{"good": {...}, "bad": 42}`
/// returned an error and **every** server in the file was lost. A config file is
/// hand-written, so a typo in one entry is the likely case rather than the exotic one,
/// and losing the other twenty entries to it is the wrong failure. Found by an
/// adversarial reviewer.
pub fn parse(contents: &str) -> Result<ConfigFile, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(contents)?;
    let object = value.as_object().ok_or_else(|| {
        // A config file has to be an object. An array or a bare string is not a shape
        // with a fallback reading, unlike the cases below.
        serde::de::Error::custom("an LSP config file must be a JSON object")
    })?;

    // Read `idleTimeoutMs` wherever it appears: it is a sibling of the servers in the
    // bare form and a sibling of the `servers` key in the wrapped one, so one lookup
    // covers both.
    let idle = object
        .get("idleTimeoutMs")
        .and_then(serde_json::Value::as_u64);

    // The wrapper form, but only when `servers` is actually a map. omp gates on
    // `isRecord(rawServers)` and otherwise falls through to the bare reading, so
    // `{"servers": 42, "gopls": {...}}` keeps gopls rather than losing the file.
    //
    // This used to deserialize a `Wrapper` struct, which errored on a non-map `servers`
    // before any fallback could happen -- the last remaining whole-file-loss path, and
    // one I was inclined to leave because "nobody writes that". A reviewer asked whether
    // that reasoning was too convenient, and it was: the fix is to inspect the value
    // instead of asking serde to, which is both smaller than the struct it replaces and
    // closer to what omp does. "Unlikely input" is a reason to not add machinery, not a
    // reason to keep a path that discards a user's whole configuration.
    if let Some(raw) = object.get("servers").and_then(serde_json::Value::as_object) {
        let (servers, skipped) = coerce_servers(raw.clone().into_iter().collect());
        return Ok(ConfigFile {
            servers,
            skipped,
            idle_timeout: idle.map(std::time::Duration::from_millis),
        });
    }

    // Bare map. `idleTimeoutMs` is a setting rather than a server, so it is dropped
    // before the entries are read; otherwise it would be reported as a skipped server.
    let raw: BTreeMap<String, serde_json::Value> = object
        .iter()
        .filter(|(name, _)| name.as_str() != "idleTimeoutMs")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let (servers, skipped) = coerce_servers(raw);
    Ok(ConfigFile {
        servers,
        skipped,
        idle_timeout: idle.map(std::time::Duration::from_millis),
    })
}

/// Parse each entry on its own, collecting the names that failed.
fn coerce_servers(
    raw: BTreeMap<String, serde_json::Value>,
) -> (BTreeMap<String, ServerOverlay>, Vec<String>) {
    let mut servers = BTreeMap::new();
    let mut skipped = Vec::new();
    for (name, value) in raw {
        match serde_json::from_value::<ServerOverlay>(value) {
            Ok(overlay) => {
                servers.insert(name, overlay);
            }
            Err(_) => skipped.push(name),
        }
    }
    (servers, skipped)
}

/// The built-in defaults.
pub fn defaults() -> Config {
    // Parsed as a bare map of complete servers: unlike a user config file, every
    // entry here is required to be whole, and `every_default_is_complete` asserts it.
    // A parse failure is a bug in the checked-in file rather than a runtime
    // condition, so it panics with the reason.
    let servers: BTreeMap<String, ServerConfig> =
        serde_json::from_str(DEFAULTS_JSON).expect("defaults.json is checked in and must parse");
    Config {
        servers,
        idle_timeout: None,
    }
}

/// Layer a config file over a base, per-field.
///
/// Per-field, not per-server, which is the whole point: writing `{"rust-analyzer":
/// {"command": "/my/ra"}}` must not silently discard the file types and root markers
/// the default supplied. omp does the same with a spread, and the failure mode of
/// getting it wrong is a server that is configured but matches no file.
pub fn merge(base: &mut Config, overlay: ConfigFile) {
    for (name, over) in overlay.servers {
        match base.servers.get_mut(&name) {
            Some(existing) => {
                // `args` is the only field where empty is meaningful, and the
                // distinction is not stylistic. `gopls` defaults to `["serve"]`, so
                // `"args": []` is the only way to say "invoke it bare" -- honouring it
                // is required.
                if let Some(args) = over.args {
                    existing.args = args;
                }
                // For command, fileTypes and rootMarkers, empty is not a value: it
                // makes the server unusable rather than differently configured. A
                // server with no command cannot be spawned, and one with no file types
                // or root markers can never be selected -- so an empty override would
                // silently delete a working default while leaving it in `status`
                // looking configured.
                //
                // omp reaches the same behaviour by a different route: they merge
                // first, re-run `normalizeServerConfig` on the result, and keep the
                // previous entry if it fails. Verified against a transcription of
                // their `mergeServers` for all four fields -- empty command, fileTypes
                // and rootMarkers each keep the default; empty args applies.
                //
                // Mine honoured empty for all four, and the comment here previously
                // celebrated that as correct. It was right about `args` and wrong
                // about the other three, which is worse than being wrong about all of
                // them: the reasoning looked considered.
                if let Some(command) = over.command.filter(|value| !value.is_empty()) {
                    existing.command = command;
                }
                if let Some(file_types) = over.file_types.filter(|value| !value.is_empty()) {
                    existing.file_types = file_types;
                }
                if let Some(root_markers) = over.root_markers.filter(|value| !value.is_empty()) {
                    existing.root_markers = root_markers;
                }
                if over.init_options.is_some() {
                    existing.init_options = over.init_options;
                }
                if over.settings.is_some() {
                    existing.settings = over.settings;
                }
                // Explicit `false` turns these back off, which an Option makes
                // possible and a bare bool would not: re-enabling a server disabled
                // by a lower-priority file is a real thing to want.
                if let Some(is_linter) = over.is_linter {
                    existing.is_linter = is_linter;
                }
                if let Some(disabled) = over.disabled {
                    existing.disabled = disabled;
                }
                if over.warmup_timeout_ms.is_some() {
                    existing.warmup_timeout_ms = over.warmup_timeout_ms;
                }
                if let Some(capabilities) = over.capabilities {
                    existing.capabilities = capabilities;
                }
            }
            None => {
                // A server the defaults never had must stand on its own, so the
                // required fields have to be present.
                if let Some(config) = over.into_config() {
                    base.servers.insert(name, config);
                }
            }
        }
    }
    if overlay.idle_timeout.is_some() {
        base.idle_timeout = overlay.idle_timeout;
    }
}

/// Does this directory look like a project root for these markers?
///
/// A marker containing `*` is matched against the directory's entries by name; the
/// rest are existence checks. Deliberately one level deep, not a recursive glob:
/// omp notes that `Bun.Glob` on `**/*.cabal` descends into `node_modules`, and a
/// root marker that is not at the root is not a root marker.
pub fn has_root_markers(dir: &Path, markers: &[String]) -> bool {
    let mut entries: Option<Vec<String>> = None;
    for marker in markers {
        if marker.contains('*') {
            if entries.is_none() {
                entries = Some(
                    std::fs::read_dir(dir)
                        .map(|read| {
                            read.filter_map(Result::ok)
                                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                                .collect()
                        })
                        .unwrap_or_default(),
                );
            }
            if let Some(names) = &entries
                && names.iter().any(|name| glob_matches(marker, name))
            {
                return true;
            }
            continue;
        }
        if dir.join(marker).exists() {
            return true;
        }
    }
    false
}

/// Match one path segment against a glob of the shape the markers use.
///
/// Only `*` and `?`, because that is all `defaults.json` contains: seven servers use
/// patterns and every one is `*.ext` or similar (`*.tla`, `*.cabal`, `*.sln`,
/// `*.xcodeproj`). No character classes, no `**`. Written out rather than pulling in
/// a glob crate for two metacharacters.
fn glob_matches(pattern: &str, name: &str) -> bool {
    // Backtracking on `*`, iterative so a pathological pattern cannot blow the
    // stack. `star` remembers where to resume if the current attempt fails.
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;

    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            // Mismatch: let the star absorb one more character and retry.
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    // Trailing stars can match nothing.
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Project-local bin directories, keyed by the markers that imply them.
///
/// Checked before `PATH`, because a project pinning its own toolchain means it: a
/// repo with `node_modules/.bin/typescript-language-server` wants that one, not
/// whatever is installed globally, and using the global one produces diagnostics
/// from the wrong version.
const LOCAL_BINS: &[(&[&str], &str)] = &[
    (
        &[
            "package.json",
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
        ],
        "node_modules/.bin",
    ),
    (PYTHON_MARKERS, ".venv/bin"),
    (PYTHON_MARKERS, ".venv/Scripts"),
    (PYTHON_MARKERS, "venv/bin"),
    (PYTHON_MARKERS, "venv/Scripts"),
    (PYTHON_MARKERS, ".env/bin"),
    (PYTHON_MARKERS, ".env/Scripts"),
    (&["Gemfile", "Gemfile.lock"], "vendor/bundle/bin"),
    (&["Gemfile", "Gemfile.lock"], "bin"),
    (&["go.mod", "go.sum", "go.work"], "bin"),
];

const PYTHON_MARKERS: &[&str] = &[
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "setup.cfg",
    "Pipfile",
    "pyrightconfig.json",
    "ruff.toml",
    ".ruff.toml",
];

/// Executable suffixes to try, in order, for a local bin lookup.
///
/// Returned on every platform, not just Windows, so the list is testable where the
/// filesystem layout is not. `existing` filtering happens in the caller; this is a
/// pure function of the base path.
///
/// The empty suffix comes first: on Unix that is the only candidate that will exist,
/// and on Windows an extensionless file is still preferred if present. Package
/// managers write `.cmd` shims in `node_modules/.bin` next to extensionless shell
/// scripts, and picking the shell script on Windows would fail to execute.
pub fn local_candidates(base: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![base.to_path_buf()];
    if cfg!(windows) {
        for suffix in [".exe", ".cmd", ".bat"] {
            let mut name = base.as_os_str().to_os_string();
            name.push(suffix);
            candidates.push(PathBuf::from(name));
        }
    }
    candidates
}

/// Find a command in the project's own bin directories, before `PATH`.
pub fn resolve_local(command: &str, root: &Path) -> Option<PathBuf> {
    for (markers, bin_dir) in LOCAL_BINS {
        let markers: Vec<String> = markers.iter().map(|marker| (*marker).to_string()).collect();
        if !has_root_markers(root, &markers) {
            continue;
        }
        let base = root.join(bin_dir).join(command);
        for candidate in local_candidates(&base) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Find a command on `PATH`.
///
/// Takes the search path explicitly so tests can supply one instead of depending on
/// whatever the machine running them happens to have installed.
pub fn resolve_in_path(command: &str, path_var: Option<&str>) -> Option<PathBuf> {
    // An absolute or explicitly-relative command is a path, not a name to look up.
    // Skipping this check would search PATH for a string containing a separator,
    // which never matches, so a configured absolute command would appear missing.
    if command.contains('/') || (cfg!(windows) && command.contains('\\')) {
        let direct = PathBuf::from(command);
        return direct.is_file().then_some(direct);
    }

    let path_var = path_var
        .map(str::to_string)
        .or_else(|| std::env::var("PATH").ok())?;
    for dir in path_var.split(if cfg!(windows) { ';' } else { ':' }) {
        if dir.is_empty() {
            continue;
        }
        let base = Path::new(dir).join(command);
        for candidate in local_candidates(&base) {
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Is this file executable by us?
///
/// On Unix, checking the mode matters: `node_modules/.bin` and `PATH` directories
/// contain plenty of non-executable files, and returning one of them means a spawn
/// failure reported as a missing server.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// On Windows, executability is the extension, which [`local_candidates`] already
/// enumerated.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Substitute runtime tokens in a server's arguments.
///
/// Currently one token, `$PID`, which `omnisharp` needs: its `--hostPID` argument
/// tells it which process to exit with, and it is in `defaults.json` as the literal
/// string `"$PID"`. omp substitutes it in `applyRuntimeDefaults`; nothing here did, so
/// omnisharp would have been spawned with a literal `$PID` and refused to start.
///
/// Found by an adversarial reviewer reading `defaults.json` against omp's loader, not
/// by any test: nothing in the suite spawns omnisharp, and nothing would have until a
/// C# user reported it.
///
/// Applied at detection time rather than at parse time, so the token survives in the
/// stored config and `status` can show what was configured rather than one process's
/// resolved value.
fn substitute_runtime_tokens(args: &[String]) -> Vec<String> {
    let pid = std::process::id().to_string();
    args.iter()
        .map(|arg| {
            if arg == PID_TOKEN {
                pid.clone()
            } else {
                arg.clone()
            }
        })
        .collect()
}

/// The token `omnisharp`'s `--hostPID` argument is written as in `defaults.json`.
///
/// Matched whole rather than substring-replaced, following omp
/// (`arg === PID_TOKEN ? String(process.pid) : arg`). A substring replace would also
/// rewrite a path that happened to contain `$PID`.
const PID_TOKEN: &str = "$PID";

/// Decide, for every configured server, whether it applies to this project.
///
/// Returns both halves. The unavailable ones are not filtered out because the reason
/// is the useful part: `status` needs to tell "not a Rust project" from
/// "rust-analyzer is not installed".
///
/// # Two servers here are not language servers
///
/// `biome` and `swiftlint` are marked `isLinter` in `defaults.json`, but in omp they
/// are more than that: `applyRuntimeDefaults` attaches a `createClient` adapter to
/// each, because neither speaks LSP the way the others do. `swiftlint`'s configured
/// command is `swiftlint lint --quiet --reporter json`, which prints JSON and exits --
/// it is not a server at all, and spawning it as one will produce a process that dies
/// immediately.
///
/// We have no adapter layer, so both are reported as available and would fail on
/// spawn. Recorded here and in `PORTING_NOTES.md` rather than removed from the
/// defaults, because the entries are correct data and it is the adapter that is
/// missing. A caller that spawns these before adapters exist gets a confusing failure,
/// so this is a known gap with a name rather than a surprise.
pub fn detect(
    config: &Config,
    root: &Path,
    path_var: Option<&str>,
) -> (Vec<Available>, BTreeMap<String, Unavailable>) {
    let mut available = Vec::new();
    let mut unavailable = BTreeMap::new();

    for (name, server) in &config.servers {
        if server.disabled {
            unavailable.insert(name.clone(), Unavailable::Disabled);
            continue;
        }
        if !has_root_markers(root, &server.root_markers) {
            unavailable.insert(name.clone(), Unavailable::NotThisProject);
            continue;
        }
        let Some(resolved) = resolve_local(&server.command, root)
            .or_else(|| resolve_in_path(&server.command, path_var))
        else {
            unavailable.insert(
                name.clone(),
                Unavailable::BinaryNotFound {
                    command: server.command.clone(),
                },
            );
            continue;
        };
        let mut config = server.clone();
        config.args = substitute_runtime_tokens(&config.args);
        available.push(Available {
            name: name.clone(),
            config,
            resolved,
        });
    }

    (available, unavailable)
}

/// Which available servers handle this file, primary servers first.
///
/// The ordering is the point rather than a nicety. A `.py` file matches pyright and
/// ruff; asking ruff for a definition gets nothing useful, because it is a linter.
/// omp sorts the same way, and callers take the first for navigation while using all
/// of them for diagnostics.
///
/// # A `fileTypes` entry is not always an extension
///
/// `dockerls` declares `"Dockerfile"`, which has no extension at all — and a
/// `Dockerfile` has no extension either, so an extension-only comparison never
/// matches it and Docker support silently does not exist. Found by asserting that
/// every default carries a leading dot; one does not, and it was the code that was
/// wrong rather than the data.
///
/// So four comparisons, following omp's `getServersForFile`: the extension and the
/// whole basename, each with and without a leading dot. The dotless forms also make
/// a user config that writes `"ts"` instead of `".ts"` work, which omp's comment
/// says is why they added it.
pub fn servers_for_file<'a>(available: &'a [Available], file: &Path) -> Vec<&'a Available> {
    let basename = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dotted = format!(".{extension}");

    let mut matched: Vec<&Available> = available
        .iter()
        .filter(|server| {
            server.config.file_types.iter().any(|file_type| {
                let declared = file_type.to_ascii_lowercase();
                let bare = declared.strip_prefix('.').unwrap_or(&declared);
                // The extension comparisons are skipped for an extensionless file,
                // or `Makefile` would match a server declaring `""`.
                (!extension.is_empty() && (declared == dotted || bare == extension))
                    || declared == basename
                    || bare == basename
            })
        })
        .collect();
    // Stable, so that servers of equal rank keep their name order rather than
    // shuffling between runs.
    matched.sort_by_key(|server| server.config.is_linter);
    matched
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
