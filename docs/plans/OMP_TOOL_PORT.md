# Plan: port omp's file tools to Rust, behind omp's tests

Status: **draft, not started.** Written 2026-08-07.

Companion to the oh-my-pi survey in `~/NLFCODE.md`. That file holds the research
and the corrections; this file holds the plan.

## Why

jcode's file tools keep breaking in the same place. The record, measured rather
than felt:

- 96 commits to `crates/jcode-app-core/src/tool/` in the last two weeks, **23 of
  them `fix`**. A quarter of all activity in that directory is repair.
- `agentgrep.rs`: 5 fixes in 14 commits. `bash.rs`: 6 in 17. `read.rs`: 5 in 7.
- `b4d873b66 fix(agentgrep): honor file field for grep/find scope (fixes #538)`
  fixed "scope grep to one file". **It is broken again today**, by a different
  mechanism, and that is the bug that prompted this plan.

The second point is the load-bearing one. #538 was someone applying care at the
layer the bug appeared in. It regressed because the underlying model — *walk a
tree, then filter the results* — is wrong at the root. More care at the same
layer cannot fix a wrong layer.

Meanwhile omp has solved these problems, in the open, with tests, and the tests
name our exact failures:

- `grep keeps explicit files exact` — our live bug, pinned.
- `search resolves bracketed literal paths (Next.js routes) when they exist`
- `selectors never choose the search root` (stated in their `grep.md` contract)

**The decision: adopt omp's behaviour as the specification, port their tests
first, then port the implementations to Rust.** Our own tests are consulted only
for jcode-integration signals (below); they are not the authority on behaviour.

## Scope

**In:** `read`, `write`, `edit`, `multiedit`, `patch`, `apply_patch`, `ls`,
`agentgrep`, `grep`, `glob`. Plus the hashline format and its snapshot store,
plus a bash interceptor.

**In, because hashline forces them to be** (see 3c for the full trace): our
`@file` mention expansion, the TUI diff rendering path, and `bash`'s
interaction with snapshot invalidation. These are not file tools, but hashline
is a contract they participate in, and omitting any of them produces a model
that addresses lines under tags nothing minted.

**Out:** `communicate` (3,351 lines, deep swarm/server coupling), `todo`, `bg`,
`ambient`, `session_search`, `discover`, and every other tool in
`app-core/src/tool/`. They are not the problem and moving them is a different
project.

**Explicitly deferred:** `lsp`, `eval`, `ast_grep`/`ast_edit`, DAP `debug`. Each
is a capability we lack rather than a tool we do badly. Judge separately.

### Search: the mapping is not one-to-one, and two modes have no counterpart

Our search surface is one tool, `agentgrep`, with four modes, plus two thin
adapters (`grep`, `glob`) that translate Claude-Code parameter names onto it.
omp's is two separate tools. They do not line up:

| our surface | omp counterpart | disposition |
|---|---|---|
| `agentgrep` mode `grep` | `tools/grep.ts` (74 KB) | **port** |
| `agentgrep` mode `find` | `tools/glob.ts` (26 KB) | **port** — this is what "find" maps to |
| `grep` adapter | `grep` is a real tool there | port; adapter layer disappears |
| `glob` adapter | `glob` is a real tool there | port; adapter layer disappears |
| **`agentgrep` mode `outline`** | **nothing** | **decide — see below** |
| **`agentgrep` modes `trace`/`smart`** | **nothing** | **decide — see below** |

**omp has no outline or trace.** Searching their whole tree for `outline` or a
`trace` tool returns only unrelated hits (a brush vendor file, a claude-trace
CLI, a metaharness reporter). Their structural-navigation story is different:
`ast_grep` over tree-sitter for structure, and `read`'s summarization mode
(declarations kept, bodies elided) for "what is in this file".

So **"port omp's search tools" implicitly means deleting outline and trace.**
That is a real decision and it must be made deliberately, not discovered in
Phase 3. Options:

1. **Keep them as jcode extensions.** They are ours, they have no omp
   equivalent, and `trace` in particular has no substitute in the omp model.
   Cost: we maintain search code omp's tests do not cover, which is exactly the
   situation this plan exists to escape.
2. **Drop them, replace with `read`'s structural summary.** omp's summarized
   read genuinely covers much of what `outline` is for, and it arrives free with
   Phase 3b. Does **not** cover `trace`.
3. **Drop `trace`, keep `outline`** until `ast_grep` lands, then reconsider.

**Recommendation: 3.** `outline` is cheap and its job is real; `trace` is a
bespoke relationship DSL with no upstream, no omp tests, and it is the mode most
likely to rot. But this is a product call — flagging it, not deciding it.

Note the sequencing consequence: if we ever adopt `ast_grep` (currently
deferred), it subsumes most of `outline`, so option 3 should be revisited then
rather than treated as permanent.

### Three different things called "find"

Worth stating because the word is overloaded and it caused confusion already:

- **our `agentgrep` mode `find`** — ranks file *names*. Maps to omp's `glob`.
- **shell `find`** — Phase 4 intercepts `find|fd|locate` and redirects to
  `glob`, matching omp's default interceptor rules.
- **omp's `fd` native module** — a walker inside their glob implementation, not
  a tool the model can call. Nothing to port.

There is no tool named `find` on either side after this plan; `glob` is the
name that survives.

## Measurements this plan rests on

| thing | number | source |
|---|---|---|
| our whole `tool/` dir | 47,512 lines, 602 test fns | `wc -l`, `grep -c` |
| our nine file tools | **3,790 lines** | `wc -l` |
| their couplings into `app-core` internals | **13 references total** | `grep -o 'crate::[a-z_]*'` |
| whole `tool/` dir couplings | 126 × `crate::todo`, 73 × `storage`, 49 × `goal`, 40 × `background` | same |
| omp `packages/hashline/src` | 44 files (~250 KB TS) | GitHub tree API |
| omp `packages/hashline/test` | **12 files, 137 KB** | same |
| omp `coding-agent/test/tools/` | 170 files, 1.66 MB (most irrelevant) | same |
| omp file-tool tests we care about | **~35 files, ~250 KB** | filtered |

