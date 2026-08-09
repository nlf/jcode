//! Group F tests: what is configured, and what applies here.
//!
//! omp's cases are `lsp-regressions.test.ts:2716` (F1), `:2797` (F2) and `:2766`
//! (F3). Theirs go through a workspace and a reload; ours test the functions those
//! behaviours are made of, because the caching F1 and F2 are about does not exist
//! yet — we have no per-cwd config cache to invalidate. **Recorded as a gap rather
//! than a pass**: see `a_reload_gap_is_recorded` at the bottom, which is a comment
//! with a name, not an assertion.

use super::*;

use std::fs;
use std::path::PathBuf;

/// A project directory with the given files, and executables where asked.
fn project(files: &[(&str, &str)], executables: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, contents) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&full, contents).expect("write");
    }
    for path in executables {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&full, "#!/bin/sh\n").expect("write");
        make_executable(&full);
    }
    dir
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// The checked-in defaults must parse, and must be the file omp shipped.
///
/// A count assertion looks brittle until you consider what it catches: a truncated
/// copy, or a merge that silently dropped entries. The file came from omp verbatim
/// and 53 is what it contains.
#[test]
fn the_defaults_parse() {
    let config = defaults();
    assert_eq!(config.servers.len(), 53, "defaults.json lost entries");

    let rust = config
        .servers
        .get("rust-analyzer")
        .expect("rust-analyzer must be configured");
    assert_eq!(rust.command, "rust-analyzer");
    assert_eq!(rust.file_types, vec![".rs"]);
    assert!(rust.root_markers.contains(&"Cargo.toml".to_string()));
    // omp sets checkOnSave false for rust-analyzer; the settings must survive the
    // parse, since dropping them silently changes behaviour against a real server.
    assert!(
        rust.settings.is_some(),
        "rust-analyzer settings were dropped"
    );
}

/// Every default has the fields needed to be usable.
///
/// omp's `normalizeServerConfig` rejects a server missing a command, file types or
/// root markers. Rather than port the rejection and never exercise it, this asserts
/// the shipped file needs none of it.
#[test]
fn every_default_is_complete() {
    for (name, server) in defaults().servers {
        assert!(!server.command.is_empty(), "{name} has no command");
        assert!(!server.file_types.is_empty(), "{name} has no file types");
        assert!(
            !server.root_markers.is_empty(),
            "{name} has no root markers"
        );
        for file_type in &server.file_types {
            // Not "must start with a dot": `dockerls` declares `"Dockerfile"`, a
            // whole filename. That assertion was my first draft and it failed, which
            // is how the extension-only matcher in `servers_for_file` was found --
            // it could never have matched a Dockerfile. The data was right.
            assert!(!file_type.is_empty(), "{name} has an empty file type");
            assert!(
                !file_type.contains('/'),
                "{name} file type {file_type:?} looks like a path, not an extension \
                 or filename"
            );
        }
    }
}

