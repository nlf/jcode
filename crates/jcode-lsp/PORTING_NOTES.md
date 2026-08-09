# Porting notes: omp's LSP tests

Companion to `docs/plans/LSP_TOOL_PORT.md`. **This file is the plan's Phase 1
exit criterion (a): enumerate before counting.** Both review passes objected to a
count asserted from a skim — first at 40, below the sum of its own groups, then
at an exact 56 with no case titles behind it. What follows is the titles.

## Progress

| group | target | ported | state |
|---|---|---|---|
| A framing and lifecycle | 14 | 14 | done, in `framing`/`transport`/`client` |
| B server→client requests | 7 | 7 | done, in `client` |
| C diagnostics freshness | 6 | 6 | done, in `freshness` |
| D position resolution | 5 | 5 | done, in `position` |
| G sanitization | 3 | 3 | done, in `display` — **did not need the render layer** |
| H dedup ledger | 9 | 9 | done, in `ledger` |
| F config and detection | 5 | 3 | `config`; F1/F2 deliberately unported (no cache to invalidate), F4/F5 belong with startup |
| write: `request` | 2 | 0 | needs the tool adapter |
| **v1 total** | **51** | **47** | |
| E `WorkspaceEdit` | 11 | 0 | v2 |
| write: `rename_file` | 4 | 0 | v2 |

**Tests written exceed cases ported**, deliberately. 179 lib tests plus 57
integration tests cover the 41 ported cases, because a behaviour omp asserts once
often needs two or three tests here: their fixtures assert an outcome where the
Rust version can also pin the *reason* (the error variant, what was consumed, what
the map holds afterwards). Several of those extra tests caught real defects; they
are noted inline where they did.

### "Done" above means ported and tested, not necessarily reached in anger

Worth stating plainly, because the table overstated it until now. Every module is
driven by its own tests; that is not the same as being on a live code path.

`freshness` and `ledger` were both ported, tested, and exported while being called
by nothing. For `ledger` that is just sequencing — its caller is the diagnostics
half of the tool adapter, which is blocked on Q2. For `freshness` it was worse than
sequencing: the client had no generation counter, so the module's central input did
not exist and no caller *could* have used it. Fixed in `95edc50e7`, which added the
counter and `observation_for`.

The general lesson, and the reason this section exists: a module with passing tests
and no caller looks finished from every angle except the one that matters.

**It then happened a second time, to the same group.** `freshness::equivalent_uris`
was tested, exported, and called by nothing: both client lookups did exact-string
`get`, so the C3 URI-equivalence case was unhandled on the hot path. I even *improved*
that function in `fe70e26a7` -- adding lexical path normalization to fix a real
divergence from omp -- without noticing that nothing called it. A reviewer found it.

Wired in `112b67b10`'s successor: `diagnostics_for` tries the exact key first, then
scans by equivalence, and `observation_for` goes through it.

So the check is not "does this module have tests" but "name the caller". Still
outstanding by that test: `ledger`, whose caller is the diagnostics half of the tool
adapter and is blocked on Q2.

**What remains for v1 is the two groups needing surfaces that do not exist yet**
(config loading for F, result rendering for G) and the tool adapter, which is
blocked on the one-tool-or-two decision.

---

Every case in omp's seven in-scope LSP test files is listed with a disposition:

- **port** — a behaviour we want, portable to our interface.
- **v2** — wanted, but it needs `WorkspaceEdit` application.
- **drop** — with a reason. A dropped test is a decision, not an omission.

Sources, all under `/tmp/omp/packages/coding-agent`:

| file | lines | cases |
|---|---|---|
| `test/tools/lsp-regressions.test.ts` | 3,451 | 75 |
| `test/tools/lsp-diagnostics-freshness.test.ts` | 704 | 15 |
| `test/lsp-format-options.test.ts` | 153 | 15 |
| `test/lsp-mux.test.ts` | 387 | 9 |
| `test/tools/lsp-diagnostics-dedup.test.ts` | 120 | 9 |
| `test/tools/lsp-batching.test.ts` | 182 | 6 |
| `test/lsp-render.test.ts` | 28 | 1 |
| **total** | **5,025** | **130** |