The 13-vs-388 coupling gap is why the file tools can be extracted and the rest
cannot.

## Phase 0 — stop the bleeding (½ day, do first regardless)

These are known-broken today and block honest measurement. Full detail in
`~/NLFCODE.md`.

1. **`multiedit` partial writes.** Preflight all edits against original content;
   write nothing unless every one resolves. Note omp's own patcher comment:
   *"Commits are non-atomic"* — for one file preflight does give
   all-or-nothing, so say **preflight**, not atomic.
2. **Four duplicate `generate_diff` copies** (`edit.rs:180`, `patch.rs:295`,
   `apply_patch.rs:341`, `multiedit.rs`) each doing `change.value().trim()`, the
   bug already fixed once in `ui_diff.rs` (`3dd611ba0`). Consolidate to one
   helper with the common-prefix dedent.
3. **agentgrep file-scoping.** `resolved_search_scope` maps a file path to
   `root=parent, glob=filename`; ripgrep's `-g` filters matches but does **not**
   stop the walk, so `~/NLFCODE.md` walks all of `$HOME`, hits TCC-protected
   dirs, and exits 2. Reproduced: same query, root=parent → `EXIT=2`, file as
   direct target → `EXIT=0`. Also fix the exact-file filter matching on bare
   `file_name()`, and the unfollowable "re-run with a narrower `path`" advice.

**Exit:** three failing tests written first, then green. These land in the
current crate; they are not blocked on any extraction.

## Phase 1 — extract the file tools into their own crate (1-2 days)

Create `crates/jcode-tool-files`, depending on `jcode-tool-core` (which already
owns `ToolContext`, `session_id`, `resolve_path`) and `jcode-tool-types`.

Move: `read.rs`, `write.rs`, `edit.rs`, `multiedit.rs`, `patch.rs`,
`apply_patch.rs`, `ls.rs`, `agentgrep.rs` (+ `agentgrep/`), `grep_glob.rs`, and
their tests.

The 13 couplings to break:

| import | count | disposition |
|---|---|---|
| `crate::util` | 4 | move `truncate_str` etc. to `jcode-tool-core` |
| `crate::bus` | 3 | **the real decision** — see below |
| `crate::logging` | 2 | move or take a small logging facade |
| `crate::storage` | 1 | inspect; likely a path helper |
| `crate::session` | 1 | inspect |
| `crate::message` | 1 | inspect |

**The `bus` coupling is the one to think about.** Tools publish `FileTouch`
events that `server/file_activity.rs` consumes for swarm coordination. Options:
(a) move the bus types to `jcode-tool-types`, (b) define a narrow
`FileEventSink` trait in `jcode-tool-core` and inject the bus implementation
from `app-core`. **(b) is better** and pays off in Phase 3, because the hashline
snapshot store wants the same shape.

**Registration stays in `app-core`.** `Registry::base_tools` keeps calling
`ReadTool::new()` etc., now from the new crate. No behaviour change, no
re-registration, no alias changes.