/// Both config shapes parse: the `servers` wrapper and a bare map.
///
/// omp's docs show the wrapper and their tests write the bare form, so a port that
/// accepts only one silently ignores real config files.
#[test]
fn both_config_shapes_are_accepted() {
    let wrapped = parse(r#"{"servers": {"mine": {"command": "x", "fileTypes": [".q"], "rootMarkers": ["q.toml"]}}}"#)
        .expect("wrapped");
    assert!(wrapped.servers.contains_key("mine"));

    let bare =
        parse(r#"{"mine": {"command": "x", "fileTypes": [".q"], "rootMarkers": ["q.toml"]}}"#)
            .expect("bare");
    assert!(bare.servers.contains_key("mine"));
}

/// `idleTimeoutMs` is a setting, not a server, in either shape.
///
/// In the bare form it sits among server names, so a parser that treats every
/// top-level key as a server fails the whole file on a number. That would make one
/// stray setting disable every LSP server, which is why this is tested in both
/// shapes.
#[test]
fn the_idle_timeout_is_not_mistaken_for_a_server() {
    let bare = parse(r#"{"idleTimeoutMs": 60000, "mine": {"command": "x", "fileTypes": [".q"], "rootMarkers": ["q.toml"]}}"#)
        .expect("bare with idle timeout");
    assert_eq!(
        bare.idle_timeout,
        Some(std::time::Duration::from_millis(60000))
    );
    assert_eq!(bare.servers.len(), 1, "idleTimeoutMs became a server");

    let wrapped = parse(r#"{"idleTimeoutMs": 500, "servers": {}}"#).expect("wrapped");
    assert_eq!(
        wrapped.idle_timeout,
        Some(std::time::Duration::from_millis(500))
    );
}

/// **An override changes one field and keeps the rest.**
///
/// The case that matters most in this module. Someone pointing rust-analyzer at their
/// own build writes `{"command": "/my/ra"}` and nothing else; if that replaced the
/// whole entry, the server would keep no file types and match no file — configured,
/// enabled, and silently never used. omp merges per-field with a spread.
#[test]
fn an_override_replaces_only_the_fields_it_names() {
    let mut config = defaults();
    let before = config.servers["rust-analyzer"].clone();

    merge(
        &mut config,
        parse(r#"{"rust-analyzer": {"command": "/my/ra"}}"#).expect("overlay"),
    );

    let after = &config.servers["rust-analyzer"];
    assert_eq!(after.command, "/my/ra", "the override did not apply");
    assert_eq!(after.file_types, before.file_types, "file types were lost");
    assert_eq!(
        after.root_markers, before.root_markers,
        "root markers were lost"
    );
    assert_eq!(after.settings, before.settings, "settings were lost");
}

/// A config file can add a server the defaults never heard of.
#[test]
fn an_overlay_can_add_a_new_server() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(
            r#"{"my-lsp": {"command": "mine", "fileTypes": [".zz"], "rootMarkers": ["zz.toml"]}}"#,
        )
        .expect("overlay"),
    );
    assert_eq!(config.servers["my-lsp"].command, "mine");
    assert_eq!(config.servers.len(), 54);
}

/// Disabling keeps the entry, so `status` can explain the silence.
///
/// Removing it instead would make a disabled server indistinguishable from an
/// unknown one, and "nothing happened and I cannot tell you why" is the failure this
/// avoids.
#[test]
fn disabling_a_server_reports_it_as_disabled_rather_than_dropping_it() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(r#"{"rust-analyzer": {"disabled": true}}"#).expect("overlay"),
    );
    assert!(
        config.servers.contains_key("rust-analyzer"),
        "the entry vanished"
    );

    let dir = project(&[("Cargo.toml", "[package]\n")], &[]);
    let (available, unavailable) = detect(&config, dir.path(), Some(""));
    assert!(
        !available
            .iter()
            .any(|server| server.name == "rust-analyzer")
    );
    assert_eq!(
        unavailable.get("rust-analyzer"),
        Some(&Unavailable::Disabled)
    );
}

/// **F3: configured is not the same as available, and the reason is kept.**
///
/// omp: "status distinguishes configured servers from started clients". Their version
/// asserts on `status` output; ours asserts the distinction that output is made of.
/// A missing binary and a wrong project type are different problems with different
/// fixes, and collapsing them into "no server" makes both undebuggable.
#[test]
fn detection_distinguishes_a_missing_binary_from_a_different_project() {
    let dir = project(&[("Cargo.toml", "[package]\n")], &[]);
    let (available, unavailable) = detect(&defaults(), dir.path(), Some(""));

    // A Cargo.toml is present but no rust-analyzer exists on the empty PATH.
    assert_eq!(
        unavailable.get("rust-analyzer"),
        Some(&Unavailable::BinaryNotFound {
            command: "rust-analyzer".to_string()
        }),
        "a Rust project without rust-analyzer must say the binary is missing"
    );
    // gopls has no go.mod here: not this kind of project, which is not a problem.
    assert_eq!(unavailable.get("gopls"), Some(&Unavailable::NotThisProject));
    assert!(
        available.is_empty(),
        "nothing is installed on an empty PATH"
    );
}

/// A server whose markers and binary are both present is available, with a path.
#[test]
fn a_present_binary_in_a_matching_project_is_available() {
    let dir = project(&[("Cargo.toml", "[package]\n")], &[]);
    let bin = project(&[], &["rust-analyzer"]);

    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(bin.path().to_str().expect("utf-8 path")),
    );

    let found = available
        .iter()
        .find(|server| server.name == "rust-analyzer")
        .expect("rust-analyzer must be available");
    assert_eq!(found.resolved, bin.path().join("rust-analyzer"));
}

/// A project's own bin directory wins over PATH.
///
/// A repo with a pinned `typescript-language-server` in `node_modules/.bin` means
/// that one. Using the global install produces diagnostics from a different version
/// than the project's own tooling, which is worse than none: they look authoritative
/// and disagree with CI.
#[test]
fn a_project_local_binary_wins_over_one_on_the_path() {
    let dir = project(
        &[("package.json", "{}"), ("tsconfig.json", "{}")],
        &["node_modules/.bin/typescript-language-server"],
    );
    let global = project(&[], &["typescript-language-server"]);

    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(global.path().to_str().expect("utf-8 path")),
    );

    let found = available
        .iter()
        .find(|server| server.name == "typescript-language-server")
        .expect("typescript-language-server must be available");
    assert_eq!(
        found.resolved,
        dir.path()
            .join("node_modules/.bin/typescript-language-server"),
        "PATH won over the project's own binary"
    );
}

