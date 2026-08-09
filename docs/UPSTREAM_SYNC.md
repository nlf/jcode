# Syncing this fork with upstream

How to bring a new upstream release into this fork. Written after the v0.71.1
merge, which is used throughout as the worked example.

Upstream is `1jehuang/jcode`. This fork diverges substantially: eight crates
that do not exist upstream (`jcode-hashline`, `jcode-ast`, `jcode-bash-intercept`,
`jcode-patch`, `jcode-read`, `jcode-search`, and others), three tools deleted
(`agentgrep`, `multiedit`, `patch`), and a replaced edit path. That sounds like
it should make syncing painful. It does not, and the reason is worth
internalising before following the steps.

## Why this is cheaper than it looks

Measured at the v0.71.1 merge: 144 commits ahead, 78 files added, 6 deleted,
102 modified. **Three files conflicted.**

Additions and deletions are structurally conflict-free. A new crate touches
nothing upstream has, and deleting a file upstream never edits produces no
conflict either. The entire conflict surface is the set of files *both* sides
modified in place, which for us is a handful of tool schemas and descriptions.

The corollary is a design rule, not just an observation: **a feature that lives
in a new crate behind a small hook costs nothing at sync time; the same feature
edited into an existing hot file costs forever.** This is why the fork's tool
work is structured as separate crates with thin call sites in `bash.rs` and
`mod.rs`.

## Merge, do not rebase

Rebasing replays 144 commits against a target that moves ~310 commits/week. It
also destroys `rerere`'s ability to help, because every replay produces new
commit hashes and new conflict contexts.

Merging keeps our history intact, records each sync as one reviewable commit,
and lets `rerere` replay resolutions automatically. Enable it once:

```bash
git config rerere.enabled true
```

An earlier note in `NLFCODE.md` recommends rebasing. That advice was written
when the fork was 31 commits of TUI tweaks and is now obsolete.

## Cadence

Upstream runs ~310 commits/week (measured over 12 weeks: 232 to 718 per
fortnight). The v0.71.1 sync covered 4 days of drift and cost three small
conflicts. **Sync weekly.** Monthly means resolving against ~1200 commits of
drift, against code nobody remembers.

## Upstream's tags are not a linear chain

This is the trap, and it is specific to how upstream works. **Do not assume
`vN+1` contains `vN`, and do not walk tags in order.**

At the time of writing:

- `upstream/master` sat at `v0.71.1`.
- `v0.72.0` and `v0.73.0` both branched off `v0.71.1` **independently**, on two
  different agent branches, with 12 unique commits each and **no patch-id
  overlap**. Neither contains the other.