An 8th file, `test/task/subagent-lsp.test.ts` (293 lines, 6 cases), is excluded
wholesale: it tests subagent LSP inheritance against omp's session model.

## Not reviewed, and worth knowing about

Four adversarial review passes covered the crate; this is what they explicitly did
**not** reach, recorded from the reviewer's own list so it does not evaporate with the
session. None is a confirmed defect. Each is a place where "it has tests" and "it is
right" have not been checked against each other.

- ~~`fail_all` routes a transport death through `RequestFailure::Server`~~ — checked, it
  was real, fixed. Kept here rather than deleted so the list records that reviewing the
  *unreviewed* list found a defect on the first item tried.
- ~~`answer_channel` drops a failed answer to a *server* request silently~~ — checked, it
  was real, fixed. Two for two on this list.
- the router and answer tasks leak if `start()` fails after spawning them.
- `path_to_uri` does no percent-encoding, so a workspace root containing a space or `#`
  produces a URI a server may reject.
- a `TimedOut` freshness result discards the cached publish where omp returns it. A
  judgement call rather than a bug, but an unexamined one.
- `idle_timeout` is parsed and consumed by nothing, pending Q1.
- `warmupTimeoutMs` and `capabilities` in `defaults.json` are silently dropped by serde
  (they affect `marksman` and `rust-analyzer`). Unknown fields are tolerated by design,
  which is also how these went unnoticed.
- omp's `stripDiagnosticNoise` (`utils.ts:194`) is unported. It will matter when
  diagnostic formatting lands, because the ledger dedups *formatted* output.
- Windows beyond reading the `cfg!` branches; symlinked binaries; case-insensitive
  filesystems in detection.
- two tool calls sharing one `Client`: nothing tests `request()` interleaved with
  `open_document()` beyond the ten-sender transport test.

## Known gaps, named

Things verified to be wrong-or-absent and deliberately left, so that none of them is
indistinguishable from an oversight. Each has a test or a doc comment at the site.

| gap | where | why it is left |
|---|---|---|
| `biome` and `swiftlint` are not LSP servers | `config::detect` doc comment | omp attaches `createClient` adapters to both; `swiftlint lint --reporter json` prints and exits. We have no adapter layer, so detection reports them available and a spawn would die. The data is right, the adapter is missing |
| a decoy `content-length:` in a banner misframes | `framing::parse_content_length` doc, pinned by `a_decoy_header_in_a_banner_wins_the_scan_as_omp_does` | omp has the identical weakness. Requiring the *last* candidate would break the bare-LF recovery that function exists for. Measured: a 5-byte body is extracted |
| omp F1/F2 config-cache invalidation | `config_tests::a_reload_gap_is_recorded` | there is no cache to invalidate; the test asserts the premise, so adding one fails loudly |
| Windows local-bin filesystem layouts | Group F below | `.venv/Scripts` cannot be built on macOS. The suffix *ordering* is tested on every platform |
| symlinks in URI comparison | `freshness::equivalent_uris` doc | lexical normalization only, matching omp; resolving them needs the filesystem and omp does not either |

## The count, now derived rather than asserted

| group | port (v1) | v2 | drop | total |
|---|---|---|---|---|
| A framing and lifecycle | 14 | 0 | 3 | 17 |
| B server→client requests | 7 | 0 | 0 | 7 |
| C diagnostics freshness | 6 | 0 | 10 | 16 |
| D position resolution | 5 | 0 | 0 | 5 |
| E `WorkspaceEdit` application | 0 | 11 | 0 | 11 |
| F config and detection | 5 | 0 | 7 | 12 |
| G rendering and sanitization | 3 | 0 | 1 | 4 |
| H dedup ledger | 9 | 0 | 0 | 9 |
| — write actions (`rename_file`, `request`) | 0 | 6 | 0 | 6 |
| — out of scope entirely | 0 | 0 | 43 | 43 |
| **total** | **49** | **17** | **64** | **130** |