/// A local bin directory is only consulted when its markers are present.
///
/// Without the check, any directory named `bin` under the cwd becomes a search path,
/// so an unrelated `bin/gopls` in a Rust project would be spawned as a Go server.
#[test]
fn a_local_bin_directory_without_its_markers_is_ignored() {
    // A `bin/gopls` but no go.mod: the Go local-bin rule must not fire.
    let dir = project(&[("Cargo.toml", "[package]\n")], &["bin/gopls"]);
    assert_eq!(resolve_local("gopls", dir.path()), None);

    // With go.mod present, the same file is found.
    let with_marker = project(&[("go.mod", "module x\n")], &["bin/gopls"]);
    assert_eq!(
        resolve_local("gopls", with_marker.path()),
        Some(with_marker.path().join("bin/gopls"))
    );
}

/// Glob root markers match, and only at the top level.
///
/// Seven defaults use patterns (`*.tla`, `*.cabal`, `*.sln`, `*.xcodeproj`, …). The
/// depth limit is deliberate: omp notes that a recursive glob descends into
/// `node_modules`, and a marker found three directories down does not indicate a root
/// here.
#[test]
fn a_glob_root_marker_matches_only_at_the_top_level() {
    let dir = project(&[("spec.tla", "---- MODULE spec ----")], &[]);
    assert!(has_root_markers(dir.path(), &["*.tla".to_string()]));

    let nested = project(&[("deep/inner/spec.tla", "---- MODULE spec ----")], &[]);
    assert!(
        !has_root_markers(nested.path(), &["*.tla".to_string()]),
        "a marker below the root must not count"
    );
}

/// The glob matcher handles the patterns the defaults actually contain.
///
/// Unit-tested separately from the filesystem because the interesting cases are
/// string cases, and a backtracking matcher written by hand deserves them.
#[test]
fn the_glob_matcher_handles_the_shapes_in_use() {
    assert!(glob_matches("*.tla", "spec.tla"));
    assert!(glob_matches("*.cabal", "my-project.cabal"));
    assert!(
        !glob_matches("*.tla", "spec.tlaplus"),
        "the suffix must anchor"
    );
    assert!(
        !glob_matches("*.tla", "tla"),
        "a bare extension is not a match"
    );

    // A dotfile: `*` matches a leading dot here, unlike a shell.
    assert!(glob_matches("*.yml", ".swiftlint.yml"));

    // `?` is one character, not any number.
    assert!(glob_matches("a?c", "abc"));
    assert!(!glob_matches("a?c", "ac"));
    assert!(!glob_matches("a?c", "abbc"));

    // Interior and multiple stars, which backtracking is what makes work.
    assert!(glob_matches("*.test.*", "thing.test.ts"));
    assert!(glob_matches("a*b*c", "axxbyyc"));
    assert!(!glob_matches("a*b*c", "axxbyy"));

    // Degenerate patterns must not loop or panic.
    assert!(glob_matches("*", ""));
    assert!(glob_matches("*", "anything"));
    assert!(glob_matches("**", "anything"));
    assert!(!glob_matches("", "x"));
    assert!(glob_matches("", ""));
}

/// The Windows executable suffixes are enumerated, so the logic is checked even
/// where the filesystem layout cannot be.
///
/// The porting notes dropped omp's four Windows cases as untestable here. Half of
/// that was right: `.venv/Scripts` layouts cannot be built on macOS. But appending
/// `.exe`/`.cmd`/`.bat` is a string operation, and jcode ships on Windows, so
/// leaving it entirely unverified was worse than testing the part that is portable.
#[test]
fn windows_executable_suffixes_are_enumerated_in_priority_order() {
    let candidates = local_candidates(Path::new("/p/node_modules/.bin/tsserver"));

    // The extensionless path always comes first: it is the only candidate on Unix,
    // and preferred on Windows when it exists.
    assert_eq!(
        candidates[0],
        PathBuf::from("/p/node_modules/.bin/tsserver")
    );

    if cfg!(windows) {
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/p/node_modules/.bin/tsserver"),
                PathBuf::from("/p/node_modules/.bin/tsserver.exe"),
                PathBuf::from("/p/node_modules/.bin/tsserver.cmd"),
                PathBuf::from("/p/node_modules/.bin/tsserver.bat"),
            ]
        );
    } else {
        assert_eq!(candidates.len(), 1, "no suffixes are tried off Windows");
    }
}