**Abort condition (per the user's instruction):** if this turns out to be more
than a mechanical move — circular deps, a coupling that drags in the server, or
anything requiring redesign — **stop and leave the tools where they are.** The
port in Phases 2-4 does not depend on the extraction; it only benefits from the
faster test loop. Record what blocked it and move on.

**Exit:** `cargo test -p jcode-tool-files` runs the file-tool tests without
compiling the server. Full suite unchanged.

## Phase 2 — port omp's tests as the specification (3-5 days)

**This phase produces failing tests and no implementation.** That is the point:
it converts "port and hope" into "port and verify", and it tells us on day three
rather than week three whether the behaviours are compatible with jcode.

### What to port, in priority order

**Tier 1 — hashline core** (`packages/hashline/test/`, 12 files, 137 KB). Pure
functions over text; the cleanest possible port and no jcode coupling at all.

| file | KB | pins |
|---|---|---|
| `core-contracts.test.ts` | 10.8 | the fundamental invariants — **start here** |
| `patcher.test.ts` | 23.3 | preflight, commit, mismatch, path recovery |
| `snapshots.test.ts` | 6.5 | store, read fusion, `seenLines` merging |
| `format-v2.test.ts` | 5.4 | tag computation, normalization, headers |
| `file-ops.test.ts` | 2.6 | `REM`, `MV`, relocate |
| `leniency.test.ts` | 11.6 | what the parser forgives |
| `landing-shift.test.ts` | 9.2 | insert landing positions |
| `clipboard.test.ts` | 7.8 | `CUT`/`PUT @name` registers |
| `recovery-session-chain.test.ts` | 10.8 | 3-way merge across an edit chain |
| `boundary-repair.test.ts` | 30.4 | **the forgiveness layer — Phase 5, not now** |
| `block.test.ts` | 20.7 | `N*` block ops — **needs tree-sitter, defer** |
| `diff-preview.test.ts` | 2.1 | rendering |

**Tier 2 — tool behaviour** (the ones naming our bugs):

- `tools/grep-path-lists.test.ts` (34.9 KB) — **contains `grep keeps explicit
  files exact`**, our live bug. Also multi-path, quoted paths, spaces,
  cwd-relative formatting.
- `edit/seen-line-guard.test.ts` (18.3 KB) — the whole provenance model.
- `edit/file-snapshot-store.test.ts` (4.2 KB)
- `core/hashline.test.ts` (25.0 KB) — **the tool-level hashline contract**, as
  opposed to the library-level contracts in `packages/hashline/test/`.
- `core/hashline-loop-guard.test.ts` (7.6 KB) — the no-op escalation. Pins the
  behaviour behind their issue #2081 (182 identical no-ops in 205 calls).
- `write-hashline-header.test.ts` (4.8 KB) — `write` as a producer.
- `tools/multi-grep-path.test.ts`, `tools/glob-validate-paths.test.ts`,
  `tools/glob.test.ts`, `tools/search-url-paths.test.ts`
- `read-multi-range.test.ts`, `read-summary.test.ts`,
  `read-column-truncation-snapshot.test.ts`, `read-raw-range.test.ts`,
  `read-directory-range.test.ts`
- `write-hashline-header.test.ts`, `write-read-selector-misfire.test.ts`,
  `write-shebang-chmod.test.ts`
- `edit-diff.test.ts`, `edit-patch-unchanged-error.test.ts`,
  `edit-snapshot-details.test.ts`
- `tools/path-utils-dotdot-selector.test.ts`,
  `tools/path-literal-colon-selector.test.ts`,
  `tools/split-internal-url-sel.test.ts`, `tools/root-path-alias.test.ts`,
  `tools/windows-drive-alias.test.ts`
- `tools/bash-interceptor.test.ts` (11 KB) — for Phase 4.

**Tier 3 — do not port.** Anything ACP (`*-acp-*`), renderer/TUI
(`*-renderer.test.ts`, `edit-streaming-preview`), `xdev`, `internal-urls`,
`sqlite`, `archive`, `pdf`, `notebook`, `ssh://`, `conflict://`. These test
surfaces we do not have and are not adopting. **Roughly 80% of
`test/tools/` is out of scope**; the directory is 1.66 MB and our slice is
~250 KB.

### How to port a test

Read the TypeScript, extract the *assertion about behaviour*, write a Rust test
asserting the same thing against our interface. Do **not** transliterate. Their
tests construct `ToolSession` objects and call `executeHashlineSingle`; ours
will construct a `ToolContext` and call `EditTool::execute`.

Where a behaviour depends on a surface we lack (`xd://`, ACP bridge), **drop the
test and record why** in a `PORTING_NOTES.md` beside the tests. A dropped test
must be a decision, not an omission.

### Two corrections found while reading their tests

Both contradict what the survey in `~/NLFCODE.md` says, and both were found only
by reading the tests rather than the source:

1. **`enforceSeenLines` defaults to OFF.** Test:
   `"applies an edit on an unseen line when edit.enforceSeenLines is off
   (default)"`. The survey says the guard defaults on. It does not — the
   *patcher option* defaults `true`, but the *setting that feeds it* defaults
   off, so shipped behaviour is unguarded. **Decide deliberately which default
   we want**; do not inherit this one by accident.
2. **Column-clipped lines DO count as seen.** Test:
   `"marks column-clipped read lines as seen (clipped-line check removed)"`.
   The survey (and my earlier reasoning) said a clipped line must not count. omp
   tried that and **removed it**. Their 4 KB-line test asserts the edit applies.
   Follow them; the exclusion is a plausible idea that did not survive contact.

### Our tests: what to keep

Our 602 tool tests are not authoritative on behaviour, but a subset encodes
**jcode integration invariants that omp cannot know about**. Audit for these
signals and keep them:

| signal | files | why it matters |
|---|---|---|
| `working_dir` | 11 | session cwd resolution is ours |
| `parameters_schema` | 7 | provider schema shape |
| `ToolOutput` | 2 | our result shape |
| `Bus::global` | 1 | `FileTouch` swarm coordination |
| `ctx.session_id` | 1 | per-session state |

Plus, in `tool/tests.rs`, these registry invariants are **must-keep** and have
no omp equivalent:

- `tool_descriptions_stay_under_token_cap`
- `tool_parameter_descriptions_stay_under_token_cap`
- `tools_competing_with_bash_name_it_as_the_wrong_choice`
- `registered_tools_are_never_aliased_to_something_else`
- `every_alias_target_is_a_registered_tool`
- `tool_definitions_auto_inject_required_intent`
- `test_tool_definitions_are_sorted`

The alias invariants matter especially: per `~/NLFCODE.md`, a stale
`grep → agentgrep` alias silently broke regex search, and the same class of bug
existed for `task`/`Agent`. A port that renames or re-registers anything must
not reintroduce that.

**Exit:** a `#[ignore]`d or failing Rust test suite that encodes omp's
behaviour, plus `PORTING_NOTES.md` listing every dropped test with a reason.

## Phase 3 — implement (2-3 weeks)

Build against the Phase 2 tests. Keep the existing implementations registered
and working throughout.

### 3a. `jcode-hashline` crate (new, no I/O)

Mirrors `packages/hashline`: parser, applier, snapshot store, mismatch types.
Pure, testable in ~1s, upstreamable, zero conflict surface.

Port order, following their file structure:

| ours | theirs | notes |
|---|---|---|
| `format.rs` | `format.ts` (6.1 KB) | `xxHash32(normalized) & 0xffff`, 4 upper hex. Normalization trims trailing `[ \t\r]` per line |
| `normalize.rs` | `normalize.ts` (1.4 KB) | LF, BOM strip/restore, line-ending detect |
| `types.rs` | `types.ts` (8.3 KB) | `Edit`, `Cursor`, `Anchor`, `ApplyResult` |
| `parser.rs` | `parser.ts` + `tokenizer.ts` (48 KB) | grammar in `grammar.lark` (717 B) is the spec |
| `apply.rs` | `apply.ts` **minus** boundary repair | the splice itself is small |
| `snapshots.rs` | `snapshots.ts` (10.4 KB) | LRU: 30 paths, 4 versions/path, 64 MiB |
| `patcher.rs` | `patcher.ts` (31.8 KB) | preflight/commit, seen-line guard, recovery |
| `mismatch.rs` | `mismatch.ts` (5.1 KB) | two distinct rejection messages |
| `messages.rs` | `messages.ts` (26.7 KB) | error text is behaviour; models read it |

**Deliberately deferred to Phase 5:** `apply.ts`'s boundary repair (~40 KB of
its 55 KB), `block.ts` (needs tree-sitter), `clipboard.ts` if registers prove
low-value.