Group C's 16 is 15 from the freshness file plus one from the regressions file
(C5), which is why it exceeds its file's case count. Every one of the 130 is
placed in exactly one row: verified by script, no case cited twice and none
missing.

**v1 target: 49, not 56.** The plan's estimate was seven high, and every unit of
that came from the two discrepancies the second review predicted: Group C claimed
"port all (~10)" against a 15-case file that is mostly writethrough machinery
(6 portable, not 10), and Group A's 15 included three cases about surfaces we do
not have. **The number moved because the enumeration disagreed with the skim,
which is what the enumeration is for.**

Per the plan, a number may shrink against a recorded reason and never for
convenience. This is that record.

**Worth recording about the arithmetic itself.** The first version of this table
said Group C had 15 entries and the out-of-scope row 44. Both were wrong, by one
in opposite directions, so the total still came to 130 and looked right. It was
caught by scripting the reconciliation — checking that every case in every file
is cited exactly once — rather than by re-reading, and a table that sums
correctly from two compensating errors is exactly the kind of thing re-reading
does not catch.

---

## Group A — framing and lifecycle (14 port, 3 drop)

From `lsp-regressions.test.ts` unless noted. These are the client's own
lifecycle, and the last several are the ones nobody writes unprompted: each is a
hang or a silent success in production.

| # | case | line | disposition |
|---|---|---|---|
| A1 | sends the LSP exit notification after shutdown completes | 356 | **port** — the ordering is load-bearing: `exit` before `shutdown` returns leaves a server killed mid-write |
| A2 | returns an already-starting client without creating a second client | 394 | **port** — two concurrent `definition` calls must not spawn two `rust-analyzer`s |
| A3 | stops waiting for a pending client on caller abort without cancelling its initialization | 434 | **port** — one caller's timeout must not poison a shared cold start for everyone else |
| A4 | advertises workspace folder support during LSP initialization | 471 | **port** |
| A5 | supports long LSP timeouts up to the advertised ceiling | 290 | **port** — our cap is 300s; a schema advertising what the code rejects is `~/NLFCODE.md` item 4 |
| A6 | uses a custom server languageId for disk and in-memory document opens | 306 | **port** — a wrong `languageId` in `didOpen` makes some servers silently ignore the document |
| A7 | sendRequest respects an explicit timeoutMs and reports it in the error | 2617 | **port** — the message must name the timeout, or a slow server is indistinguishable from a wedged one |
| A8 | sendRequest uses the signal as the deadline when no explicit timeout is set | 2642 | **port** |
| A9 | shutdownClientInstance removes the client by identity and confirms process exit | 3002 | **port** — **by identity, not by name.** Removing by name evicts a *replacement* client spawned since |
| A10 | shutdownClientInstance reports a failed teardown when the process outlives the kill | 3028 | **port** — this is the daemon leak with no symptom |
| A11 | surfaces the process diagnostic when stdout closes before exit publication | 3055 | **port** — a server dying on startup (missing dylib, bad args) must say why, not time out |
| A12 | kills and evicts a client whose stdout reader fails while the process is alive | 3082 | **port** — a live process with a dead reader accepts writes and answers nothing |
| A13 | aborts a wedged cold-start initialize on the caller signal instead of the 30s internal fallback | 3158 | **port** |
| A14 | bounds a wedged notification flush on the caller signal and tears down the client | 3311 | **port** — the `FAKE_LSP_STOP_READING_ON` knob exists for this |
| — | does not negative-cache caller-aborted initialize attempts | 3189 | **drop** — needs the init-failure backoff cache, which v1 does not have. **Re-add with the backoff**, or an aborted first call blackholes the server for 3 minutes |
| — | does not tear down when a caller aborts before its queued write reaches flush | 3216 | **drop** — needs omp's write-queue model. Related to A14; revisit if we adopt a queue |
| — | surfaces an asynchronous stdin write rejection instead of resolving the notification | 3269 | **drop** — Bun `FileSink` semantics. Tokio surfaces write errors synchronously from `write_all` |

