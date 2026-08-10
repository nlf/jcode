#!/usr/bin/env python3
"""Mutation-test the helpers behind `Config::save`'s formatting preservation.

Each entry breaks one behavior and reports which named test catches it. A
mutation nothing catches is a hole: code with no check behind it, and a test
suite that would not notice the behavior being deleted.

Two of the holes this found were real defects rather than missing tests, so it
is worth re-running after any change to config_file.rs:

  python3 scripts/mutation_sweep_config_save.py

Exits non-zero if any mutation survives. Restores the source file afterwards,
including on failure, and works from any directory.

Self-tested: adding a mutation nothing can catch (a comment-only edit) makes it
report a hole and exit 1, so a "0 holes" result is a measurement rather than a
vacuous pass.
"""
import subprocess, shutil, sys, re
from pathlib import Path

# Resolve against the script's own location, so the sweep can be run from
# anywhere rather than only the repo root.
REPO_ROOT = Path(__file__).resolve().parent.parent
SRC = REPO_ROOT / "crates/jcode-base/src/config/config_file.rs"
# Not /tmp: AGENTS.md notes it may be RAM-backed, and this holds the only copy
# of the original file while mutations are applied.
BACKUP = REPO_ROOT / "target" / "mutation-sweep-config-save.orig.rs"

MUTATIONS = [
    ("set_preserving_decor: drop decor preservation",
     """            let decor = slot.decor().clone();
            *slot = new_value;
            *slot.decor_mut() = decor;
            return;""",
     """            *slot = new_value;
            return;"""),

    ("clear_loaded_snapshot: do not clear on missing file",
     "            clear_loaded_snapshot();\n            return Ok(None);",
     "            return Ok(None);"),

    ("changed_keys: emit a change even when baseline == desired",
     "        if baseline_value == Some(desired_value) {\n            // Untouched by this caller: leave the file's value alone.\n            continue;\n        }",
     "        if false {\n            continue;\n        }"),

    ("apply_changes: ignore removals",
     "                if let Some(table) = descend(doc, parents) {\n                    table.remove(leaf);\n                }",
     "                let _ = parents;"),

    ("descend_or_create: refuse to create missing tables",
     "        let entry = table\n            .entry(key)\n            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));\n        table = entry.as_table_mut()?;",
     "        table = table.get_mut(key)?.as_table_mut()?;"),

    ("save: re-snapshot from the written text instead of self",
     "        record_loaded_snapshot(self);\n        Self::invalidate_cache();",
     "        if let Ok(v) = doc.to_string().parse::<toml::Value>()\n            && let Ok(mut slot) = LOADED_SNAPSHOT.write() { *slot = Some(v); }\n        Self::invalidate_cache();"),

    ("save: re-serialize the whole value instead of patching text (the original bug)",
     "        std::fs::write(&path, doc.to_string())?;",
     "        let mut whole = Self::load_from_file().and_then(|c| toml::Value::try_from(&c).ok()).unwrap_or_else(|| baseline.clone());\n        for (p, ch) in &changes { if let (KeyChange::Set(v), Some((leaf, par))) = (ch, p.split_last()) { let mut cur = &mut whole; for k in par { cur = cur.as_table_mut().unwrap().entry(k.clone()).or_insert(toml::Value::Table(Default::default())); } cur.as_table_mut().unwrap().insert(leaf.clone(), v.clone()); } }\n        std::fs::write(&path, toml::to_string_pretty(&whole)?)?;"),
]


def run_tests():
    out = subprocess.run(
        ["cargo", "test", "-p", "jcode-base", "--lib", "format_tests", "--",
         "--test-threads=1"],
        capture_output=True, text=True, cwd=REPO_ROOT)
    text = out.stdout + out.stderr
    if "error[E" in text or "error: could not compile" in text:
        return None
    return sorted(set(re.findall(r"^test (config::format_tests::\w+) \.\.\. FAILED",
                                 text, re.M)))


def main():
    BACKUP.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(SRC, BACKUP)
    original = SRC.read_text()
    holes = []
    try:
        for name, find, replace in MUTATIONS:
            if find not in original:
                print(f"SKIP (anchor not found): {name}")
                holes.append(name + "  [anchor missing]")
                continue
            SRC.write_text(original.replace(find, replace, 1))
            caught = run_tests()
            if caught is None:
                print(f"*** HOLE (did not compile): {name}")
                holes.append(name + "  [did not compile]")
            elif caught:
                short = [c.rsplit("::", 1)[1] for c in caught]
                print(f"CAUGHT  {name}\n        by: {', '.join(short[:3])}"
                      + (f" (+{len(short)-3} more)" if len(short) > 3 else ""))
            else:
                print(f"*** HOLE  {name}: no test failed")
                holes.append(name)
    finally:
        SRC.write_text(original)
    print()
    print("holes:", len(holes))
    for h in holes:
        print("  -", h)
    return 1 if holes else 0


sys.exit(main())