Details that are easy to get wrong, all confirmed by reading their source:

- **Hash the bytes the filesystem reports were written, not the intended text.**
  Formatters and write hooks transform content on save; hashing intent records a
  tag for content that no longer exists. Report drift as a one-line warning and
  leave the model-visible diff scoped to the intended hunk.
- **`commit` records the new snapshot with NO `seenLines`** — you just wrote it,
  you have seen it. Recording the edited lines instead would be *more*
  restrictive than omp and would break edit chains.
- **The edit response returns the new tag and new line numbers**, so a chain
  continues without re-reading. Large part of the token saving.
- **Dedup snapshots on full-text equality, not tag equality** (their issue
  #4075): two texts colliding on 16 bits must stay separate or `seenLines` from
  one is attributed to the other.
- **The seen-line guard runs only on the no-drift path**; on drift, recovery
  remaps anchors instead.
- **Reveal caps**: 40 lines, 512 columns. Over either cap, *nothing* merges into
  `seenLines`, so a model cannot piecewise-reveal past the guard.

### 3b. File tools, omp's model

- **`read`**: `[path#TAG]` header, `N:TEXT` lines, records `seenLines` for what
  it displayed. Selectors (`:50-200`, `:50+150`, `:5-16,960-973`, `:raw`).
  Structural summary for parseable code with no selector, with the recovery
  footer naming elided ranges. **Elisions must not enter `seenLines`.**
- **`grep`/`glob`**: **an explicit file path is a search target, not a root.**
  Scope before the walk. Search is a *producer*: record `seenLines` for matched
  and context lines shown. Permission errors on unreadable dirs are filtered,
  never a hard failure.
- **`edit`**: hashline as an **additional** addressing mode. String matching
  keeps working — a model that ignores tags must still succeed.
- **`write`**: records the content it wrote, so write-then-edit chains work.

### 3c. Everything hashline touches — the full blast radius

**Hashline is not a change to `edit`. It is a new contract between every surface
that shows the model file content and every surface that writes files.** Adding
it to `edit` alone produces a system where the model confidently addresses lines
under tags that nothing minted, which is worse than not having it.

This list was built by tracing the actual callers of omp's snapshot API in their
source (`grep -rln "recordFileSnapshot\|recordSeenLines\|getFileSnapshotStore"`),
not by reasoning about which tools "seem related". Two of them I would not have
guessed.

#### Producers — mint a tag and/or record `seenLines`

| omp producer | our counterpart | obligation |
|---|---|---|
| `tools/read.ts` | `read` | mint `[path#TAG]`, record displayed lines; **elisions and the `... N more lines` tail are NOT seen** |
| `tools/grep.ts` | `agentgrep`/`grep` | search is a producer: record matched + context lines actually shown |
| `tools/write.ts` | `write` | record what it wrote, so write-then-edit chains work; **also `invalidate()` on the paths it clobbers** |
| `tools/ast-grep.ts` | *(deferred)* | if `ast_grep` ever lands it must record too — noted so it is not forgotten |
| `tools/ast-edit.ts` | *(deferred)* | re-records post-apply snapshots on canonical keys |
| **`utils/file-mentions.ts`** | **our `@file` mention expansion** | **easily missed.** When a user's message inlines file content, that content is displayed to the model and must mint a tag, or the model addresses lines it saw with no valid anchor |
| `edit/hashline/execute.ts` | `edit` | records the post-commit snapshot (with **no** `seenLines` — see below) |

#### Consumers — verify against tags

- `edit` (hashline mode) — the patcher's preflight.
- Anything that reports "this file changed under you" for swarm coordination.
  Our `FileTouch` bus is the natural place, and Phase 1's `FileEventSink` trait
  is where the two meet.

#### Mechanisms that come with hashline and are not optional

Each of these exists in omp because a real failure forced it. Skipping any one
reproduces that failure.

1. **`canonicalSnapshotKey` — realpath canonicalization.** Different code paths
   reach the store by different spellings of the same file (`local://foo.md`
   resolves symlinks, the `[path#tag]` header does not; macOS `/tmp` vs
   `/private/tmp`). Without collapsing through `realpath`, **a freshly minted tag
   is rejected as stale because the lookup spelled the path differently.** New
   files fall back to realpath-of-parent + basename so creates and updates share
   a key. **This one will bite us on macOS immediately.**

2. **`SNAPSHOT_MAX_BYTES` (4 MiB).** A tag is a hash of the *whole* file, so
   minting one means holding the full normalized text. Files above the cap emit
   **no header at all** — line-anchored editing of multi-megabyte files is
   deliberately out of scope. We need the same cap and the same graceful
   degradation to string-match editing.

3. **The no-op loop guard** (`hashline/noop-loop-guard.ts`). A patch can apply
   cleanly and change nothing when the body is byte-identical to the target
   lines. Their issue #2081 recorded **182 identical no-op repeats in 205 calls**
   before the user aborted. The soft hint was not enough; after
   `NOOP_HARD_LIMIT = 3` consecutive byte-identical no-ops on a path they throw a
   hard `ToolError`, because the agent loop reacts to a *failure* where it
   ignores a *hint*. Per-path, keyed on payload hash; a different payload or a
   real commit resets the counter. **Port this with the format, not after it.**

4. **`recordSeenLinesFromBody`.** After an edit, the body rows the model just
   wrote are content it has demonstrably seen; parsing them back into the seen
   set keeps a chain going without a re-read.

5. **Deliberate asymmetry: `commit` records the new snapshot with NO
   `seenLines`.** You just wrote that content, so you have seen it. Recording
   the edited lines instead would be *more* restrictive than omp and would break
   edit chains. (Already noted in 3a; repeated here because it is the single
   easiest thing to "fix" into a bug.)

6. **`relocate(from, to)` on `MV`** so tags survive a rename, and
   `invalidate(path)` when a file is deleted or clobbered.

#### The gap omp leaves open, which we must decide about

**`bash` does not invalidate snapshots.** Confirmed by inspection: no
`fileSnapshotStore` reference anywhere in their `tools/bash.ts`. So a shell
command that rewrites a file leaves a stale snapshot in the store.

It is not a correctness hole for *them*, because the patcher re-reads the file
at apply time and compares hashes, so a stale tag is caught and rejected. It is
a **quality-of-error** issue: the model gets "file changed between read and
edit" with no hint that its own `sed` did it.

For us it matters more, because we have **multiple agents in one checkout** (see
the `Config::save()` story in `~/NLFCODE.md`). Options: invalidate on any
`bash` command that the interceptor recognizes as a writer, invalidate on
`FileTouch` events from other sessions, or accept the rejection and improve the
message. **Recommend: wire invalidation to the existing `FileTouch` bus**, which
we have and they do not — this is a place our architecture is genuinely ahead.

#### Session scoping, and subagents

The store hangs off the session object (`session.fileSnapshotStore`), created
lazily, aging out exactly with the session. **A subagent therefore gets its own
empty store**, so a parent's reads grant a child nothing and vice versa. That is
the correct behaviour and it falls out of the ownership model rather than
needing a rule. Our equivalent must key on `ctx.session_id` and must **not** be
a process-global.

#### Rendering and display

`edit/hashline/diff.ts` (16 KB) and `diff-preview.ts` exist because a hashline
patch is not a unified diff and cannot be rendered as one directly. Our TUI
(`jcode-tui-tool-display`, `ui_diff.rs`) will need a path for rendering a
hashline result. **This is exactly where the four duplicate `generate_diff`
copies from Phase 0 live**, which is another reason to consolidate them first.

#### Compaction and persistence

Snapshots are in-memory and session-scoped; they do **not** survive a restart,
and nothing persists them. A resumed session therefore has an empty store, so
the first edit after a resume must re-read. Worth pinning as a test so it is a
known property rather than a surprise, and worth an explicit line in the error
message when a tag is unrecognized after a resume (their `hashRecognized: false`
branch already says "never reuse one from a prior session").

#### The Anthropic OAuth curated-builtin path — a hard constraint

**Found late; the plan would have broken this.** On the Anthropic OAuth
(subscription) endpoint we do not advertise our own schemas. We advertise
*Claude-Code's builtin definitions*, hand-tuned, in
`crates/jcode-provider-anthropic/src/lib.rs:450-520`. The comment is explicit:

> the Anthropic OAuth (subscription) endpoint expects the builtin names with
> compatible schemas

The curated schemas are fixed, and `additionalProperties: false` on every one:

| curated | schema |
|---|---|
| `Read` | `file_path`, `offset`, `limit`, `pages` |
| `Edit` | `file_path`, **`old_string`**, **`new_string`**, `replace_all` |
| `Grep` | `pattern`, `path`, `glob`, `output_mode`, `-A/-B/-C`, `-i`, `type`, `head_limit`, `offset`, `multiline` |
| `Glob` | `pattern`, `path` |
| `Write` | `file_path`, `content` |

**Three consequences the plan must absorb:**

1. **A hashline-only `edit` cannot be advertised on this path.** The curated
   `Edit` requires `old_string`/`new_string` and forbids extra properties, so an
   `input`-shaped hashline payload is unrepresentable. This is independent
   justification for the plan's existing "keep string matching working" rule —
   it is not merely a courtesy to models with old priors, it is **required for
   OAuth sessions to edit at all**.
2. **`read` output changes are safe; `read` *schema* changes are not.** Adding
   the `[path#TAG]` header changes the tool *result*, which is uncurated and
   fine. Adding a selector *parameter* (`path: "foo.ts:50-200"`) is fine too,
   since it rides inside the existing `file_path` string. But adding a new
   top-level property would be dropped by curation on this path.
3. **`has_backing` (`lib.rs:440`) drops any curated entry with no matching local
   tool name.** Per `~/NLFCODE.md`, this already silently removed `Glob`/`Grep`
   for a whole release, and a stale alias then bypassed the new grep tool into a
   literal search. **Any rename or re-registration in Phase 6 must keep the
   names `read`, `write`, `edit`, `grep`, `glob` exactly**, or they vanish from
   the advertised toolset on OAuth with no error.

The plan's Phase 2 already keeps `registered_tools_are_never_aliased_to_something_else`
and `every_alias_target_is_a_registered_tool`. **Add a third:** an OAuth-path
test asserting every curated entry still has backing after the swap. That is the
invariant which actually failed before.

**Design consequence for hashline addressing:** the format has to reach the model
through a parameter the curated schema already permits. Options: a distinct
`edit` sibling tool advertised only off the OAuth path, or hashline carried
inside `new_string` with a sentinel, or accept that OAuth sessions use string
matching and hashline is an API-key-path feature. **Decide this in Phase 2, not
Phase 3** — it changes what the tests can assert.

#### Two more jcode-side constraints, both verified

**OpenAI strict mode.** `tool/tests.rs:1752`
(`only_the_known_open_world_tools_are_ineligible_for_openai_strict_mode`) pins
the exempt set to exactly `["batch", "browser", "initiative", "swarm"]` and
fails on any addition. Every ported file tool must therefore keep a
strict-compatible schema: no open-ended maps, no unconstrained
`additionalProperties`. omp's `read` takes a single `path` string with inline
selectors, which is strict-friendly — but if we were tempted to model selectors
as a structured object, that test would stop us. Good; it is doing its job.

**`ToolOutput` is a flat string, and there is no structured details channel.**
`crates/jcode-tool-types/src/lib.rs:2` — `output: String`, plus `title`,
`metadata`, `images`. Two implications:

1. **omp's tests assert on `result.details`** (e.g. `grep keeps explicit files
   exact` checks `details?.fileCount === 2` and
   `details?.scopePath === "alpha.txt, beta.txt"`). We have `metadata:
   Option<Value>` which can carry the same information, but **the port must
   decide where each asserted detail lands** — into `metadata`, or into the
   rendered `output` string that the assertion then greps. Doing this
   per-test-ad-hoc is how a port drifts. Decide once, in Phase 2.
2. **There is no streaming/partial tool output.** omp has
   `edit-streaming-preview`, `write-streaming-preview-expand`,
   `streaming-edit-abort`, and `edit/streaming-matcher-paths` tests. Those
   surfaces do not exist here. Already covered by the plan's "Tier 3 — do not
   port" rule, but worth naming explicitly so nobody tries: **all four streaming
   test files are out of scope**, and the corresponding `hashline/src/stream.ts`
   (3.8 KB) need not be ported.

#### The `batch` tool makes the snapshot store concurrently accessed

**This is the sharpest jcode-specific hazard in the whole plan, and omp has no
equivalent.** `tool/batch.rs:273` collects sub-calls into a
`futures::stream::FuturesUnordered` and drives them **genuinely concurrently**.
`ToolContext::for_subcall` (`jcode-tool-core/src/lib.rs:120`) clones
`session_id` unchanged — only `tool_call_id` differs.

So a single `batch` call can run several `read`s, or a `read` and an `edit`,
**against one snapshot store, in parallel**. omp's store is a plain
`LRUCache` with no synchronisation because their tool calls are sequential.

Consequences for the port:

- The store must be `Arc<Mutex<..>>` or equivalent, not the bare map omp uses.
  Their `InMemorySnapshotStore` is **not** a safe blueprint for concurrency.
- `record()` does read-modify-write on `seenLines` (union into an existing
  snapshot). Two concurrent reads of the same file **must not lose one's seen
  lines**. Their code mutates `existing.seenLines` in place, which is only safe
  under their single-threaded assumption.
- Batched `read` + `edit` on the same file is a genuine race: the edit may
  preflight against a snapshot the concurrent read is still widening. Decide
  whether to serialise writes per path, or to reject `edit` inside a batch that
  also reads the same path.

**Acceptance:** a test that batches N concurrent reads of one file and asserts
the union of `seenLines` is complete, plus a test batching a read and an edit of
the same path and asserting a deterministic outcome (either ordering is fine;
silent seen-line loss is not).

There is a second-order point here: `batch` is one of the four tools exempt from
OpenAI strict mode, so it is not going away, and any tool we port becomes
reachable through it.

#### Where the interceptor sits among the existing gates

`Registry::execute` (`tool/mod.rs:668-692`) already runs a **user-configured
`pre_tool` hook** that can block any call, and a `post_tool` observer at :555.
Adding the interceptor makes four checks over one `bash` call. Required order,
outermost first:

1. **`pre_tool` hook** — user policy wins over everything. Already first;
   leave it.
2. **Destructive-risk gate** — `rm -rf /` is refused as dangerous, never
   "redirected to a tool".
3. **Tool-preference interceptor** (new) — `cat` → use `read`.
4. **Execute.**

Two consequences:

- The interceptor must live where the destructive gate lives, so both see the
  same tokenization and the order is explicit rather than emergent.
- **A user's `pre_tool` hook must be able to override the interceptor**, because
  it runs first and a user who has deliberately allowed `cat` should not be
  second-guessed by us. This is the escape hatch the plan asks for, and it
  already exists — no new mechanism needed, just do not invert the order.

#### Verified as non-issues

Checked and found to need nothing:

- **Remote/server mode.** `Registry::empty()` is used by remote-mode clients
  that do not execute tools locally (`tool/mod.rs:175`), so tools always run
  where the filesystem is. The snapshot store therefore lives with the
  executing side and no cross-process synchronisation is needed.
- **MCP.** Our MCP tool (`tool/mcp.rs`) *consumes* external MCP servers; we do
  not re-export our own file tools over MCP, so there is no second schema
  surface to keep compatible.

### 3d. Prompt addendums and tool descriptions — **required, not optional**

Their descriptions are separate templated markdown
(`packages/coding-agent/src/prompts/tools/*.md`), rendered with variables like
`IS_HL_MODE` and `DEFAULT_LIMIT`. They are part of the behaviour: a tool whose
description does not teach the format will not be used correctly, and their
benchmark numbers include the prompt.

Port these, adapted to our tone and our token caps:

| source | KB | carries |
|---|---|---|
| `packages/hashline/src/prompt.md` | 6.0 | **the whole hashline spec taught to the model** — ops, worked examples, WRONG/RIGHT pairs, the three closing rules |
| `prompts/tools/read.md` | 2.1 | selector syntax, source kinds, "NEVER fabricate the tag", the elision rule |
| `prompts/tools/grep.md` | 0.6 | "MUST use this instead of shell grep/rg", "selectors never choose the search root" |
| `prompts/tools/glob.md` | 0.8 | when to prefer glob over grep |
| `prompts/tools/replace.md` | 1.4 | string-replace mode, for the fallback path |
| `prompts/tools/patch.md`, `apply-patch.md` | 2.8 / 2.9 | if we keep those tools |

Two hard constraints, both ours not theirs:

1. **Our token caps are enforced by tests** (`tool_descriptions_stay_under_token_cap`,
   `tool_parameter_descriptions_stay_under_token_cap`), and per `~/NLFCODE.md`
   four tools had already drifted past them. omp's `read.md` is 2 KB, far over
   our always-on budget. **Resolve deliberately:** either raise the cap with a
   recorded rationale, or split — a short always-on description plus the full
   format spec injected only when hashline is active. omp effectively does the
   latter via `{{#if IS_HL_MODE}}`.
2. **Keep the bash-avoidance language.** Our
   `tools_competing_with_bash_name_it_as_the_wrong_choice` test exists because
   of measured behaviour (zero bash calls across 20 exploration runs). omp's
   `MUST use this instead of shell grep/rg` says the same thing; do not lose it
   in the rewrite.

Also port the **system-prompt-level** guidance where it applies: the
"re-ground after every edit", "ranges are tight", "body = final content" rules
at the end of `hashline/src/prompt.md` are addressed to the model's whole
workflow, not just one tool.

## Phase 4 — bash interceptor (2 days, independently valuable)

**Promoted from "nice to have" to a hashline prerequisite.** The reasoning:
hashline's guarantee is that the model only edits lines a producer recorded as
displayed. `bash cat` output can never carry a tag, so every byte of file
content the model reads through the shell is content it may then try to address
with no valid anchor. Worse, `sed -i` mutates a file behind the snapshot store's
back. **An unscoped `bash` is a hole straight through the provenance model**, so
the interceptor is not a separate nicety — it is the wall that makes the rest
sound.

That is also why omp can afford `enforceSeenLines` defaulting off: their
interceptor already prevents the untracked-read path from existing.

### We already have most of the machinery

`crates/jcode-command-risk` (2,096 lines) exists and is **better suited to this
than omp's regex approach**. Do not write a third command parser.

What it already provides, verified by reading `tokenize.rs`:

| capability | where | why it matters here |
|---|---|---|
| `split_segments()` | `tokenize.rs:63` | splits on `&&`, `\|\|`, `;`, `\|`, newline, so chaining is not a bypass |
| **`Token::receives_pipe`** | `tokenize.rs:12` | **exactly omp's pipe exemption.** A stage consuming piped stdin cannot be replaced by a path-based tool, and we already track it |
| `Token::basename()` | `tokenize.rs:32` | `/bin/rm` matches `rm`; omp's regexes anchor on `^\s*cmd` and miss absolute paths |
| `is_truncating_redirect_target` | `tokenize.rs:15` | the `echo > file` → `write` rule, already modelled |
| `strip_heredoc_bodies()` | `tokenize.rs:92` | a heredoc body is data, not commands. omp's regex approach has no equivalent and would mis-fire on a fixture mentioning `grep` |
| `GateOutcome::{Allow,Reflect,Deny}` | `gate.rs:26` | a three-way outcome, richer than omp's boolean block |

`bash_destructive_gate.rs` (84 lines) is the thin `app-core` adapter onto it.

**We are structurally ahead of omp here.** Their `bash-interceptor.ts` is
regex-over-string with hand-rolled quote skipping; ours is a real tokenizer with
segment splitting and heredoc awareness. Port their *rule set* and their *test
cases*, not their matching technique.

### The rules to adopt

From `DEFAULT_BASH_INTERCEPTOR_RULES`:

| blocked | redirected to |
|---|---|
| `cat`, `head`, `tail`, `less`, `more` | `read` |
| `grep`, `rg`, `ripgrep`, `ag`, `ack` | `grep` |
| `find`, `fd`, `locate` (with `-name`/`-type`/…) | `glob` |
| `sed -i`, `perl -i`, `awk -i inplace` | `edit` |
| `echo`/`printf`/`cat <<` redirected to a real file | `write` |
| `nohup`, trailing `&`, dev servers, watch modes | `bg` (our `hub` equivalent) |

Constraints from their implementation and tests, all of which our tokenizer
already makes easy:

- **A rule only fires when the suggested tool is actually available.** Ours must
  check the registry, or a subagent with a restricted toolset gets told to use a
  tool it cannot call.
- **Piped stages are exempt** — use `receives_pipe`, do not re-derive it.
- Check both the original and the cwd-normalized command; strip leading
  `NAME=value` assignments before matching.
- `/dev/null`, `/dev/tty`, `/dev/stdout`, `/dev/stderr` are **not** real file
  redirects. We have already been bitten by exactly this: per `~/NLFCODE.md`,
  the `/dev/null` allowance sat behind an unreachable check and `2>&1` parsed as
  a redirect to a file named `1`.

### The interaction that needs care

This makes **two independent refusal paths over one command string**: the
destructive-risk gate and the tool-preference interceptor. Requirements:

1. **One tokenizer, two rule sets.** The interceptor is a new rule set inside
   `jcode-command-risk`, not a new parser.
2. **Distinguishable messages.** "This would destroy data" and "use the `read`
   tool instead" are different problems with different fixes. Conflating them
   is how a model ends up retrying a command it should have abandoned.
3. **Order matters.** Destructive check first: `rm -rf /` should be refused as
   dangerous, not redirected to a tool.
4. **`Reflect` vs `Deny`.** Tool-preference refusals are `Deny`-with-alternative
   (there is always a right tool), not `Reflect` (which asks the model to
   justify). Do not reuse the justification path here — a justification cannot
   make `cat` the right call.

### Escape hatch

There must be one, and it must be deliberate. A model genuinely needs
`tail -f`, `head -c`, or `cat` into a pipe sometimes. omp's answer is the
piped-stdin exemption plus config (`DEFAULT_BASH_INTERCEPTOR_RULES` is
user-overridable in settings). Ours should be: the pipe exemption, plus a config
key to disable individual rules, plus the tool-availability check. **No prose
override** — "the model said it needed to" is not a gate.

### Acceptance

- Every case in `tools/bash-interceptor.test.ts` (11 KB, 26 tests) ported and
  passing, including the negative ones: quoted text containing `>`, fd
  duplication (`2>&1`), `/dev` sinks, downstream pipe stages, and
  "does not block when the suggested tool is unavailable".
- The existing `jcode-command-risk` suite still green (2,096 lines including
  `assess_tests.rs` at 537 and `paths_tests.rs` at 219).
- Measured: bash calls for read/search in a read-only exploration stay at the
  **zero** baseline recorded in `~/NLFCODE.md`.

## Phase 5 — the forgiveness layer (2-3 weeks, gated on measurement)

`boundary-repair.test.ts` is 30 KB and `apply.ts`'s repair logic is ~40 KB:
boundary echo, one-sided echo, duplicate prefix/suffix, dropped structural
closers weighed against the whole-patch delimiter residual, and indentation
repair. It needs a delimiter-balance scanner that understands comments, strings,
templates, and JSX.

**Do not start this until Phase 3 has shipped and been measured.** A meaningful
share of omp's headline numbers plausibly comes from repairing near-miss
payloads rather than from anchoring alone, and we cannot separate the two from
published figures. Measure our own failed-edit rate before and after Phase 3,
then decide.

Two principles to keep even if we implement a smaller repair set:

- **When two readings are equally plausible, throw rather than guess.** They
  have dedicated errors for this (`ambiguousBoundaryEchoMessage`,
  `ambiguousCloserSpareMessage`).
- **Bias toward not repairing.** Constructs the scanner cannot classify are
  counted naively, which "can only suppress a repair (the safe direction), never
  force one".

## Phase 6 — swap (2-3 days)

Only when the Phase 2 suite is green against the new implementations.

1. Register new tools under the existing names.
2. Keep the old ones behind a config flag for one release, so a regression is a
   config change rather than a revert.
3. Run the acceptance check from `~/NLFCODE.md`: a read-only exploration of the
   codebase completes with **zero bash calls** (the measured baseline).
4. Measure failed-edit rate per session against the pre-swap baseline.
5. Delete the old implementations once the flag has gone unused for a release.

## Risks, named

| risk | mitigation |
|---|---|
| **Port is bigger than estimated** — read.ts alone is 143 KB | We are porting ~20% of it. Everything sqlite/archive/pdf/url/xdev is out of scope. If the *core* proves bigger than a week, stop and reassess. |
| **We inherit omp's bugs instead of ours** | Their tests encode their known bugs too. Net: their bug surface is better explored than ours, but this is a real trade, not a free win. |
| **602 of our tests discarded** | Most pin formatting, not semantics — and notably **none caught the file-scoping bug twice**. Keep the integration invariants listed in Phase 2. |
| **Token caps vs omp's long descriptions** | Explicit decision required in 3c; do not let it be discovered at test-failure time. |
| **`FileTouch` / swarm coordination breaks** | Phase 1 makes this an injected trait; Phase 2 keeps the `Bus::global` test. |
| **Rebase surface** | The file tools are ours and mostly additive; a new `jcode-hashline` crate cannot conflict at all. But `read`/`edit` are shared upstream files — per the maintenance plan, prefer upstreaming. |
| **Two refusal paths over bash** | Phase 4: one tokenizer (`jcode-command-risk`), two rule sets, distinguishable messages, destructive check first. |
| **Interceptor blocks a legitimate shell need** | Pipe exemption via the existing `receives_pipe`, per-rule config disable, and the tool-availability check. Deliberately no prose override. |
| **agentgrep is an external git dep** (`1jehuang/agentgrep`, tag `v0.1.6`) | The scoping fix belongs in *our* adapter, so no tag bump needed. The rg-exit-2 handling may want an upstream change; note it, do not block on it. |

## Sequencing summary

```
Phase 0  ½d    fix 3 live bugs                      (independent)
Phase 1  1-2d  extract crate                        (abandon if hard)
Phase 2  3-5d  port omp's tests → failing suite     (the gate)
Phase 4  2d    bash interceptor                     ← MOVED EARLIER
Phase 3  2-3w  implement: jcode-hashline + tools + prompts
Phase 5  2-3w  forgiveness layer                    (gated on measurement)
Phase 6  2-3d  swap behind a flag, measure, delete old
```

**Phase 4 now runs before Phase 3, despite the numbering** (kept for stable
cross-references). The interceptor is a prerequisite for hashline, not a
follow-on: without it, `bash cat` feeds the model untagged content and `sed -i`
mutates files behind the snapshot store, so the provenance model has a hole
through it on day one. It is also independently shippable, so it can land while
Phase 2's test port is still in progress.

Phase 0 is independent. Phase 2 is the gate: if omp's behaviours turn out to be
incompatible with jcode's session model, we find out there, cheaply, before any
implementation exists.

## Open questions for the user

1. **`enforceSeenLines` default.** omp ships it **off**. On is safer and is the
   whole point of provenance; off matches their shipped behaviour and avoids
   rejecting edits a model reasonably expected to work. Recommend **on**, with
   the reveal-and-retry path making rejections self-healing.
2. **Token caps for descriptions.** Raise the cap, or split always-on from
   format-spec-on-demand? Recommend the split, mirroring their `IS_HL_MODE`.
3. **Do we keep `patch` and `apply_patch`** once hashline exists? Three edit
   formats is a lot of surface. omp keeps `replace`, `patch`, `apply-patch`
   alongside hashline, so there is precedent for keeping them.
4. **`multiedit`'s fate.** Hashline sections are natively multi-edit. Keeping
   both may be redundant.