- A commit titled `chore(release): prepare v0.72.0` exists **twice**, with
  different hashes and different trees: `cc589f14c` (the tag) and `cd3112a55`
  (inside `v0.73.0`'s history).

So `v0.73.0` is not "v0.72.0 plus more". Merging tags sequentially would either
resurrect an abandoned line or silently drop real work. Always check the actual
topology before deciding what to merge.

Also note **tags lead `upstream/master`**. Watching only `upstream/master`
leaves you many commits behind without any signal. Fetch tags and inspect them.

## The process

### 1. Fetch and survey

```bash
git fetch upstream --tags
git log --oneline -1 upstream/master
git tag --sort=-creatordate | head -5
```

For each candidate tag, establish where it actually sits:

```bash
# Is it already in our history?
git rev-list --count HEAD..<tag>

# Is it a descendant of the last tag we merged?
git merge-base --is-ancestor <previous-tag> <tag> && echo linear || echo DIVERGENT

# What actually contains it? An orphaned tag names no branch, or only a
# stale release branch.
git branch -r --contains <tag>
```

If two tags are divergent, check whether one's work exists in the other before
concluding you must merge both:

```bash
# Empty output on both sides means genuinely independent work.
git log --oneline <newer>..<older>
```

For a stronger check that survives rebasing, compare patch-ids rather than
hashes: identical content rebased onto a new base keeps its patch-id but
changes its commit hash.

### 2. Start from a clean tree on a sync branch

An unrelated dirty file will abort the merge. Commit or stash it first, and
commit it *separately* so the merge commit stays reviewable.

```bash
git status --porcelain          # must be empty
git checkout -b sync/upstream-<tag>
```

### 3. Preview the conflict set before committing to anything

`merge-tree` performs the merge in memory and touches neither the working tree
nor the index. This is the single most useful step: it tells you the size of
the job in about a second.

```bash
git merge-tree --write-tree HEAD <tag>
```

Lines beginning `CONFLICT` are the whole cost. If the list is small and confined
to files you recognise, proceed.

### 4. Merge

```bash
git merge --no-commit <tag>
```

`--no-commit` keeps the merge open so nothing lands before the tests pass.

### 5. Resolve, judging each conflict on merits

Find them with the `grep` tool, not by scrolling:

    pattern: ^<<<<<<<|^=======$|^>>>>>>>

The defaults that held up in practice:

- **Upstream's prompt and schema copy usually wins.** If they expanded a
  description or widened an enum, it is generally deliberate and informed by
  their own eval data. At v0.71.1 they widened `feedback_loop_relevance` from 3
  to 5 variants and added `feedback_loop_traceability`; those variants are
  calibration, not padding, and taking our shorter rewording would have thrown
  away real signal.
- **Our tests usually win**, when they assert intent rather than exact
  sentences. Ours survived a rewording that broke the upstream-side assertions
  they replaced. This is an argument for writing them that way.
- **When both sides add a step to the same function, keep both** and think hard
  about the order. See the warning below.

### 6. Order is behaviour, not formatting

The one genuinely dangerous conflict class. At v0.71.1 both sides added a step
to `BashTool::execute`: upstream wraps `cargo` invocations in a shell preamble,
we refuse commands a dedicated tool does better.

Both orderings compile. Both look reasonable. But wrapping rewrites the command
string, and interception scans leading tokens, so **wrapping first disables
interception entirely** — verified by building it that way, where
`cat Cargo.toml` printed the whole file instead of redirecting to `read`.

When two sides add steps to one function, ask what each step reads and what it
mutates. If an earlier step rewrites state a later one inspects, the order is
load-bearing, and it needs a test naming the ordering so the next merge cannot
quietly invert it.

### 7. Verify, and separate ours from theirs

```bash
cargo build --profile selfdev -p jcode --bin jcode
cargo test -p jcode-app-core --profile selfdev
cargo test -p jcode-base --profile selfdev
```

**Any failure must be attributed before it is accepted.** Check it against a
clean upstream worktree rather than assuming:

```bash
git worktree add /tmp/upcheck <tag>
cd /tmp/upcheck && cargo test -p <crate> --profile selfdev --lib -- <test-name>
git worktree remove /tmp/upcheck --force
```

At v0.71.1 this cleared two `jcode-base` failures
(`spawn_detached_creates_new_session`,
`streaming_guard_creates_visible_macos_sleep_assertion`) as pre-existing
upstream, and it is the only reason we could tell they were not ours.

### 8. Expect our invariant tests to catch their changes

The fork holds quality invariants upstream does not, most notably the prompt
token caps in `crates/jcode-app-core/src/tool/tests.rs`. Upstream's verbose new
descriptions fail our parameter cap; at v0.71.1, upstream failed that test with
**8** violations.

That test failing is the invariant working, not a merge error. Resolve it by
judging each violation:

- If the text earns its cost, add it to the test's `EXEMPT` list **with an
  inline justification**, matching the idiom in
  `tool_descriptions_stay_under_token_cap`.
- If it does not, shorten it and expect to re-resolve that conflict on future
  syncs.

Any exemption list needs a guard that fails when an entry no longer matches a
real parameter, or it silently becomes a licence to exceed the cap.

### 9. Commit with the reasoning

The merge commit is the only place a future reader learns *why* a conflict went
the way it did. Record, per conflict, which side won and what argued for it;
any ordering decision and the evidence behind it; the test results, with
pre-existing failures named and attributed.

### 10. Land it

```bash
git checkout master
git merge --ff-only sync/upstream-<tag>
git push origin master
```

Push after every sync. The fork's history existing only on one machine is a
risk the `NLFCODE.md` notes already flagged once.

## Checklist

- [ ] `git fetch upstream --tags`
- [ ] Topology checked: is the tag a descendant, or divergent?
- [ ] Orphaned or superseded tags identified and skipped
- [ ] `rerere` enabled
- [ ] Working tree clean, sync branch created
- [ ] `merge-tree` preview reviewed
- [ ] Conflicts resolved on merits, not by side
- [ ] Ordering conflicts tested, not reasoned about
- [ ] Build green, tests run, every failure attributed
- [ ] Cap/invariant violations judged and justified inline
- [ ] Merge commit records the reasoning
- [ ] Merged to `master` and pushed
