---
name: upstream-sync
description: |
  Merge new upstream releases from 1jehuang/jcode into this fork. Use when asked
  to sync, catch up, pull, or merge upstream, or to check whether upstream has
  shipped anything new. Handles release discovery, divergent release lines,
  conflict resolution, failure attribution, and landing on master.
allowed-tools: Bash, Read, Edit, Grep, Glob, Write, todo
---

# Syncing this fork with upstream

Merge published upstream releases into this fork, one at a time, verifying each.

Full background and the measurements behind these rules are in
`docs/UPSTREAM_SYNC.md`. This file is the procedure; read the doc when you need
the reasoning, or when something here does not match what you observe.

**Run these steps yourself. Do not report a plan back and wait for approval.**
Stop only for the two decisions marked STOP below.

## Rules that are not obvious

1. **The unit of sync is a published GitHub release, not a tag.** Upstream has
   tags that were never released and releases that never reached `master`. Ask
   `gh release list`. Getting this wrong once already cost 12 dropped commits.
2. **Release order is not ancestry order.** `vN+1` frequently does not contain
   `vN`. Check every unmerged release, not just the newest.
3. **Merge, never rebase.** We are 160+ commits ahead with 8 crates upstream
   does not have. Rebasing replays all of it against a target moving ~310
   commits/week, and destroys `rerere`'s recorded resolutions.
4. **Every test failure must be attributed before it is accepted**, by running
   it against a clean upstream worktree. Never assume a failure is pre-existing.
5. **Our invariant tests failing is them working**, not a merge error. Upstream
   does not enforce our prompt-token caps.

## Step 1: find what is unmerged

```bash
git fetch upstream --tags
gh release list --repo 1jehuang/jcode --limit 10
```

For each release, is it already in our history?

```bash
git rev-list --count HEAD..<tag>     # 0 means merged
```

Build the list of unmerged published releases, oldest first. Ignore tags with
no release. If the list is empty, say so and stop; there is nothing to do.

For each unmerged release, check whether a newer one already contains it:

```bash
git merge-base --is-ancestor <older> <newer> && echo contained || echo INDEPENDENT
```

If independent, confirm by content before deciding to merge both, since
upstream rebases and rebasing changes hashes while preserving patch-ids:

```bash
git log --oneline <newer>..<older>   # non-empty means it has unique work
```

Track the releases as a todo list, one item each, so progress is visible.

## Step 2: per release, merge and verify

Work one release at a time, oldest first. Do not batch them: a conflict is far
easier to judge against one release's worth of change.

```bash
git status --porcelain              # must be empty; commit unrelated work first
git config rerere.enabled true
git checkout -b sync/upstream-<tag>
git merge-tree --write-tree HEAD <tag>    # preview: CONFLICT lines are the cost
git merge --no-commit <tag>
```

If `.git/index.lock` exists, another agent shares this checkout. Wait and retry
rather than deleting it.

Find conflicts with the `grep` tool, pattern `^<<<<<<<|^=======$|^>>>>>>>`.

Resolve on merits, not by side:

- **Their prompt and schema copy usually wins.** An expanded description or a
  widened enum is generally deliberate and informed by their evals.
- **Our tests usually win**, where they assert intent rather than exact
  sentences.
- **Version bookkeeping** (`Cargo.toml`, `Cargo.lock`, `changelog/index.json`):
  keep the highest version, and keep *all* release entries rather than letting
  one replace another.
- **A conflict naming a tool we deleted** (`agentgrep`, `multiedit`, `patch`) is
  a signal to check the surrounding code for the same stale reference. That is
  how the restricted-profile bug was found.

**STOP and ask** if a conflict requires deleting one of our features, or if
upstream has independently reimplemented something the fork already replaced.

### Ordering conflicts are behaviour

If both sides add a step to the same function, do not reason about the order,
**test it**. Ask what each step reads and what it mutates; if an earlier step
rewrites state a later one inspects, the order is load-bearing. Write a test
naming the ordering, then verify by inverting the code and watching it fail.

This is not hypothetical: both orderings of the `bash.rs` merge compiled, and
one silently disabled command interception entirely.

## Step 3: verify

```bash
cargo build --profile selfdev -p jcode --bin jcode
cargo test -p jcode-app-core --profile selfdev
cargo test -p jcode-base --profile selfdev
cargo test -p jcode-tui --profile selfdev
```

`jcode-tui` has a population of pre-existing failures. Do not eyeball the count,
**diff the failure sets**:

```bash
# before, from the pre-merge commit
git worktree add -q /tmp/premerge <pre-merge-sha>
cd /tmp/premerge && cargo test -p jcode-tui --profile selfdev --lib 2>&1 \
  | sed -n '/^failures:$/,/^test result/p' | grep "^    tui::" | sort > /tmp/before.txt
# after, from the merge
cd <repo> && cargo test -p jcode-tui --profile selfdev --lib 2>&1 \
  | sed -n '/^failures:$/,/^test result/p' | grep "^    tui::" | sort > /tmp/after.txt
comm -13 /tmp/before.txt /tmp/after.txt   # NEW failures: must be empty
git worktree remove /tmp/premerge --force
```

Attribute any other failure against a clean upstream worktree before accepting
it:

```bash
git worktree add /tmp/upcheck <tag>
cd /tmp/upcheck && cargo test -p <crate> --profile selfdev --lib -- <test-name>
git worktree remove /tmp/upcheck --force
```

If our token-cap tests fail on their new descriptions, judge each violation. Add
genuinely load-bearing text to the test's `EXEMPT` list **with inline
justification**; shorten the rest. Any exemption list needs a guard that fails
when an entry stops matching a real parameter.

## Step 4: commit and land

The merge commit is where a future reader learns why each conflict went the way
it did. Record, per conflict, which side won and what argued for it; any
ordering decision and the evidence; and test results with every pre-existing
failure named and attributed.

```bash
git checkout master
git merge --ff-only sync/upstream-<tag>
git branch -d sync/upstream-<tag>
```

Repeat from step 2 for the next release. Push once, after the last one:

```bash
git merge-base --is-ancestor origin/master HEAD && git push origin master
```

**STOP and ask** if the push would not be a fast-forward.

## Step 5: report

State which releases landed, the conflicts and how each was decided, anything
found along the way, test results with failures attributed, and what upstream
shipped that matters to us.

Confirm the end state:

```bash
gh release list --repo 1jehuang/jcode --limit 5
git rev-list --count HEAD..<latest-release-tag>    # expect 0
```