/// A non-executable file on PATH is not a server.
///
/// PATH directories and `node_modules/.bin` contain plenty of non-executable files.
/// Returning one turns "not installed" into a spawn failure at first use, which is
/// reported far from its cause.
#[test]
#[cfg(unix)]
fn a_non_executable_file_on_the_path_is_not_resolved() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("pretend-lsp"), "not executable").expect("write");

    assert_eq!(
        resolve_in_path("pretend-lsp", Some(dir.path().to_str().expect("utf-8"))),
        None,
        "a non-executable file was accepted as a server"
    );
}

/// An absolute command is used as a path, not looked up on PATH.
///
/// This is the case an override creates: `{"command": "/my/ra"}`. Searching PATH for
/// a string containing a separator never matches, so without this the configured
/// server would be reported as missing.
#[test]
fn an_absolute_command_is_used_directly() {
    let bin = project(&[], &["custom-ra"]);
    let absolute = bin.path().join("custom-ra");

    assert_eq!(
        resolve_in_path(absolute.to_str().expect("utf-8"), Some("")),
        Some(absolute.clone()),
        "an absolute command must be used as given, with an empty PATH"
    );
    assert_eq!(
        resolve_in_path("/nonexistent/ra", Some("")),
        None,
        "a missing absolute command must not be reported as found"
    );
}

/// An empty PATH entry is skipped rather than treated as the current directory.
///
/// `PATH=/a::/b` and a trailing colon are common, and the empty element
/// conventionally means the cwd. Honouring that would search whatever directory the
/// process happens to be in for a language server, which is a way to execute a file
/// out of a repository being examined.
#[test]
fn an_empty_path_element_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let previous = std::env::current_dir().expect("cwd");
    fs::write(dir.path().join("sneaky-lsp"), "#!/bin/sh\n").expect("write");
    make_executable(&dir.path().join("sneaky-lsp"));

    // Set the cwd to the directory holding the file, then search a PATH whose only
    // element is empty.
    std::env::set_current_dir(dir.path()).expect("chdir");
    let found = resolve_in_path("sneaky-lsp", Some(":"));
    std::env::set_current_dir(previous).expect("restore cwd");

    assert_eq!(found, None, "an empty PATH element was searched as the cwd");
}

/// **Linters sort after primary servers.**
///
/// A file can match a linter and a language server. Asking the linter for a
/// definition returns nothing, because it lints -- so the caller taking the first
/// result means the order decides whether go-to-definition works for that language at
/// all.
///
/// # The pair here is chosen, not convenient
///
/// My first version used Python (pyright and ruff) and it **passed with the sort
/// deleted**: mutation-testing caught it. The servers come out of a `BTreeMap`, so
/// they arrive in name order, and `pylsp` < `pyright` < `ruff` already puts the
/// linter last. The test asserted an order the data supplied for free.
///
/// `biome` (a linter) sorts before `typescript-language-server` alphabetically, so
/// only a real sort can fix it. Verified by deleting the `sort_by_key` again: this
/// test fails, and the Python one did not.
#[test]
fn a_linter_sorts_after_a_primary_server_for_the_same_file() {
    // Markers for both, so both are configured for this project.
    let dir = project(
        &[
            ("biome.json", "{}"),
            ("package.json", "{}"),
            ("tsconfig.json", "{}"),
        ],
        &[],
    );
    let bin = project(&[], &["biome", "typescript-language-server"]);
    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    let matched = servers_for_file(&available, Path::new("app.ts"));
    let names: Vec<&String> = matched.iter().map(|server| &server.name).collect();
    assert!(
        matched.len() >= 2,
        "expected both a language server and a linter, got {names:?}"
    );
    // The precise assertion: biome comes first by name and must not come first here.
    assert_eq!(
        matched[0].name, "typescript-language-server",
        "a linter came first: {names:?}"
    );
    assert!(
        matched.iter().any(|server| server.name == "biome"),
        "the linter must still be offered, just not first: {names:?}"
    );
}

/// Extension matching ignores case and requires the whole extension.
#[test]
fn file_type_matching_is_case_insensitive_and_anchored() {
    let dir = project(&[("Cargo.toml", "[package]\n")], &[]);
    let bin = project(&[], &["rust-analyzer"]);
    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    assert_eq!(servers_for_file(&available, Path::new("a.rs")).len(), 1);
    assert_eq!(
        servers_for_file(&available, Path::new("a.RS")).len(),
        1,
        "an uppercase extension must still match"
    );
    // `.rss` is not `.rs`.
    assert!(servers_for_file(&available, Path::new("a.rss")).is_empty());
    // No extension at all.
    assert!(servers_for_file(&available, Path::new("Makefile")).is_empty());
}