## Group B — server→client requests (7 port, 0 drop)

Port all. An LSP server asks the client things and blocks until answered; ignore
them and servers wedge. This is the largest single difference from our MCP client
and the reason it is a blueprint rather than a base class.

| # | case | line | disposition |
|---|---|---|---|
| B1 | answers workspace/workspaceFolders requests with the current folder set | 508 | **port** |
| B2 | sends initial workspace configuration after initialized before semantic requests (#5276) | 540 | **port** — their issue number: configuration arriving late means the first request runs unconfigured |
| B3 | answers missing workspace configuration sections with null in request order | 603 | **port** — **in request order**, and `null` rather than omitted. A reordered array silently gives a server the wrong section's settings |
| B4 | keeps the session alive when configuration is pulled after didChangeConfiguration | 645 | **port** |
| B5 | accepts dynamic capability registration before semantic requests | 700 | **port** — `client/registerCapability` arriving mid-handshake |
| B6 | drains every workspace/configuration pull during lazy cold start when a pull id collides with an in-flight request | 772 | **port** — **the sharpest test in the file.** Server→client and client→server ids are independent spaces, so a collision is legal and a shared pending map answers the wrong one |
| B7 | answers defined server→client requests with spec no-op results | 876 | **port** — an unanswered request blocks the server forever |

## Group C — diagnostics freshness (6 port, 10 drop)

From `lsp-diagnostics-freshness.test.ts` (15 cases) plus one from the regressions
file, so 16 entries. **The plan estimated 10 portable; there are 6.** The other 10
test the writethrough and deferred-diagnostics machinery this plan excludes by
name, so "port all" was never possible against this file. This is the discrepancy
the second review predicted and the largest single correction to the estimate.

| # | case | line | disposition |
|---|---|---|---|
| C1 | suppresses stale write diagnostics until the matching document version arrives | 354 | **port** — the correctness core. A naive read returns diagnostics for pre-edit content, which looks authoritative and is wrong |
| C2 | settles on the latest unversioned publish when the server never echoes a version | 398 | **port** — many servers never echo versions, so this is the common path, not the fallback |
| C3 | matches published diagnostics when the server renormalizes the document URI | 441 | **port** — a server may answer about a differently-spelled URI for the same file |
| C4 | matches Windows drive-letter case and percent-encoding differences | 472 | **port** — pure URI-equivalence logic, cheap, and it runs on any platform |
| C5 | does not reuse stale file diagnostics after another URI publishes (regressions:1471) | 1471 | **port** — a publish for file B must not satisfy a wait on file A |
| C6 | suppresses TypeScript project diagnostics for orphan files but keeps syntax errors | 651 | **port** — a file outside any `tsconfig` yields project-wide noise that is not about the file. Kept because the *shape* recurs (`rust-analyzer` on a standalone `.rs`), even though the codes are TS-specific |
| — | announces watched-file creates even when no server owns the file type | 131 | **drop** — `didChangeWatchedFiles` announcement, a writethrough concern |
| — | does not start an LSP server just to notify existing clients when write-time features are disabled | 152 | **drop** — writethrough |
| — | does not cold-start an LSP server for custom formatting when diagnostics are disabled | 179 | **drop** — formatting, out of scope |
| — | keeps an already-running LSP client synchronized after custom formatting | 208 | **drop** — formatting |
| — | waits for an already-starting LSP client without cold-starting another one | 234 | **drop as duplicate** — same property as A2, which is ported. Recorded rather than silently omitted |
| — | starts cold diagnostic initialization before custom formatting completes | 269 | **drop** — formatting |
| — | announces batched sibling writes before syncing the diagnostic target | 310 | **drop** — batching |
| — | returns completed pull diagnostics inside the inline write window | 486 | **drop** — writethrough timing window |
| — | returns promptly and delivers diagnostics via the deferred channel when the server is slow | 535 | **drop** — needs the post-turn delivery channel we do not have |
| — | returns the write tool result before slow diagnostics and queues them for the agent | 596 | **drop** — same channel |

**Note the pull-diagnostics case** (`reports pull diagnostics advertised through
${dynamicRegistration}`, regressions:1375). It is a template-literal title inside
a `for` loop over `[false, true]`, so it is one static declaration and two runtime
cases. Counted under Group B's dynamic-registration property (B5) rather than
double-counted here.

## Group D — position resolution (5 port)

| # | case | line | disposition |
|---|---|---|---|
| D1 | resolves the requested symbol occurrence on a line | 1152 | **port** — `name#N`, 1-indexed |
| D2 | throws when symbol does not exist on the target line | 1165 | **port** — must **error**, never silently fall back to the first non-whitespace column. A rename at a guessed position is a silent wrong rename |
| D3 | throws when occurrence is out of bounds | 1179 | **port** |
| D4 | resolves $-prefixed identifiers past compound matches | 2493 | **port** — word-boundary matching, so `id` does not match inside `uuid` |
| D5 | filters and deduplicates workspace symbols by query | 1193 | **port** — server-side `workspace/symbol` filtering is unreliable, so results are post-filtered |

## Group E — `WorkspaceEdit` application (11, all v2)

`sortAndValidateTextEdits` is 37 lines and every one is a bug someone had.

| # | case | line |
|---|---|---|
| E1 | dedupes byte-identical non-empty text edits before overlap validation | 2403 |
| E2 | keeps byte-identical zero-width inserts because they are not idempotent | 2424 |
| E3 | applies equal-position inserts in array order | 2432 |
| E4 | validates every file's edits before writing any workspace-edit file | 2442 |
| E5 | applies a create op followed by a text edit for the same URI in declared order | 2519 |
| E6 | flushes pending descendant text edits before a folder rename | 2293 |
| E7 | flushes pending edits queued against a rename target before performing the rename | 2346 |
| E8 | flushes pending descendant text edits before a folder delete | 2568 |
| E9 | round-trips file URIs containing percent and hash characters | 2481 |
| E10 | applies command-only code actions by executing workspace commands | 1241 |
| E11 | resolves code actions before applying edits | 1261 |

E9 is arguably Group A (URI handling), but it is here because the failure it
prevents is an edit landing in the wrong file.

## Group F — config and detection (5 port, 7 drop)

| # | case | line | disposition |
|---|---|---|---|
| F1 | workspace reload rediscovers LSP servers after an empty config was cached | 2716 | **port** — a cold start with no servers must not cache "none" forever |
| F2 | reload * invalidates the per-cwd config cache so newly written .omp/lsp.json is observed | 2797 | **port** |
| F3 | status distinguishes configured servers from started clients | 2766 | **port** — "configured, not started" vs a live client. The `status` action is this test |
| F4 | opens rust-analyzer Cargo workspace files before polling workspace readiness | 939 | **port** — **the server we actually test against** |
| F5 | skips rust-analyzer workspace polling for standalone Rust files | 1035 | **port** — a `.rs` outside a Cargo workspace must not wait out the readiness poll |
| — | detects Windows local .exe LSP shims in node_modules/.bin | 1637 | **drop** — Windows path resolution. Keep the *shape* of local-bin lookup; the `.exe`/`.cmd` variants are untestable here |
| — | detects Ruff in Windows virtualenv Scripts directories | 1661 | **drop** — same |
| — | detects Ruff in Windows virtualenv Scripts directories for Ruff-only roots | 1685 | **drop** — same |
| — | detects pyright and pylsp in Windows virtualenv Scripts for Python-only roots | 1713 | **drop** — same |
| — | loads config-only marketplace LSP servers from Claude plugin cache | 1779 | **drop** — omp's plugin marketplace, which we do not have |
| — | detects tlaplus files for LSP startup and language ids | 1746 | **drop** — startup discovery, plus a server not in our `defaults.json` |
| — | detects extensionless .emacs files for UI and LSP language ids | 1773 | **drop** — startup discovery |

**Windows was recorded as a gap here, and then half of it was closed.** jcode
supports Windows (`scripts/install.ps1`, `docs/WINDOWS.md`), so leaving binary
resolution unverified there was not acceptable as a permanent state.

Splitting the four dropped cases by what actually blocks them:

- the *filesystem layout* (`.venv/Scripts`, a `node_modules/.bin` full of `.cmd`
  shims) cannot be built on macOS, and those remain untested;
- appending `.exe`/`.cmd`/`.bat` to a candidate path is a string operation, and it
  is now tested. `config::local_candidates` returns the suffix list on every
  platform, with the extensionless path first, and
  `windows_executable_suffixes_are_enumerated_in_priority_order` asserts the whole
  list under `cfg!(windows)` and its absence otherwise.

So the ordering logic is verified everywhere and only the filesystem shape is
unverified. The original entry treated "we cannot test the layout" as "we cannot
test any of it", which was the easier reading rather than the true one.

**F1 and F2 are unported on purpose**, and the reason is recorded as a test
(`config_tests::a_reload_gap_is_recorded`) rather than only here. Both cases are
about invalidating a per-cwd config cache; `config::detect` has no cache and walks
the filesystem every call, so there is nothing to invalidate and a passing test
would only be asserting that an impossible bug is absent. The named test asserts the
premise instead — detection observes a project that appeared after the previous call
— so adding a cache turns these two cases live with a failure that points here.

**F4 and F5 are not in `config`** either. They are about rust-analyzer's workspace
readiness *after* a server is running, and this module deliberately spawns nothing.
They belong with startup, which does not exist yet.

## Group G — rendering and sanitization (3 port, 1 drop)

Not cosmetic: a diagnostic message goes through our TUI, and a server is free to
put tabs and control characters in it.

**Ported in `display`, and the "needs the render layer" blocker in the table above
was wrong.** omp's three cases run through a themed renderer and assert on the
rendered string, which is what made them look render-dependent. But every
assertion is about the *text*: no tab survives, the words are still separated, an
over-long error is bounded. Those are properties of a string function. The theme
and the terminal width contribute nothing to what is being checked, so waiting for
a render layer would have meant leaving a real safety property untested for the
sake of copying their test harness.

Recorded because the mistake is instructive: I read "renderer output" in a test
title and inferred a dependency the assertions do not have.

| # | case | line | disposition |
|---|---|---|---|
| G1 | sanitizes symbol metadata in renderer output | 1296 | **ported** — `display::inline` |
| G2 | sanitizes tabs in rendered diagnostic output | 1334 | **ported** — `display::block` |
| G3 | sanitizes expanded generic error output (#7041) | 1358 | **ported** — `block` + `truncate`; their case asserts length as well as tabs, so truncation is part of it |
| — | renders hover code through the cached theme highlighter (`lsp-render.test.ts`) | 14 | **drop** — omp's theme highlighter. Our TUI has its own; the *sanitization* property is what transfers |

Two deliberate divergences, both on the test file:

- **Tab width 4, not omp's 3.** Theirs is `DEFAULT_TAB_WIDTH` in
  `packages/utils/src/tab-spacing.ts`; ours matches
  `jcode-app-core/src/tool/tool_diff.rs`, so a diagnostic and a diff on one screen
  agree about how wide a tab is. No ported case asserts a width, only the absence
  of tabs.
- **Tab stops, not a fixed substitution.** omp replaces each tab with three
  spaces wherever it falls; we advance to the next stop. Theirs cannot misalign a
  column because it never claims to align one. Ours is tested for it
  (`a_tab_advances_to_the_next_stop_rather_than_a_fixed_width`), and that test
  fails against omp's own algorithm — which is the point of writing it down.

ANSI stripping was **not** reimplemented. `jcode-base` already had a stripper more
thorough than omp's regex, but `jcode-base` takes two minutes to compile and this
crate exists partly to keep its test loop at two seconds. Rather than duplicate the
parser or pay the compile, the function moved to a new leaf crate
(`jcode-text-sanitize`, 0.17s, no dependencies) and `jcode-base` re-exports it, so
no existing caller changed.

## Group H — dedup ledger (9 port, all of `lsp-diagnostics-dedup.test.ts`)

51 lines of implementation, directly aimed at context waste: a diagnostic already
reported for a file is not reported again, keyed on the message with its
`path:line:col` prefix stripped, so one that merely *moved* is suppressed.

| # | case | line |
|---|---|---|
| H1 | returns all messages unchanged the first time a file is reduced | 32 |
| H2 | fully suppresses an identical second reduce | 42 |
| H3 | suppresses diagnostics whose line and column shifted | 53 |
| H4 | returns only genuinely new messages and recomputes summary state | 62 |
| H5 | re-surfaces a diagnostic after it was removed | 79 |
| H6 | tracks files independently | 89 |
| H7 | strips path, line, and column while preserving diagnostic identity | 100 |
| H8 | distinguishes severity and code changes | 108 |
| H9 | falls back to the full message when the prefix is unparseable | 115 |

H5 is the one that makes the rest safe: suppression must not be permanent, or a
reintroduced error stays invisible.

## Write actions — 6, all v2

| # | case | line |
|---|---|---|
| W1 | rename_file applies LSP willRenameFiles edits and renames the file | 1864 |
| W2 | rename_file with apply:false previews edits without filesystem changes | 1972 |
| W3 | rename_file enumerates every file inside a directory rename | 2030 |
| W4 | rename_file skips the LSP loop when no configured server handles the file extension | 2676 |
| W5 | request action sends raw LSP method with auto-built textDocument/position params | 2104 |
| W6 | request action forwards explicit JSON payload verbatim | 2177 |

**W5 and W6 move to v1** with `request` itself, per the plan's Scope section.
Listed here because they sit among the write-action tests in omp's file. That
makes the true v1 count **51** and v2 **15**; the summary table above splits by
group rather than by shipped version, and this is the one place they differ.

## Dropped wholesale, with reasons

| what | cases | why |
|---|---|---|
| `lsp-mux.test.ts` | 9 | the broker-shared transport. We have no broker |
| `lsp-format-options.test.ts` | 15 | formatting, out of scope for v1 and v2 |
| `lsp-batching.test.ts` | 6 | writethrough batching |
| `test/task/subagent-lsp.test.ts` | 6 | subagent LSP inheritance, omp's session model |
| `capabilities action dumps server capabilities` (2235) | 1 | v2, with the `capabilities` action |
| reload cancellation and fallback (2877, 2911, 2942, 2971) | 4 | v2, with the `reload` action. **2971 is worth keeping in mind**: recognising `-32601` by code rather than by message text, because servers word it differently |
| `keeps ordinary backoff but lets explicit reload retry immediately` (3105) | 1 | v2, needs the backoff cache |
| glob diagnostic targets (1109, 1125, 284) | 3 | glob-scoped diagnostics, not in v1's single-file `diagnostics` |
| go.work workspace diagnostics (1557, 1593) | 2 | `file: "*"`, which we refuse by name and delegate to `bash` |
| `registers expert for .ex while keeping elixirls primary` (3444) | 1 | server-priority ordering for a server we do not ship |

## Method note

Titles extracted with a regex over `it(`/`test(` declarations, then read in
context to assign a disposition. Two things the regex alone gets wrong, both
worth knowing before trusting any count:

1. **`it.skipIf(...)` cases** need the modifier in the pattern or they vanish.
   `lsp-mux.test.ts` reports 0 cases without it, which is how the plan's first
   draft nearly recorded that file as empty.
2. **Template-literal titles** inside loops are one declaration and several
   runtime cases, so a static count and a runner's count legitimately differ.
   The regressions file is 75 static, 76 at runtime.
