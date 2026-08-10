#!/usr/bin/env python3
"""Check that docs/CONFIG_SAVE_VERIFICATION.md still describes reality.

The doc claims every requirement of `Config::save` maps to a named check. That
claim is itself an unverified assertion unless something enforces it, and a
verification doc that has quietly gone stale is worse than none: it launders an
unchecked change into a documented one.

Verifies, against the tree rather than against the prose:

  1. every test name cited in the doc exists in the suite
  2. every test in config_format_tests.rs is cited in the doc
  3. every mutation in the sweep is cited in the doc's mutation table
  4. every file the change touched is accounted for in the doc

Exits non-zero on any drift. Runs from any directory.
"""
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DOC = REPO_ROOT / "docs/CONFIG_SAVE_VERIFICATION.md"
TESTS = REPO_ROOT / "crates/jcode-base/src/config_format_tests.rs"
SWEEP = REPO_ROOT / "scripts/mutation_sweep_config_save.py"
BASE_COMMIT = "f5eb4f860~1"


def fail(problems: list[str]) -> int:
    for p in problems:
        print(f"*** DRIFT  {p}")
    print()
    print("drift:", len(problems))
    return 1 if problems else 0


def main() -> int:
    doc = DOC.read_text()
    problems: list[str] = []

    # 1 + 2: the doc's test names and the suite's must agree.
    cited = set(re.findall(r"`([a-z][a-z0-9_]{12,})`", doc))
    declared = set(re.findall(r"#\[test\]\s*\nfn (\w+)", TESTS.read_text()))

    listed = subprocess.run(
        ["cargo", "test", "-p", "jcode-base", "--lib", "--", "--list"],
        capture_output=True, text=True, cwd=REPO_ROOT,
    ).stdout
    real = set(re.findall(r"::(\w+): test", listed))

    # A cited name is "test-shaped" if it looks like a test identifier. Anything
    # test-shaped that is not in the suite is either a typo or a renamed test,
    # and both are exactly the drift this exists to catch. Names the doc uses
    # for helpers and functions (`changed_keys`, `save`) are excluded by
    # requiring the verb-ish multi-word shape these test names all have.
    test_shaped = {n for n in cited if n.count("_") >= 3}
    for name in sorted(test_shaped - real):
        problems.append(f"doc cites a test that is not in the suite: {name}")
    for name in sorted(declared - cited):
        problems.append(f"test exists but the doc never mentions it: {name}")

    # 3: every mutation the sweep runs should appear in the doc's table.
    sweep = SWEEP.read_text()
    mutations = re.findall(r'\(\s*"([^"]+)"\s*,', sweep)
    for label in mutations:
        if label.startswith("SELF-TEST"):
            continue
        # The doc paraphrases labels, so match on the helper name in the label.
        helper = label.split(":")[0].strip()
        if helper not in doc:
            problems.append(f"sweep mutates '{helper}' but the doc's table omits it")

    # 4: every file the change touched should be accounted for.
    changed = subprocess.run(
        ["git", "diff", "--name-only", f"{BASE_COMMIT}..HEAD"],
        capture_output=True, text=True, cwd=REPO_ROOT,
    ).stdout.split()
    for path in changed:
        stem = Path(path).name
        if stem in ("CONFIG_SAVE_VERIFICATION.md",):
            continue  # the doc need not cite itself
        if stem not in doc and path not in doc:
            problems.append(f"change touched {path} but the doc never mentions it")

    if not problems:
        print(f"doc cites {len(cited & real)} real tests; "
              f"{len(declared)} tests all cited; "
              f"{len(mutations)} mutations covered; "
              f"{len(changed)} changed files accounted for")
        print()
        print("drift: 0")
        return 0
    return fail(problems)


sys.exit(main())
