//! Coverage check for the real-home write guard.
//!
//! The guard in `src/test_guard.rs` is armed per-crate, through a
//! `[dev-dependencies]` entry on `jcode-storage` with `features =
//! ["test-guard"]`. That is what makes it structurally absent from shipped
//! binaries, but it has one weakness: **a crate nobody armed is silently
//! unguarded**, in exactly the way the original bug was silent.
//!
//! This test closes that gap. It walks the workspace, finds every crate that
//! both depends on `jcode-storage` and has tests, and fails if any of them
//! has not armed the guard. So the failure mode becomes "a new crate is
//! reported the first time it is tested" rather than "a new crate quietly
//! writes to the developer's home for months".
//!
//! It lives as an integration test rather than a unit test because it reads
//! the workspace manifests from disk, which is a property of the repository
//! rather than of the library.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/jcode-storage.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/jcode-storage")
        .to_path_buf()
}

fn crate_has_tests(crate_dir: &Path) -> bool {
    fn any_test_attr(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if any_test_attr(&path) {
                    return true;
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
                && (text.contains("#[test]") || text.contains("#[tokio::test]"))
            {
                return true;
            }
        }
        false
    }

    any_test_attr(&crate_dir.join("src")) || any_test_attr(&crate_dir.join("tests"))
}

/// Every crate that can write through `jcode-storage` and has tests must arm
/// the guard, or its tests can scribble into the developer's real home
/// without anything noticing.
#[test]
fn every_crate_that_can_write_arms_the_guard() {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut unarmed = BTreeSet::new();

    for entry in std::fs::read_dir(&crates_dir).expect("read crates/").flatten() {
        let crate_dir = entry.path();
        let manifest = crate_dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let name = crate_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        // The guard lives here; it cannot depend on itself.
        if name == "jcode-storage" {
            continue;
        }

        let text = std::fs::read_to_string(&manifest).expect("read manifest");
        if !text.contains("jcode-storage") {
            continue;
        }
        if !crate_has_tests(&crate_dir) {
            continue;
        }
        if text.contains("test-guard") {
            continue;
        }
        unarmed.insert(name);
    }

    assert!(
        unarmed.is_empty(),
        "these crates depend on jcode-storage and have tests, but have not \
         armed the real-home write guard, so their tests can write into the \
         developer's actual ~/.jcode without anything noticing:\n\n{}\n\n\
         Add to each crate's [dev-dependencies]:\n\n    \
         jcode-storage = {{ path = \"../jcode-storage\", features = [\"test-guard\"] }}\n\n\
         See crates/jcode-storage/src/test_guard.rs for why this is a \
         dev-dependency feature rather than cfg!(test).",
        unarmed
            .iter()
            .map(|n| format!("  - {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