/// F1 and F2 are **not** ported, and this is where that is written down.
///
/// omp's cases are about a per-cwd config cache:
///
/// - F1 (`:2716`): "workspace reload rediscovers LSP servers after an empty config
///   was cached" — a cold start in a project with nothing installed must not cache
///   "no servers" forever.
/// - F2 (`:2797`): "reload * invalidates the per-cwd config cache so newly written
///   .omp/lsp.json is observed".
///
/// Both describe invalidating a cache. This module has no cache: [`detect`] walks the
/// filesystem every call. So there is nothing to invalidate and nothing to test, and
/// writing a test that passes because the bug is impossible would be worse than
/// saying so.
///
/// The exposure is real but deferred: our daemon is long-lived, so when a cache is
/// added (and it will be, since `detect` stats once per configured server) these two
/// cases become live and load-bearing. They are recorded in `PORTING_NOTES.md`
/// against the cache, not against this module.
///
/// This is a comment with a test's name because a named unported case is findable and
/// a paragraph in a document is not.
#[test]
fn a_reload_gap_is_recorded() {
    // Asserts the premise the gap depends on: no caching, so repeated calls observe
    // the filesystem as it is now. If someone adds a cache, this fails and points
    // them at the paragraph above.
    let dir = tempfile::tempdir().expect("tempdir");
    let config = defaults();

    let (available, _) = detect(&config, dir.path(), Some(""));
    assert!(available.is_empty(), "an empty directory has no servers");

    // Make it a Rust project *after* the first call, with a binary available.
    fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write");
    let bin = project(&[], &["rust-analyzer"]);
    let (available, _) = detect(
        &config,
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    assert!(
        available
            .iter()
            .any(|server| server.name == "rust-analyzer"),
        "detection must observe a project that appeared since the last call; if this \
         fails, a cache was added and omp's F1/F2 reload cases now apply"
    );
}

/// **A `fileTypes` entry can be a whole filename, and `Dockerfile` is one.**
///
/// `dockerls` declares `"Dockerfile"`. A `Dockerfile` has no extension, so an
/// extension-only comparison matches nothing and Docker support quietly does not
/// exist -- no error, no diagnostic, just a language that never works.
///
/// Found by `every_default_is_complete`: I asserted every file type starts with a
/// dot, one did not, and the data turned out to be right and my matcher wrong.
#[test]
fn a_file_type_that_is_a_whole_filename_matches_that_file() {
    let dir = project(&[("Dockerfile", "FROM scratch\n")], &[]);
    let bin = project(&[], &["docker-langserver"]);
    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    let matched = servers_for_file(&available, Path::new("Dockerfile"));
    assert_eq!(
        matched.len(),
        1,
        "Dockerfile matched {:?}",
        matched
            .iter()
            .map(|server| &server.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(matched[0].name, "dockerls");

    // And a path, not just a bare name: the basename is what counts.
    assert_eq!(
        servers_for_file(&available, Path::new("deploy/Dockerfile")).len(),
        1,
        "the basename of a path must be compared, not the whole path"
    );
}

/// A dotless `fileTypes` entry in a user config still matches.
///
/// omp accepts `"ts"` as well as `".ts"`, and their comment says why: a missing dot
/// otherwise silently excludes the server from extension routing. Since we accept the
/// same input, we owe it the same test.
#[test]
fn a_user_config_may_omit_the_leading_dot() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(r#"{"dotless": {"command": "sh", "fileTypes": ["zz"], "rootMarkers": ["zz.toml"]}}"#)
            .expect("overlay"),
    );

    let dir = project(&[("zz.toml", "")], &[]);
    let bin = project(&[], &["sh"]);
    let (available, _) = detect(
        &config,
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    let matched = servers_for_file(&available, Path::new("thing.zz"));
    assert_eq!(matched.len(), 1, "a dotless file type did not match");
    assert_eq!(matched[0].name, "dotless");
}

/// An extensionless file does not match a server that declares an empty file type.
///
/// The guard this covers is subtle: comparing a dotless extension against a dotless
/// declaration means `""` equals `""`, so a malformed config entry would claim every
/// extensionless file in the repo -- Makefile, LICENSE, README.
#[test]
fn an_extensionless_file_does_not_match_an_empty_file_type() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(r#"{"greedy": {"command": "sh", "fileTypes": ["."], "rootMarkers": ["zz.toml"]}}"#)
            .expect("overlay"),
    );

    let dir = project(&[("zz.toml", "")], &[]);
    let bin = project(&[], &["sh"]);
    let (available, _) = detect(
        &config,
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    assert!(
        servers_for_file(&available, Path::new("Makefile")).is_empty(),
        "a server declaring \".\" claimed an extensionless file"
    );
}

/// **A config file naming no command still parses.**
///
/// `{"rust-analyzer": {"disabled": true}}` is the commonest edit anyone will write.
/// Parsed as a complete server it fails on `missing field command` and takes the
/// whole file with it, so one line disabling one server would disable every server.
/// That was the first implementation, and this is the test that caught it.
#[test]
fn a_partial_overlay_parses_without_a_command() {
    let file =
        parse(r#"{"rust-analyzer": {"disabled": true}}"#).expect("a partial overlay must parse");
    assert_eq!(file.servers.len(), 1);
    assert_eq!(file.servers["rust-analyzer"].disabled, Some(true));
    assert_eq!(
        file.servers["rust-analyzer"].command, None,
        "an absent command must stay absent, not become empty"
    );
}

/// An explicit `false` can re-enable a server an earlier file disabled.
///
/// This is why the overlay fields are `Option<bool>` rather than `bool`: with a bare
/// bool, `false` and "not mentioned" are the same value, and a higher-priority config
/// could never undo a lower-priority `disabled: true`.
#[test]
fn an_explicit_false_can_undo_a_disable() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(r#"{"gopls": {"disabled": true}}"#).expect("first"),
    );
    assert!(config.servers["gopls"].disabled);

    merge(
        &mut config,
        parse(r#"{"gopls": {"disabled": false}}"#).expect("second"),
    );
    assert!(
        !config.servers["gopls"].disabled,
        "an explicit false must re-enable, or a lower-priority disable is permanent"
    );
}

/// An overlay may set empty args, and that is different from omitting them.
///
/// `gopls` defaults to `["serve"]`. Someone who wants it invoked bare writes
/// `"args": []`, and treating empty as absent would silently keep `serve`.
#[test]
fn empty_args_are_honoured_rather_than_treated_as_absent() {
    let mut config = defaults();
    assert_eq!(config.servers["gopls"].args, vec!["serve"]);

    merge(
        &mut config,
        parse(r#"{"gopls": {"args": []}}"#).expect("overlay"),
    );
    assert!(
        config.servers["gopls"].args.is_empty(),
        "explicit empty args were ignored"
    );
}

/// A new server missing required fields is rejected, not half-added.
///
/// omp's `normalizeServerConfig` returns null and logs. A server with no command
/// cannot be spawned and one with no file types can never be selected, so keeping it
/// would only produce a `status` entry that never does anything.
#[test]
fn a_new_server_without_required_fields_is_rejected() {
    let mut config = defaults();
    let before = config.servers.len();

    merge(
        &mut config,
        parse(r#"{"broken": {"disabled": false}, "alsobroken": {"command": "x"}}"#)
            .expect("overlay"),
    );

    assert_eq!(
        config.servers.len(),
        before,
        "an unusable server was added: {:?}",
        config.servers.keys().collect::<Vec<_>>()
    );
}

/// **An empty override does not delete a default it cannot replace.**
///
/// Verified against a transcription of omp's `mergeServers` + `normalizeServerConfig`,
/// run in node: an empty `command`, `fileTypes` or `rootMarkers` each makes the merged
/// entry fail normalization, so omp keeps the previous config. Empty `args` normalizes
/// fine and applies.
///
/// My first version honoured empty for all four and the comment said so approvingly.
/// It was right about `args` and wrong about the rest, which is the worse kind of wrong:
/// the reasoning looked deliberate. An empty `fileTypes` would have left rust-analyzer
/// configured, enabled, and matching no file -- silently never used. Found by an
/// adversarial reviewer.
#[test]
fn an_empty_override_cannot_invalidate_a_working_default() {
    let before = defaults().servers["rust-analyzer"].clone();

    for overlay in [
        r#"{"rust-analyzer": {"fileTypes": []}}"#,
        r#"{"rust-analyzer": {"rootMarkers": []}}"#,
        r#"{"rust-analyzer": {"command": ""}}"#,
    ] {
        let mut config = defaults();
        merge(&mut config, parse(overlay).expect("overlay"));
        let after = &config.servers["rust-analyzer"];
        assert_eq!(
            after.file_types, before.file_types,
            "{overlay} emptied the file types"
        );
        assert_eq!(
            after.root_markers, before.root_markers,
            "{overlay} emptied the root markers"
        );
        assert_eq!(
            after.command, before.command,
            "{overlay} blanked the command"
        );
    }
}

/// **One malformed entry does not lose the rest of the file.**
///
/// omp normalizes per entry and warns about the ones it drops. Parsing the whole map at
/// once meant `{"good": {...}, "bad": 42}` returned an error and *every* server in the
/// file was discarded. Config files are hand-written, so a typo in one entry is the
/// likely case, and taking twenty good entries down with it is the wrong failure.
#[test]
fn a_malformed_entry_is_skipped_and_the_rest_survive() {
    let file = parse(
        r#"{
            "good": {"command": "sh", "fileTypes": [".zz"], "rootMarkers": ["zz.toml"]},
            "bad": 42,
            "alsogood": {"disabled": true}
        }"#,
    )
    .expect("a file with one bad entry must still parse");

    assert!(file.servers.contains_key("good"), "a good entry was lost");
    assert!(
        file.servers.contains_key("alsogood"),
        "a good entry was lost"
    );
    assert!(!file.servers.contains_key("bad"));
    assert_eq!(
        file.skipped,
        vec!["bad".to_string()],
        "the skipped name must be reported, or a typo is invisible"
    );
}

/// Truly invalid JSON is still an error, not silently empty.
///
/// Per-entry tolerance must not become "any input parses". A file that is not JSON at
/// all is a different problem from one entry being wrong, and reporting it as "no
/// servers configured" would hide it.
#[test]
fn malformed_json_is_still_an_error() {
    assert!(parse("{not json").is_err());
    assert!(parse("").is_err());
    // A JSON array is valid JSON but not a config shape.
    assert!(parse("[1, 2, 3]").is_err());
}

/// **`$PID` in omnisharp's args is substituted.**
///
/// `omnisharp` takes `--hostPID <pid>` to know which process to exit with, and
/// `defaults.json` carries the literal `"$PID"`. omp substitutes it in
/// `applyRuntimeDefaults`; nothing here did, so omnisharp would have been spawned with
/// a literal `$PID` and refused to start.
///
/// Nothing in the suite spawns omnisharp, so no test would ever have caught this. Found
/// by an adversarial reviewer reading the data against omp's loader.
#[test]
fn the_pid_token_is_substituted_for_the_real_pid() {
    let dir = project(&[("app.sln", ""), ("app.csproj", "")], &[]);
    let bin = project(&[], &["omnisharp"]);
    let (available, _) = detect(
        &defaults(),
        dir.path(),
        Some(bin.path().to_str().expect("utf-8")),
    );

    let omnisharp = available
        .iter()
        .find(|server| server.name == "omnisharp")
        .expect("omnisharp must be available");

    assert!(
        !omnisharp.config.args.iter().any(|arg| arg == "$PID"),
        "a literal $PID reached the spawn arguments: {:?}",
        omnisharp.config.args
    );
    assert!(
        omnisharp
            .config
            .args
            .contains(&std::process::id().to_string()),
        "the real pid is missing from {:?}",
        omnisharp.config.args
    );
    // The rest of the arguments are untouched.
    assert!(omnisharp.config.args.contains(&"--hostPID".to_string()));
    assert!(
        omnisharp
            .config
            .args
            .contains(&"--languageserver".to_string())
    );
}

/// The stored config keeps the token, so `status` shows what was configured.
///
/// Substitution happens at detection, not at parse. Otherwise one process's pid gets
/// baked into a config that outlives it, and a reader of `status` sees a number with no
/// explanation instead of the token they wrote.
#[test]
fn the_stored_config_keeps_the_token_unsubstituted() {
    assert!(
        defaults().servers["omnisharp"]
            .args
            .contains(&"$PID".to_string()),
        "the token must survive in the configured form"
    );
}

/// Only the exact token is substituted, not any argument containing it.
///
/// omp compares whole (`arg === PID_TOKEN`). A substring replace would rewrite a path
/// that happened to contain `$PID`, which is the kind of thing that works until someone
/// has a directory with an unusual name.
#[test]
fn a_path_containing_the_token_is_not_rewritten() {
    let substituted = substitute_runtime_tokens(&[
        "$PID".to_string(),
        "/tmp/$PID/socket".to_string(),
        "--flag=$PID".to_string(),
    ]);
    assert_eq!(substituted[0], std::process::id().to_string());
    assert_eq!(substituted[1], "/tmp/$PID/socket", "a path was rewritten");
    assert_eq!(substituted[2], "--flag=$PID", "a flag value was rewritten");
}

/// **A non-map `servers` falls back to the bare reading rather than losing the file.**
///
/// omp gates the wrapper reading on `isRecord(rawServers)` and otherwise treats the
/// top-level keys as servers, so `{"servers": 42, "gopls": {...}}` keeps gopls.
///
/// This was the last whole-file-loss path, and I was inclined to leave it because nobody
/// writes that. The reviewer asked whether the reasoning was too convenient, and it was:
/// the fix turned out to be *smaller* than the code it replaced, because inspecting the
/// value directly is less machinery than a struct that serde has to reject first.
/// "Unlikely input" is a reason not to add machinery, not a reason to keep a path that
/// discards someone's whole configuration.
#[test]
fn a_non_map_servers_key_does_not_lose_the_file() {
    let file = parse(
        r#"{"servers": 42, "gopls": {"command": "gopls", "fileTypes": [".go"], "rootMarkers": ["go.mod"]}}"#,
    )
    .expect("a bogus servers key must not lose the file");

    assert!(file.servers.contains_key("gopls"), "gopls was lost");
    // `servers` itself is not a server, so it is reported as skipped rather than
    // silently dropped.
    assert_eq!(file.skipped, vec!["servers".to_string()]);
}

/// The wrapper form still wins when `servers` is a map.
///
/// The fallback must not become "always read the top level", or a wrapped config would
/// have its `servers` key parsed as a server named `servers`.
#[test]
fn the_wrapper_form_still_takes_precedence_when_it_is_a_map() {
    let file = parse(
        r#"{"servers": {"gopls": {"command": "gopls", "fileTypes": [".go"], "rootMarkers": ["go.mod"]}}}"#,
    )
    .expect("wrapped");

    assert!(file.servers.contains_key("gopls"));
    assert_eq!(
        file.servers.len(),
        1,
        "the wrapper key leaked in as a server"
    );
    assert!(file.skipped.is_empty());
}

/// `idleTimeoutMs` is read in either shape.
///
/// It sits beside the servers in the bare form and beside the `servers` key in the
/// wrapped one. Rewriting `parse` to inspect the object made one lookup cover both, and
/// this is what says so.
#[test]
fn the_idle_timeout_is_read_in_both_shapes() {
    let wrapped = parse(r#"{"idleTimeoutMs": 500, "servers": {}}"#).expect("wrapped");
    assert_eq!(
        wrapped.idle_timeout,
        Some(std::time::Duration::from_millis(500))
    );

    let bare = parse(r#"{"idleTimeoutMs": 700}"#).expect("bare");
    assert_eq!(
        bare.idle_timeout,
        Some(std::time::Duration::from_millis(700))
    );
    assert!(
        bare.servers.is_empty() && bare.skipped.is_empty(),
        "idleTimeoutMs was treated as a server: {bare:?}"
    );
}

/// **`warmupTimeoutMs` and `capabilities` survive the parse.**
///
/// Both are real fields in `defaults.json` that omp consumes -- `marksman` sets a 2000ms
/// warm-up, `rust-analyzer` declares five extension capabilities -- and serde was dropping
/// both silently. Tolerating unknown fields is deliberate here (real servers send values
/// outside the spec), and that tolerance is precisely why these went unnoticed for the
/// whole port: a field in the data but not in the struct is invisible.
///
/// Neither is consumed yet, because startup readiness and the rust-analyzer extensions are
/// later work. Parsed anyway, so the gap is a visible `todo` rather than a silent loss, and
/// so that whoever wires them finds the values already present.
///
/// Reported by an adversarial reviewer as unreached.
#[test]
fn the_startup_and_capability_fields_are_not_silently_dropped() {
    let config = defaults();

    assert_eq!(
        config.servers["marksman"].warmup_timeout_ms,
        Some(2000),
        "marksman's warm-up override was dropped"
    );
    // Every other server leaves it unset, which is what "use the default" looks like.
    assert_eq!(config.servers["gopls"].warmup_timeout_ms, None);

    let rust = &config.servers["rust-analyzer"].capabilities;
    assert_eq!(
        rust.len(),
        5,
        "rust-analyzer's declared capabilities were dropped: {rust:?}"
    );
    for capability in [
        "flycheck",
        "ssr",
        "expandMacro",
        "runnables",
        "relatedTests",
    ] {
        assert_eq!(
            rust.get(capability),
            Some(&true),
            "{capability} is missing from {rust:?}"
        );
    }
    // And a server that declares none has an empty map rather than a missing field.
    assert!(config.servers["gopls"].capabilities.is_empty());
}

/// A config file can set both, and an unknown capability name is preserved.
///
/// Kept as a string-keyed map rather than an enum precisely so that a server declaring
/// something we have not heard of is carried rather than rejected -- the same tolerance
/// argument the crate makes for not depending on `lsp-types`.
#[test]
fn a_config_file_can_set_the_startup_and_capability_fields() {
    let mut config = defaults();
    merge(
        &mut config,
        parse(
            r#"{"gopls": {"warmupTimeoutMs": 9000, "capabilities": {"somethingNew": true, "off": false}}}"#,
        )
        .expect("overlay"),
    );

    let gopls = &config.servers["gopls"];
    assert_eq!(gopls.warmup_timeout_ms, Some(9000));
    assert_eq!(gopls.capabilities.get("somethingNew"), Some(&true));
    assert_eq!(
        gopls.capabilities.get("off"),
        Some(&false),
        "an explicitly false capability must be kept, not filtered out"
    );
}
