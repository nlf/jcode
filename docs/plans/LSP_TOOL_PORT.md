# Plan: port omp's `lsp` tool to Rust, behind omp's tests

Status: **ready to implement (revision 3)**, written 2026-08-09 and revised twice
the same day against two adversarial reviews. Every measurement was re-taken and
independently confirmed. Both review passes and the errors they corrected are
recorded at the end rather than edited away. **Questions 1 and 2 at the end block
Phase 0 and need answering first.**

Companion to `docs/plans/OMP_TOOL_PORT.md`, which shipped the file tools by the
same method, and to the oh-my-pi survey in `~/NLFCODE.md`, which ranks `lsp` as
**recommendation 2** — the one genuine capability gap.

The method is inherited and not up for debate: **omp's behaviour is the
specification, their tests are the authority, and we reimplement in Rust rather
than porting code.** What that method cost and bought last time is recorded at
the end of `OMP_TOOL_PORT.md`; the most important line of it is that roughly
eight defects were found by running a live agent and *none* by tests. This plan
budgets for that.

## Why

`agentgrep`'s `outline` and `trace` modes were deleted in the file-tool port on
the explicit understanding that **LSP is their successor**
(`OMP_TOOL_PORT.md`, "outline / trace / smart: superseded, not lost"):

> **LSP** gives real symbol navigation — definitions, references, document
> symbols — which is what `trace` approximates textually and does worse.

So this is not a new capability being weighed on its merits for the first time.
It is a promise already made, against a capability already removed. Today jcode
has `grep` (text), `ast_grep` (structure), and nothing semantic: no tool follows
a re-export, resolves a shadowed binding, or knows that two identically spelled
identifiers are different symbols.

The expensive failure is rename. A cross-file rename by `ast_edit` or `sed`
silently drops callsites, and silence is the problem: the model reports success
and the build breaks somewhere the model never looked. omp's prompt states the
rule in exactly those terms, and it is worth copying verbatim as the reason this
tool exists:

> NEVER do a cross-file rename with `ast_edit`/`sed`/hand edits when `lsp`
> `rename`/`rename_file` can — text renames silently drop callsites.

### There was an `lsp` tool here before, and it was right to delete it

`crates/jcode-app-core/src/tool/lsp.rs`, deleted 2026-06-30 in `3d391a517`
("lsp was a default-disabled stub"). Read it: 94 lines, nine operation names, no
process, no JSON-RPC, no client. `execute` validated the path and returned the
string *"LSP is not integrated in jcode yet. Use grep or read to inspect
symbols."*

**This is the most useful piece of local history for this plan, and it is a
warning about scope rather than about LSP.** A tool that advertises a capability
it does not have is worse than no tool: it consumes schema tokens, invites the
call, and returns an apology. The deletion was correct.

The consequence for this plan is a hard rule: **no phase ships a registered
`lsp` tool that cannot actually answer the operations it advertises.** A partial
tool must advertise only the subset that works.

Two artifacts of the stub survive and must be reconciled:

1. `jcode-tui/src/tui/ui_tools.rs:1484` still has an `"lsp" =>` arm reading
   `operation`, `file_path`, `line`. Its parameter names are the **stub's**, not
   omp's (`action`, `file`, `line`, `symbol`), so left alone it renders every
   call as `lsp :0` — the `unwrap_or` defaults, not a blank.
2. `jcode-provider-claude-cli-runtime/src/lib.rs:1103` and `:1139` carry a live
   `"lsp"` ↔ `"Lsp"` name mapping in both directions. **This one is not dead
   code**: it is on the path that translates tool names for the Claude CLI
   runtime, so it already round-trips a name for a tool that does not exist.

The second was missed in this plan's first draft and found in review, which is
the same category of miss the file-tool port's review caught
(`OMP_TOOL_PORT.md`, "Surfaces keyed on tool name or `file_path`"). See "Blast
radius" below, which exists because of it.

## Scope

**In, v1:** a real stdio JSON-RPC client with process lifecycle; config loading
with auto-detection; the read-only actions that carry most of the value —
`diagnostics` (single file), `definition`, `references`, `hover`, `symbols`
(document + workspace), `type_definition`, `implementation`, `status` — **plus
`request`**, which needs no `WorkspaceEdit` machinery and is what makes an
incomplete v1 usable.

**`request` forces a write-tier tool into v1, and that is a real consequence
rather than a footnote.** It can send an arbitrary LSP method, so it cannot be
auto-approved. Under the two-tool split (question 2) it therefore belongs on
`lsp_edit`, which means **v1 ships two tools**: a read-only `lsp` and a minimal
`lsp_edit` carrying only `request`. That is more surface than "v1 is the
read-only half" suggests, and it changes what question 4 is asking: shipping v1
first still ships an approval-gated tool, just one with a single action.

If that is unwanted, the alternative is to drop `request` back to v2 and accept
that an unported operation is unreachable until then. **Recommend keeping it in
v1**: the whole reason to have it is that our v1 will not cover everything.

**In, v2:** the write actions — `rename`, `code_actions`, `rename_file` — plus
`reload` and `capabilities`. These are separated because they apply
`WorkspaceEdit`s, which is a second body of work (edit application, overlap
validation, resource ops).

**Out, and each for a stated reason:**

| omp piece | lines | why out |
|---|---|---|
| `mux/` (daemon, server, protocol) | 1,241 | a broker sharing one server across sessions. Real value at scale; we do not have the broker, and a private process per project works |
| `lspmux.ts` | 233 | detection of an external third-party multiplexer |
| `clients/` (biome, swiftlint, linter adapter, index) | 518 | CLI-shaped linters wearing an LSP-ish interface. Not LSP, and each is a bespoke JSON format |
| `writethrough.ts` | 561 | format-and-diagnose on every `write`/`edit`. **A separate feature** (see below) |
| `workspace-diagnostics.ts` | 170 | `file: "*"` shells out to `cargo check`/`tsc`/`go build`. That is `bash`, which we have |
| `deferred-diagnostics.ts` | 66 | needs a post-turn delivery channel we do not have |
| `format-options.ts` | 119 | formatting is not in scope for v1/v2 |
| `startup-events.ts` | 13 | a startup-warmup event channel we do not have |

**Total out: 2,921 lines.** The `clients/` figure is the whole directory (518),
not just the two named adapters (383) — the first draft counted only the two and
the arithmetic did not close.

**Deferred with intent, not dismissed:** `writethrough` is the single most
interesting thing in their LSP directory — every file write gets formatted and
diagnosed, so the model learns it broke the build *at the moment it broke it*
rather than at the next `cargo check`. It is a bigger behavioural change than
the tool itself and it belongs in its own plan, with its own measurement. Naming
it here so it is not silently lost, as `outline` nearly was.

**`file: "*"` workspace diagnostics: deliberately refused, not silently
unimplemented.** The action must return a message naming `bash cargo check` as
the thing to run. A silent empty result would read as "no problems in the
workspace", which is a lie. (This is the same class as the mutation-testing
finding in the file-tool port: a form refused as "unrecognized syntax" gets
retried identically, so refusals must name themselves.)

## The measurements this plan rests on

Every figure below was re-measured after the first draft got several of them
wrong. The corrections are recorded in the review-findings section at the end
rather than hidden, because two of them changed a conclusion.

| thing | number | source |
|---|---|---|
| omp `src/lsp/` TypeScript | **9,354 lines** across 24 `.ts` files | `wc -l $(find . -name '*.ts')` |
| plus `defaults.json` | 499 lines | `wc -l` |
| **in-scope subset** | **6,433 lines** (`client` 1,465, `tool` 1,352, `utils` 747, `render` 668, `config` 549, `diagnostics` 516, `types` 479, `servers` 296, `edits` 288, `diagnostics-ledger` 51, `index` 22) | same |
| **out-of-scope subset** | **2,921 lines** | the table above |
| omp lsp tests | **5,025 lines** across 7 files | `wc -l` |
| `lsp-regressions.test.ts` alone | 3,451 lines, **75 cases** | `it\(` count |
| all lsp test cases | **130** | same, across the 7 files |
| their `defaults.json` | **53 servers** | parsed the JSON |
| language servers on this machine | **2** (`rust-analyzer`, `/usr/bin/clangd`) | `which` |

6,433 + 2,921 = 9,354, so the split accounts for every line.

**The first draft claimed the tests were larger than the code and called that
"the strongest available evidence" for its difficulty model. That claim is
false.** In-scope code is 6,433 lines against 5,025 of tests. The honest version
of the point is narrower and still worth making: **the test-to-code ratio for
the in-scope subset is 0.78:1**, which for a protocol client is high, and the
content of those tests is overwhelmingly about servers misbehaving rather than
about the happy path (see the group breakdown in Phase 1). That is the asset the
port is for. But it is not the slam dunk the first draft asserted, and the
estimate below is scaled accordingly.

**Two language servers are installed, not one.** `clangd` ships with Xcode. That
is a second live-verifiable server for free, and it is a *different shape* from
`rust-analyzer` — no long project load, `compile_commands.json` rather than
`Cargo.toml` as its root marker. Worth using precisely because it is different.

## Where the code goes

Following the file-tool port's shape, which worked: a pure crate with a fast
test loop, plus a thin tool adapter in `app-core`.

| crate | contents | I/O? |
|---|---|---|
| **`crates/jcode-lsp`** (new) | JSON-RPC framing, client lifecycle, config load + auto-detect, `WorkspaceEdit` application, formatting of results | process + fs, but **no jcode types** |
| `crates/jcode-app-core/src/tool/lsp.rs` | schema, dispatch, `ToolOutput`, hashline recording | jcode-coupled |

`jcode-lsp` depends only on `tokio`, `serde`, `serde_json`, `anyhow`, and
`percent-encoding`/`url` (all already in the workspace lockfile). **No new
third-party crate is required.** I considered `lsp-types` and rejected it: it
models the entire protocol including everything we do not send, its enums are
exhaustive where real servers send unknown values, and hand-written structs for
the ~15 methods we use are smaller than the impedance matching. `tower-lsp` and
`async-lsp` are both server-side frameworks; we are the client.

The measured argument for the crate split is in `OMP_TOOL_PORT.md`:
`cargo check -p jcode-app-core --tests` is **24s** for a one-line change,
against **~1s** for `jcode-hashline`. Over a multi-week test-first port that is
the difference between a loop and a wait.

**Deviation from the file-tool port:** `jcode-lsp` cannot be pure. It spawns
processes and reads files. That makes it slower to test than `jcode-hashline`
and means the fake-server fixture (below) is load-bearing rather than a
convenience.

## Precedent to reuse: `jcode-base/src/mcp/client.rs`

We have already written a JSON-RPC-over-stdio client with request correlation.
`McpHandle` (`client.rs:18-115`, ~98 lines; the file is 511 with tests) has: an
`AtomicU64` id allocator, a `Mutex<HashMap<id, oneshot::Sender>>` pending map,
an `mpsc` writer task serialising outbound messages, and a per-request timeout.
That is the same skeleton `client.ts` needs.

**It is a blueprint, not a base class, and three differences are load-bearing:**

1. **Framing.** MCP uses newline-delimited JSON. LSP uses
   `Content-Length: N\r\n\r\n<body>`. Not interchangeable, and a length-prefixed
   reader has to handle a body split across chunk boundaries — the single most
   likely place for a subtle bug.
2. **Server-initiated requests.** MCP's client mostly speaks first. An LSP
   server *asks the client things* and blocks until answered:
   `workspace/configuration`, `workspace/workspaceFolders`,
   `workspace/applyEdit`, `client/registerCapability`. Ignore them and servers
   wedge. omp has five separate regression tests about this, including one where
   a server-request id **collides with an in-flight client request id** — which
   is legal, because the two id spaces are independent, and which a naive shared
   pending-map gets wrong.
3. **Unsolicited notifications.** `publishDiagnostics` and `$/progress` arrive
   whenever. Diagnostics are the tool's main product and they are *pushed*, so
   the client needs a cache and a wait-for-fresh mechanism, not a request path.

Do not try to generalise the MCP client into a shared JSON-RPC crate as part of
this work. Read it, copy the shape, keep them separate. A shared abstraction
between two protocols we understand unevenly is a refactor to do afterwards, if
ever.

## Blast radius: every surface keyed on a tool name

**This section exists because the first draft did not have it. The first review
found seven live surfaces it missed; the second review, against this table, found
one more that matters and three that are benign.** The file-tool port's review
found exactly the same category and recorded the lesson; not applying it here was
a failure to read our own history, twice. Each row below breaks **silently** — no
compile error, because every one of them is a string match with a default arm.

| surface | what it keys on | what breaks |
|---|---|---|
| `jcode-provider-claude-cli-runtime/src/lib.rs:1103,1139` | `"lsp"` ↔ `"Lsp"`, both directions | already maps a name for a deleted tool. A second name (`lsp_edit`) has **no** mapping and passes through raw |
| `jcode-usage-types/src/lib.rs:82-112` | tool name → telemetry category | `lsp` falls to `Other`. `ast_grep` is listed at `:91`; the read-only `lsp` belongs beside it, and a write `lsp_edit` under `Write` at `:95` |
| `jcode-telemetry-core/src/lib.rs:1069,1125,1132,2291` | four separate write-tool name lists | LSP renames are invisible to file-write telemetry in all four |
| `jcode-tui/src/tui/app/observe.rs:89-106` | `mutates_repo` list | a rename leaves the TUI's git-info cache stale. `ast_edit` is in the list at `:103` for exactly this reason |
| `jcode-tui-tool-display/src/lib.rs:40` `is_edit_tool_name` | `"write"｜"edit"｜"multiedit"｜"patch"｜"apply_patch"` | **the consequential one, found on the second pass.** Five production call sites gate edit-diff rendering and pinning on it: `ui_messages.rs:4062`, `ui_pinned.rs:788`, `ui_prepare.rs:1576`, `state_ui.rs:61`, `input.rs:2454`. An `lsp_edit` rename renders and pins as a plain tool row, not as an edit, **everywhere in the TUI**. Its own doc comment at `:38` says a renamed edit tool "must still display its diffs rather than degrade to a bare tool name" — the warning was already written |
| `jcode-tui/src/tui/remote_diff.rs:51-61` | `"edit"｜"write"｜"multiedit"` plus the `file_path` **argument** | remote diffs miss every LSP rename. Note the existing comment at `:63`: hashline already forced this code to stop trusting `file_path` alone |
| `jcode-app-core/src/agent/tools.rs:104-148` | per-name arg display in run mode | `lsp` falls to the silent default arm, so `jcode run` prints the tool name and nothing about the call |
| `jcode-tui/src/tui/app/state_ui_storage.rs:14` | canonical tool name → which args survive compaction | undecided for `lsp`; falls to the default arm, so a compacted transcript may lose what the call was |
| `jcode-tui/src/tui/ui_tools.rs:1484` | the stub's arg names | renders `lsp :0` |
| `jcode-app-core/src/agent/inline_tail.rs:128-147` | per-tool "interesting field" map | the live status line shows nothing for `lsp`. `file` would be the field |
| `jcode-app-core/src/catchup.rs` | per-name `file_path` extraction for resume summaries | a rename does not appear in "what happened while you were away" |
| `jcode-productivity-core/src/aggregate.rs:109` | `tool("grep") + tool("agentgrep") + tool("glob")` | navigation activity uncounted. Judgement call whether LSP counts as search |
| `jcode-desktop2/src/edits.rs` | string-scans raw JSON for `"file_path"` | an edit card with no files. Only if a write action lands there |
| user `pre_tool` hooks | tool args | a hook written against `{file_path, old_string}` sees neither |

**Checked and benign, recorded so a fourth reviewer does not have to find them
again.** Each keys on a tool name, so each belongs under this heading, but each
passes an unknown name through unchanged rather than mishandling it:

| surface | why it is fine |
|---|---|
| `jcode-provider-core/src/anthropic.rs:367-397` | OAuth name mapping both directions; `lsp`/`lsp_edit` pass through. Note the "keep in sync" cross-reference at `jcode-tool-types/src/lib.rs:106` — if a mapping is ever added, it must be added in both |
| `jcode-tool-types/src/lib.rs:71` `resolve_tool_name` | alias resolution; no `lsp` alias exists, and per `~/NLFCODE.md` a *stale* alias here silently broke `grep` for a release. **Do not add one.** The invariant tests (`registered_tools_are_never_aliased_to_something_else`) already guard it |
| `jcode-productivity-core/src/scan.rs:350` `canonical_tool_name` | counts by raw name, so a new name counts as itself |
| `jcode-tui-tool-display/src/lib.rs:21` `canonical_tool_name` | identity for unknown names |

**The unifying fact: LSP write actions have no `file_path` argument.** The paths
live inside a `WorkspaceEdit` the *server* returned, which is not visible in the
tool input at all. So every `file_path`-keyed consumer is structurally blind to
an LSP rename, in a way that is different from and worse than hashline — hashline
at least had the paths in its own payload.

**Two consequences for the design, both decided here rather than discovered:**

1. **The write action must report its touched paths in `ToolOutput::metadata`**,
   and the consumers that care must read them from there. This is the same fix
   the file-tool port arrived at for the renderer
   (`OMP_TOOL_PORT.md`, "The cleaner fix"): put it in the result rather than
   making six consumers reconstruct it from arguments.
2. **A test must pin it.** `an_lsp_write_reports_every_path_it_touched`, in the
   spirit of the prior port's
   `every_file_tool_call_still_yields_a_file_path_for_downstream_consumers`.

**Minimum set for v1.** Most `file_path`-keyed rows are v2 concerns, but **v1
ships `lsp_edit` carrying `request`** (see Scope), so it is not purely read-only:
the cli-runtime mapping, the telemetry category (both names), `ui_tools`,
`inline_tail`, `agent/tools.rs`, and `state_ui_storage`. `is_edit_tool_name`,
`observe.rs`, `remote_diff`, `catchup`, and `desktop2` wait for v2, because
`request` does not apply a `WorkspaceEdit` — **unless** the model uses it to send
`textDocument/rename` by hand, which it can. That is an argument for treating
`is_edit_tool_name` as v1 too, and the cheap resolution is to add both names
there once rather than reason about which methods a raw `request` might carry.

## Phase 0 — decide the surface, then reconcile the stub (½ day)

**Not independent, and the first draft was wrong to say it was.** Phase 0 item 1
retargets a renderer arm to "the schema this plan will ship", which cannot be
done before question 2 (one tool or two) is answered — that decision determines
the names. So Phase 0 *starts* with the two open questions.

0. **Answer questions 1 and 2** (registry lifetime, one tool or two). Both are
   below, with recommendations. Nothing else in this plan can be written against
   an undecided tool surface: Phase 1's Group A tests pin registry semantics, and
   every test's entry point is the tool name.
1. `ui_tools.rs:1484` reads `operation`/`file_path`. Retarget to the schema
   chosen in step 0, and comment that the arm predates the tool.
   **Whether old transcripts contain `lsp` calls is unverified** — the first
   draft asserted they do. Check before deciding retain-vs-delete: the tool was a
   stub that returned an apology, so a model may have called it once and never
   again. `grep` the session store rather than guessing.
2. `jcode-base/src/safety.rs`: `classify` fails closed, so an unlisted `lsp`
   requires permission. Right for write actions, **wrong for read-only ones**:
   every `hover` would interrupt the user. Resolved by step 0's answer to
   question 2; the list edit is mechanical once the names exist.
3. Add the read-only name to the surfaces in "Blast radius" marked as the v1
   minimum set.
4. Record the test baseline (`cargo test --workspace` pass/fail counts) so
   "suite unchanged" is checkable. The fork has known pre-existing `jcode-tui`
   failures; **record the actual number rather than trusting the 8 quoted from
   memory in the first draft.**

**Exit:** questions 1 and 2 are answered in writing in this document; the
renderer arm and the v1-minimum name-keyed surfaces match those answers; a
baseline count is recorded here.

## Phase 1 — port the tests (3-4 days)

**This phase produces failing tests and no implementation.** Same as the file
tools, and for the same reason: it tells us in days rather than weeks whether
these behaviours fit jcode.

### The fake server comes first

`test/fixtures/fake-lsp-server.ts` (176 lines) is the foundation of the
regression suite. It is a real process speaking real framed JSON-RPC over stdio,
with a `test/state` introspection method returning `didOpen` counts,
`didChange` versions, `didClose` list, and the notification order.

**Ours must be a Rust binary, not a mock object.** `#[cfg(test)]` fakery inside
the client cannot catch a framing bug, a chunk-boundary split, or a wedged pipe
— and those are the failures that will actually happen. Build it as a test-only
bin in `jcode-lsp` (`src/bin/fake_lsp_server.rs`, or a `tests/` helper binary
resolved via `env!("CARGO_BIN_EXE_...")`).

It needs, mirroring theirs: `initialize` returning configurable capabilities,
`test/state`, `test/echo`, `test/serverRequest` (to drive the server→client
direction), `publishDiagnostics` on open/change, `shutdown`/`exit`, and
`-32601 Method not found` for everything else. Plus, beyond theirs, knobs for
the failure modes: hang before responding, exit mid-request, emit a body across
two writes.

**Do this first and do it well.** If the fake server is awkward, every
subsequent test is awkward.

### What to port, in priority order

Of 5,025 lines and 130 cases across the 7 files, **~60 are in scope for v1/v2**.
Grouped by what they pin rather than by file, because `lsp-regressions.test.ts`
mixes concerns freely:

**Group A — framing and lifecycle (~15 cases). Port all. Start here.**
`initialize`/`initialized` order, the `exit` notification after `shutdown`, an
already-starting client not being duplicated, caller abort not cancelling a
shared initialization, `shutdownClientInstance` removing by *identity* (not
name), a reader dying while the process lives, stdout closing before exit
publication, and an async stdin write rejection surfacing rather than resolving.

The last three are the ones nobody writes unprompted, and each is a hang or a
silent success in production.

**Group B — server→client requests (~7 cases). Port all.**
`workspace/configuration` answered in request order with `null` for unknown
sections; configuration pulled *after* `didChangeConfiguration` not killing the
session; dynamic capability registration accepted before semantic requests;
`workspace/workspaceFolders`; spec no-op results for defined server→client
methods; and **the id-collision case**, which is the sharpest test in the file.

**Group C — diagnostics freshness (~10 cases). Port all.**
This is the correctness core of `diagnostics` and the part most likely to be
wrong in a naive implementation. It pins: stale diagnostics suppressed until the
matching document version arrives; settling on the latest unversioned publish
when a server never echoes versions; not reusing one file's diagnostics after a
*different* URI publishes; and URI-renormalization matching (a server may send
back a differently spelled URI for the same file).

**A naive implementation reads the cache and returns whatever is there**, which
gives the model diagnostics for the pre-edit content and is worse than no
diagnostics because it looks authoritative.

**Group D — position resolution (~6 cases). Port all.**
`symbol` resolution: the Nth occurrence on a line, a symbol absent from the
target line throwing, an out-of-bounds `#N` throwing, and `$`-prefixed
identifiers resolving past compound matches. Word-boundary matching for bare
identifiers is in `findSymbolMatchIndexes` and is why `id` does not match inside
`uuid`.

**Group E — `WorkspaceEdit` application (~12 cases). v2.**
Overlap rejection, byte-identical dedup, zero-width inserts *not* deduped
(inserts are not idempotent), equal-position inserts applied in array order per
LSP §3.16.2, every file validated before any file is written, and pending text
edits flushed before a rename/delete of the same subtree.

**`sortAndValidateTextEdits` is 37 lines and every one of them is a bug someone
had.** Port it near-literally, and its tests exactly.

**Group F — config and detection (~8 cases). Port selectively.**
Root markers, binary resolution, `.omp/lsp.json` precedence, and workspace
`reload` invalidating the config cache. **Drop** the Windows-`.exe`,
virtualenv-`Scripts`, and Claude-plugin-marketplace cases: they test paths we do
not have. Keep the *shape* of local-bin resolution (`node_modules/.bin`) since
that is how a TypeScript server is usually found.

**Group G — rendering and sanitization (~5 cases). Port the sanitization.**
Tab and control-character sanitization in rendered diagnostic output is not
cosmetic: a diagnostic message containing a tab or an escape sequence goes
through our TUI. **This is a real hazard** — a hostile or merely eccentric
server message reaching a terminal renderer. Their `lsp-render.test.ts` and
three sanitization cases in the regressions file cover it.

**Group H — dedup ledger (9 cases). Port. It is 51 lines.**
`DiagnosticsLedger` suppresses a diagnostic already reported for a file, keyed
on the message with its `path:line:col` prefix stripped — so a diagnostic that
merely *moved* is not re-reported. Cheap, and directly aimed at context waste.

**Do not port:** the `mux` suite (387 lines, 9 cases),
`lsp-format-options.test.ts` (153, 15 cases), the `writethrough`/batching suite
(182, 6 cases), workspace-diagnostics project detection (`go.work` etc.), and
`test/task/subagent-lsp.test.ts`.

**One file needs an explicit decision rather than silence:**
`test/interactive-mode-lsp-startup.test.ts` (134 lines) covers omp's
startup discovery and background warmup — the mechanism that surfaces available
servers in the welcome screen before any tool call. We have no equivalent, and
**cold-start latency is the risk most likely to make the model avoid this tool**
(see the risk table). Decide in Phase 1: port it as the spec for a future warmup,
or drop it and record that first-use pays the cold start. Recommend **drop with
the note**, since warmup needs a UI surface this plan does not touch.

### How to port a test

Same rule as last time, restated because it is the rule most easily broken:
**read the TypeScript, extract the assertion about behaviour, write a Rust test
asserting the same thing against our interface. Do not transliterate.**

Where a behaviour depends on a surface we lack, **drop the test and record why**
in `crates/jcode-lsp/PORTING_NOTES.md`. A dropped test is a decision, not an
omission.

**Exit, in two parts. The first part is the one that actually closes the
loophole.**

**(a) Enumerate before counting.** The first commit of Phase 1 writes, into
`crates/jcode-lsp/PORTING_NOTES.md`, the **case titles** from omp's test files
assigned to each group, with each marked *port*, *drop (reason)*, or *v2*. Only
then is the count real. The numbers in the table below are estimates taken from
reading the files; they are the target, not the evidence.

This is the second review's sharpest point and it is correct: the first draft set
the target at 40, below the sum of its own groups; revision 2 hardened tildes
into an exact 56, which errs the other way. **The enumeration, not the number, is
what closes it.** A count asserted from a skim is not more trustworthy for being
precise.

Two known discrepancies the enumeration must resolve, both already visible:

- **Group C says "port all (~10)" but `lsp-diagnostics-freshness.test.ts` has
  15 cases**, and at least three of them test surfaces this plan excludes (the
  deferred-diagnostics channel, batched sibling writes, custom formatting). So
  "port all" cannot mean the file, and C is an unenumerated subset. Enumerate it
  and drop the writethrough-dependent cases by name.
- **Group G is "~5 cases" in the body and 4 in the table.** Resolve by counting.

**(b) The count, once enumerated. This is now done** —
`crates/jcode-lsp/PORTING_NOTES.md` lists all 130 case titles with a disposition
each, reconciled by script so every case is cited exactly once. Every case marked
*port* is ported, each failing with an **assertion** — not a compile error and
not `todo!()`:

| group | enumerated | tests | v1 or v2 |
|---|---|---|---|
| A framing and lifecycle | 14 of 17 | the client | v1 |
| B server→client requests | 7 of 7 | the client | v1 |
| C diagnostics freshness | 6 of 16 | the client | v1 |
| D position resolution | 5 of 5 | the tool | v1 |
| G sanitization | 3 of 4 | the tool | v1 |
| H dedup ledger | 9 of 9 | the tool | v1 |
| F config and detection | 5 of 12 | the tool | v1 |
| write actions: `request` only | 2 of 6 | the tool | v1 |
| **v1 total** | **51** | | |
| E `WorkspaceEdit` application | 11 of 11 | the tool | v2 |
| write actions: `rename_file` | 4 of 6 | the tool | v2 |
| **v2 total** | **66** | | |

**The enumeration moved the target from 56 to 51, and the reason is the one the
second review predicted.** Group C was estimated at "port all (~10)" against a
15-case file that is mostly writethrough machinery: 6 are portable. Group A's 15
included three cases about surfaces we do not have. Group F is 5 of 12, the drops
being Windows path resolution and omp's plugin marketplace.

The `tests` column exists because Phase 2's exit depends on it: **A, B, and C
are the groups that exercise the client rather than the tool surface**, so they
are the ones that can be green before any tool exists. That is **27** cases
(14 + 7 + 6).

A number may shrink further against a recorded reason in `PORTING_NOTES.md`; it
may never shrink for convenience.

## Phase 2 — implement the client (1.5 weeks)

Build against Phase 1. Order chosen so each step is testable by the tests
already written.

1. **Framing.** `Content-Length` reader/writer over `tokio` pipes. A body split
   across reads must work; a header split across reads must work. This is the
   single highest-risk 100 lines in the project because everything downstream
   silently depends on it.
2. **Correlation.** Client→server requests with ids, a pending map, timeouts,
   and `$/cancelRequest` on abort. **Separate** the server→client request path
   from the client's own pending map: the id spaces are independent, and group
   B's collision test fails otherwise.
3. **Lifecycle.** `initialize` → store capabilities → `initialized` →
   `didChangeConfiguration`. Then `shutdown` → `exit` → wait for exit with a
   timeout, then kill.
4. **Notifications.** `publishDiagnostics` into a per-URI cache with versions;
   `$/progress` tokens tracked so "project loaded" is answerable;
   `window/logMessage` dropped.
5. **Server→client handlers.** `workspace/configuration` (ordered, `null` for
   unknown), `workspace/workspaceFolders`, `client/registerCapability`, and
   spec no-op results for the rest. `workspace/applyEdit` is **refused in v1**
   and honoured in v2 when edit application exists.
6. **Document sync.** `didOpen` with an inferred `languageId`, version tracking,
   `didClose`. A `didOpen` for a file already open must not be sent twice.

**Exit for Phase 2** (the first draft stated none): every case from groups **A,
B, and C** is green — those are the groups that test the client rather than the
tool, **27 by the enumeration** (14 + 7 + 6). Revision 2 said "41" here, which
matched no subset of its own table (it was A+B+C+H, and H tests the tool); the
number is now derived from the group list and then from the enumeration rather
than asserted beside it.

Plus a hand-run smoke check against **both** real servers completing
`initialize` → `didOpen` → one `definition` → `shutdown` → `exit` with the child
process confirmed gone. The process check matters: a clean shutdown that leaves
the child alive is the leak the risk table names, and it looks identical to
success from inside the client.

**The clangd half of that needs a fixture this plan must budget for.** This
repository is Rust, and `clangd`'s root markers are `compile_commands.json` and
friends, so there is nothing here for it to attach to. Phase 2 therefore creates
a minimal C fixture — one `.c` file and a hand-written `compile_commands.json`
under the crate's `tests/` — as part of the smoke check. Two lines of JSON, but
without it the exit criterion is unsatisfiable, which is how a criterion quietly
becomes optional.

### Client registry: the ownership question, and it is not omp's

omp caches clients in a module-global `Map<"command:cwd", LspClient>`, shared
across sessions in one process. **We cannot copy that, and the reason is
specific to us.**

jcode runs a **long-lived daemon** serving many sessions
(`~/.jcode/builds/shared-server/jcode`, see `AGENTS.md`). A module-global map
means a language server spawned by one session is reused by another, and never
dies until the process does. `rust-analyzer` on a large workspace is
multi-gigabyte. Three projects and a daemon that runs for days is a memory leak
with a friendly face.

**This must be decided in Phase 0, not here.** Phase 1's Group A ports
"an already-starting client is not duplicated" and "`shutdownClientInstance`
removes by identity", both of which are assertions *about the registry*. They
cannot be written against an undecided ownership model. The first draft deferred
this to "Phase 2's first decision", which was wrong by one phase.

Three options (question 1 below):

- **(a) Per-project, process-global, idle-timeout.** Closest to omp. One
  `rust-analyzer` per workspace shared by every session in it, shut down after N
  minutes idle. Best behaviour (warm servers), needs the idle sweeper to
  actually work or it is the leak above. omp has `setIdleTimeout` and an
  `IDLE_CHECK_INTERVAL_MS` sweeper for exactly this, defaulting **off**.
- **(b) Per-session, dropped at session end.** Safest, and it matches how the
  hashline store is keyed. But a fresh `rust-analyzer` per session means a
  30-second cold start on the first `definition` of every session, which is
  slow enough that the model may learn to avoid the tool.
- **(c) Per-project with a hard cap** (say 3 servers), LRU-evicted.

**Recommendation: (a) with the idle timeout ON by default** (~5 minutes) and a
cap on concurrent servers. Warm servers are most of the value, and an
always-armed sweeper is the difference between (a) and a leak. This inverts
omp's default deliberately, because their host process is short-lived and ours
is not.

**Whatever is chosen must be pinned by a test that shuts a server down**, since
the failure mode is invisible: nothing breaks, memory just grows. Verified in
omp's source: the cache key is `` `${config.command}:${cwd}` `` (`client.ts:699`),
`idleTimeoutMs` defaults to `null` with the comment "Idle timeout configuration
(disabled by default)", and `IDLE_CHECK_INTERVAL_MS` is 60,000.

### The approval problem

`SafetySystem::classify` (`jcode-base/src/safety.rs:180`) takes a **tool name**
and nothing else. `AUTO_ALLOWED` (`safety.rs:132-147`) is a list of **twelve**
read-only names. Anything else requires permission — correct, and it fails
closed.

But `lsp` is one tool with fourteen actions spanning both tiers. `hover` is
strictly more read-only than `read`; `rename` rewrites files across a repo. The
existing mechanism cannot express that, and the two obvious moves are both
wrong:

- **`lsp` in `AUTO_ALLOWED`** auto-approves `rename`. Unacceptable.
- **`lsp` not in `AUTO_ALLOWED`** prompts on every `hover`. The model will stop
  using it, and we will have shipped the stub's outcome with more code.

Three real options:

1. **Two tools: `lsp` (read-only) and `lsp_edit` (write).** Names carry the
   tier, `classify` needs one list entry, no new mechanism. Cost: two schemas,
   two descriptions, two always-on token budgets, and a split the model has to
   learn. **This is also precisely the shape `ast_grep`/`ast_edit` already
   took**, and that precedent is tested
   (`only_the_read_only_tool_is_auto_allowed`).
2. **Argument-aware classification.** `classify(name, args)`. Correct in
   principle and it is a change to a security surface used by every tool. Bigger
   than this plan.
3. **One tool, self-gated.** The write actions request approval themselves.
   Bypasses the mechanism that exists to be the single choke point. No.

**Recommendation: option 1**, following the `ast_grep`/`ast_edit` precedent
exactly. It is the only one that needs no new mechanism, and the fork has
already validated the shape.

This is **question 2** for the user. It changes the tool surface and therefore
the schema, the prompt, every test's entry point, and every row of the blast
radius table. **It is Phase 0 step 0**, not a decision to reach later.

## Phase 3 — v1 tool surface (4-5 days)

`crates/jcode-app-core/src/tool/lsp.rs`. Read-only actions only.

### Schema

omp's parameter names, since models with omp or Claude-Code priors will guess
them and a first-call failure is what sends a model to bash for the session
(`~/NLFCODE.md` item 4, learned the hard way):

`action`, `file`, `line` (1-indexed), `symbol` (with `name#N`), `query`,
`timeout`. `new_name` and `apply` arrive in v2 with the write tool.

Constraints from our side, all verified:

- **OpenAI strict mode.** `tool/tests.rs:1821` pins the exempt set to exactly
  `["batch", "browser", "initiative", "swarm"]` and fails on additions. So: no
  open-ended maps, no unconstrained `additionalProperties`. A flat schema of
  scalars is fine. Note `request`'s `payload` is a **JSON string**, not an
  object — which is what makes it strict-compatible, and is presumably why omp
  typed it that way too.
- **Description token cap: 20** (`tool/tests.rs:489`). omp's `lsp.md` prompt is
  19 lines. The always-on description gets one line; the operations table goes
  in the `action` enum's parameter description, capped at 25 each.
- **Curated OAuth schemas.** `lsp` is not a Claude-Code builtin, so it is
  appended from the registry and no curation applies
  (`jcode-provider-anthropic/src/lib.rs:521`). **Nothing to do** — but worth
  stating, because the file-tool port found this constraint late and it changed
  the design.
- **`intent` and `accept_large_output`** are injected centrally
  (`ensure_intent_in_schema`). Do not declare them.

### Output

`ToolOutput` is a flat `output: String` plus `title`, `metadata`, `images`.
omp's tests assert on `details.{action,success,serverName}`. **Decide once, in
Phase 1, where each asserted detail lands** — `metadata` or the rendered string
— because deciding per-test is how a port drifts. Recommend: human-readable text
in `output` (the model reads it), `serverName`/`action`/`success` in `metadata`
(the renderer reads it).

### Hashline: `lsp` is a producer, and this is not optional

`OMP_TOOL_PORT.md` established the standing rule:

> **Every tool that displays file content must mint a tag and record seen
> lines. Every tool that writes files must re-record afterwards and return the
> fresh tag.**

`lsp` displays file content in v1: `definition` and `references` print
surrounding context lines (`readLocationContext`, ±1 line), and `symbols`
prints `name @ file:line:col`. A model that sees a line through `lsp` and then
edits it by number **must have a valid anchor**, or we have reintroduced the
silent-wrong-site edit that hashline exists to prevent.

So v1 `lsp` must, for every file whose content it shows:
`hashline_store::for_session(&ctx.session_id).record(&key, &content, Some(&seen_lines))`
and emit the `[path#TAG]` header. The precedent to copy is `ast_grep`
(`ast_tools.rs:175-183` for the recording, `:240-290` for the rendering) — and
copy its **fix** too: `50418a767 fix(ast_grep): show whole source lines, so the
tag can actually be edited from` recorded that showing a *fragment* of a line
mints a tag the model cannot safely use. Show whole lines.

**Note `ast_grep` passes `None` for seen lines** (`ast_tools.rs:181`), so it
mints a tag without provenance, which under the store's contract *skips* the
seen-line guard for that path. `lsp` should pass `Some(&seen_lines)` and be
stricter, because it shows only a handful of context lines out of a whole file —
granting an unguarded anchor over a file the model saw three lines of is exactly
the case the guard is for. **This is a deliberate divergence from the local
precedent**, so it needs a test saying so.

**`symbols` is the ambiguous case and needs a decision.** It prints a symbol
name and a location but not the line's text. Recommendation: **do not** record
those lines as seen — the model has not seen the line, only that a symbol lives
there. Recording them would grant an anchor for content never displayed, which
is the exact failure the guard exists to catch. Pin it as a test.

In v2, the write actions become invalidators: after applying a `WorkspaceEdit`,
**every** touched path must be re-recorded, and the fresh tags returned.
`ast_edit` does this (`ast_tools.rs:383`) and the reasoning is in its comment.

### Live-run acceptance, not just tests

Given the file-tool port's finding that eight defects came from live runs and
none from tests, Phase 3 does not exit on green tests. It exits on a **recorded
live agent session** against this repository doing: find every caller of a
function, jump to a definition through a re-export, and read diagnostics on a
file with a deliberate type error. Failures get fixed and recorded, in the
tradition of that plan's "what only the live agent runs caught" section.

## Phase 4 — v2: the write actions (1.5 weeks)

`rename`, `rename_file`, `code_actions`, plus `reload`, `capabilities`,
`request`.

1. **`WorkspaceEdit` application** (`edits.rs`): flatten `changes` and
   `documentChanges`, sort bottom-to-top, reject overlaps, dedup byte-identical
   non-empty edits, keep zero-width inserts, and **validate every file before
   writing any**. Group E's 12 tests.
2. **`rename`.** Requires `symbol` when `line` is given — omp *errors* rather
   than falling back to the first non-whitespace column, because a rename at a
   guessed position is a silent wrong rename. Copy that.
3. **`rename_file`.** Sends `willRenameFiles`, applies returned edits, renames
   on disk, sends `didRenameFiles`. Caps at 1,000 pairs for a directory.
4. **`code_actions`.** Lists by default; applies one with `apply: true` plus a
   `query` selector.
5. **`reload`, `capabilities`, `request`.** Small.

**Move `request` into v1.** The first draft scheduled it here while arguing it
is "worth having early precisely because it makes the tool's gaps survivable" —
an argument against its own placement, caught in review. It is a thin wrapper
over `sendRequest` once the client exists, it needs no `WorkspaceEdit`
machinery, and it is what makes an incomplete v1 usable: any operation we did
not port is still reachable. It is a **write-tier** action though (it can send
arbitrary methods), so under the two-tool split it belongs on `lsp_edit`, which
means v1 ships a minimal `lsp_edit` carrying only `request`.

**Exit for Phase 4:** Group E's 12 tests green; a live agent run performing a
cross-file rename in this repository, with the resulting diff inspected by hand
and `cargo check` clean afterwards; and `an_lsp_write_reports_every_path_it_touched`
passing, so the blast-radius consumers have something to read.

**Every write action goes through the approval gate**, which is what the
two-tool split decided in Phase 0 buys. And note omp's own documented bug, which we
should fix rather than inherit: `apply: true` with `query` omitted silently
lists instead of applying. Their own doc calls it out. Return an error naming
the missing selector.

## Verification, and its honest ceiling

**Two language servers are installed on this machine** — `rust-analyzer` and
`/usr/bin/clangd` (Apple clangd 21.0.0, ships with Xcode). That still bounds live
verification hard, and pretending otherwise is how a port ships broken for every
other language.

| what | how |
|---|---|
| protocol correctness | the fake server. Deterministic, covers failure modes no real server reproduces on demand |
| a slow project-loading server | `rust-analyzer` on this repo. Real cold start, real multi-second project load, real diagnostics |
| a fast, differently-shaped server | `clangd`. No project-load phase, `compile_commands.json` rather than `Cargo.toml` as root marker, so it exercises the config path differently |
| every other server | **not verified.** Config entries for them are untested data |
| model-facing behaviour | live agent runs, per Phase 3 |

**Using both real servers matters more than the count suggests.** A client tested
only against `rust-analyzer` will bake in its assumptions — the analyzer-status
polling, the long warmup, the Cargo root — and `clangd` is the cheapest available
check that those are handled as *server-specific* rather than as universal truths.

Two consequences to accept out loud:

- **Ship a small `defaults.json`, not all 53.** Include what can be reasoned
  about, with the two that can be tested first: `rust-analyzer`, `clangd`,
  `typescript-language-server`, `pyright`, `gopls`. An untested config entry is a
  claim we cannot support, and a wrong `args` array is a server that silently
  never starts. Config is user-extensible, so a missing entry is a config line,
  not a wall.
- **rust-analyzer needs special handling and it is not optional.** omp's
  `waitForRustAnalyzerWorkspace` (`client.ts:658-691`) polls
  `rust-analyzer/analyzerStatus` against four constants (`client.ts:634-637`):
  5,000ms timeout, 100ms poll, 2,000ms settle, 1,000ms status-request timeout.
  Separately, `rust-analyzer/reloadWorkspace` is used by the **reload action**
  (`servers.ts:264`), not by the readiness wait — the first draft conflated the
  two. Port both, but do not mistake one for the other. **This is a server we
  will actually be testing against**, so getting it wrong makes every local check
  unreliable.

## Risks, named

| risk | mitigation |
|---|---|
| **We reship the stub** — a tool that advertises what it cannot do | No phase registers an action it cannot answer. `file: "*"` refuses by name rather than returning empty |
| **Servers leak in the daemon** | The registry question is answered in Phase 0, and a shutdown test pins it. This is the failure mode with no symptom |
| **Framing bug** | The fake server exists specifically to split bodies and headers across reads. Do not accept a mock here |
| **Wedged server hangs a turn** | Every request has a timeout, abort sends `$/cancelRequest`, a wedged write tears the client down. Group A's tests |
| **Cold start is slow enough that the model avoids the tool** | Argues for warm shared servers (registry option (a)) and for honest `timeout` handling that says *why* it timed out |
| **Only two servers verifiable** | Small `defaults.json`; untested entries stated as untested; use both real servers so analyzer-specific behaviour is not mistaken for universal |
| **Approval fatigue kills adoption** | The two-tool split, so read-only is auto-allowed |
| **Hashline contract broken silently** | `lsp` is a producer from day one, not retrofitted. `50418a767`'s lesson: show whole lines |
| **A server message reaches the TUI unsanitized** | Group G ported before v1 ships |
| **Scope creep into `writethrough`** | Explicitly out, with its own future plan |
| **Rebase surface** | `jcode-lsp` is a new crate and cannot conflict. `tool/mod.rs` (42 commits/6mo) and `safety.rs` (3) take pure insertions. `ui_tools.rs` (14) is the hot file; Phase 0 touches it once |
| **Blast-radius surfaces missed again** | The table above, plus `an_lsp_write_reports_every_path_it_touched`. The first draft missed seven of them; this is the second time the same category has been missed on this method |

## Sequencing

```
Phase 0  ½d    decide Q1+Q2, reconcile the stub, baseline    (the prerequisite)
Phase 1  3-4d  fake server + 56 ported tests → failing suite (the gate)
Phase 2  1.5w  the client: framing, correlation, lifecycle
Phase 3  4-5d  v1 tool: read-only + request + hashline + live run
Phase 4  1.5w  v2: WorkspaceEdit, rename, code_actions
```

**~5 weeks to v2**, revised up from the first draft's 3.5. The reason is
measured, not defensive: the in-scope base was understated by 50% (6,433 lines,
not ~4,300), and the two phases sized by code volume were scaled by that ratio.
The prior port by this same method underestimated its own extraction by ~2x
(`OMP_TOOL_PORT.md`, "Extraction is ~7,900 lines, not 3,790"), so the correction
is a known failure mode of this author, not bad luck.

Against the survey's framing — "large build: server lifecycle, per-language
config, a tool surface over ~14 ops. Worth scoping as an epic before committing"
— five weeks is what an epic looks like, and v1 alone is ~3 weeks.

Phase 1 is the gate. If these behaviours turn out to be incompatible with
jcode's session model, we find out in week one with no implementation written.
## Questions for the user

1. **Client registry lifetime** — per-project shared with an idle timeout (a),
   per-session (b), or capped LRU (c)? Recommend (a) with the timeout **on** by
   default, inverting omp's default because our daemon is long-lived.
2. **One tool or two** — `lsp` + `lsp_edit`, following `ast_grep`/`ast_edit`?
   Recommend two. It is the only option needing no new mechanism, and the
   alternative auto-approves cross-repo renames or prompts on every `hover`.
3. **Scope of `defaults.json`** — the five servers we can reason about, or all
   53 as untested data? Recommend five.
4. **Is v2 in scope now, or does v1 ship and get measured first?** v1 is
   independently useful (navigation and diagnostics are most of the value) and
   `rename` is the headline capability. Recommend shipping v1, measuring
   adoption, then v2 — mirroring how the file-tool port gated its forgiveness
   layer on measurement.

Questions 1 and 2 are **Phase 0 step 0** and block everything. Questions 3 and 4
can be answered later without invalidating written work.

---

# Adversarial review findings, folded in 2026-08-09

A reviewer agent checked the first draft against `/tmp/omp` and the jcode tree.
**It found errors in nine measurements, seven missed blast-radius surfaces, two
sequencing inversions, and one exit criterion that was smaller than the sum of
its own parts.** Corrections are applied in place above; this section records
what was wrong so the mistakes stay visible, per the house style.

## What was wrong, and what it cost

**1. The size table did not add up, and the conclusion drawn from it was false.**
The draft claimed 11,047 total lines, ~4,300 in scope, ~6,700 out. Actual:
9,354 lines of TypeScript across 24 files, **6,433 in scope**, 2,921 out — and
those two now sum exactly to the total. The draft's own parenthetical summed to
5,692, contradicting its own "~4,300" in the same cell.

Two files were missing from both tables: **`render.ts` (668 lines)**, which is
load-bearing because the crate description includes "formatting of results" and
Group G tests its sanitization, and `diagnostics-ledger.ts` (51), which Group H
explicitly ports.

The cost: the draft asserted "the tests are larger than the code" and called it
"the strongest available evidence" for its difficulty model. With correct
numbers the tests are *smaller* (5,025 against 6,433). The argument is now
restated as a ratio (0.78:1) rather than an inequality, and **the estimate went
from 3.5 to 5 weeks** as a direct consequence.

**2. Seven blast-radius surfaces were missed, in the exact category the prior
port's review had already established.** `OMP_TOOL_PORT.md` has a section titled
"Surfaces keyed on tool name or `file_path` — a missing category". The draft
listed three touched files and moved on. There are twelve, and the worst is
`jcode-provider-claude-cli-runtime/src/lib.rs:1103,1139`, which carries a **live**
`"lsp"` ↔ `"Lsp"` mapping the draft did not know existed while asserting that
exactly one artifact of the stub survived.

This is the second time this method has missed this category. The new "Blast
radius" section exists because of it, and the standing lesson is worth stating
plainly: **on this codebase, a new tool name is not a local change.** Grep for
the name-keyed switch statements before writing the plan, not after.

**3. Two sequencing inversions, both making a phase unwritable.**
- Phase 0 was labelled "independent of everything else" while its first item
  required knowing the tool's schema — which question 2 decides.
- The registry-lifetime question was deferred to "Phase 2's first decision", but
  Phase 1 ports two tests that assert registry semantics. Both questions are now
  Phase 0 step 0.

**4. Phase 1's exit was 40 tests against "port all" groups summing to 56.** A
whole group could have been dropped without failing the criterion, and `N` was to
be "enumerated in `PORTING_NOTES.md`" — which let the implementer set the target
after the fact. The count is now fixed in this document, per group, and may only
shrink with a recorded reason.

**5. Phases 2 and 4 had no exit criteria at all.** Both now have them, and both
include a real-process check, because the leak this plan worries about is
invisible from inside the client.

**6. `request` was scheduled in v2 while the text argued it belonged early.** The
draft literally said it is "worth having early precisely because it makes the
tool's gaps survivable" and then put it in the later phase. Moved to v1.

**7. Smaller factual corrections:** 75 regression cases not 77; 130 total cases;
53 servers in `defaults.json` not 49; `AUTO_ALLOWED` has 12 entries not 11;
`McpHandle` is ~98 lines not 391 (the *file* is 511); `sortAndValidateTextEdits`
is 37 lines not 36; the `ast_grep` recording precedent is at `ast_tools.rs:175-183`,
not `:240-290`; the stub's renderer arm produces `lsp :0` rather than a blank
label; `rust-analyzer/reloadWorkspace` is used by the reload *action*
(`servers.ts:264`) and not by the readiness wait, which polls
`rust-analyzer/analyzerStatus` instead.

**8. Two claims were asserted without checking, and both are now marked as
unverified rather than quietly kept.** That the fork has exactly 8 pre-existing
`jcode-tui` failures (quoted from memory), and that old transcripts contain `lsp`
calls — which was the entire justification for retaining the renderer arm rather
than deleting it. Phase 0 now says to check the session store rather than guess.

**9. A second language server was available and the plan said there was one.**
`/usr/bin/clangd` ships with Xcode. This *helps* — it is a differently-shaped
server, so it is the cheapest available check that `rust-analyzer`'s quirks are
handled as server-specific rather than as universal. But the "honest ceiling"
section was built on a measurement taken carelessly, which is the opposite of
what that section is for.

## Where the reviewer was wrong

**`test/task/subagent-lsp.test.ts` does exist.** The review reported "no such
file exists anywhere in /tmp/omp". It is at
`packages/coding-agent/test/task/subagent-lsp.test.ts`, and it is listed in the
file inventory this plan's research pass produced. The "do not port" line naming
it was correct as written.

Recorded because the review's own standard applies to itself: a negative claim
from a search that did not find something is weaker than a positive claim from a
search that did, and this one was stated with the same confidence as the
measurements that held up.

## What was verified and held

Worth recording so the next reader knows which foundations were checked rather
than assumed:

- The deleted stub: 94 lines, 9 operation names, the apology string, deleted in
  `3d391a517` for being "a default-disabled stub". The same commit removed its
  `DEFAULT_DISABLED_TOOLS` entry.
- `SafetySystem::classify` at `safety.rs:180`, failing closed on unlisted names.
- The strict-mode exempt set pinned at `tool/tests.rs:1820-1824` as exactly
  `["batch","browser","initiative","swarm"]`, failing on additions.
- Description cap 20 (`tests.rs:488-493`), parameter cap 25 (`:740-741`).
- `lsp` is not a curated OAuth builtin, so it is appended from the registry and
  no curation constrains its schema. "Nothing to do" was right.
- `intent`/`accept_large_output` injected centrally by `ensure_intent_in_schema`
  (`jcode-tool-core/src/lib.rs:48`).
- The fake server contains `test/state`, `test/echo`, `test/serverRequest`,
  `publishDiagnostics`, `shutdown`, and `-32601`, all as described.
- The id-collision regression test exists, as do all of Group B's other cases.
- omp's idle timeout defaults **off**; cache key is `` `${command}:${cwd}` ``
  (`client.ts:699`); `IDLE_CHECK_INTERVAL_MS` is 60,000. The daemon-leak argument
  rests on real facts.
- `MAX_RENAME_PAIRS = 1000`; the `code_actions` apply-without-query fall-through
  is documented as a bug in omp's own docs; `rename` rejects a missing `symbol`.
- `percent-encoding` and `url` are both already in the workspace lockfile, so the
  "no new dependency" claim holds.
- `SnapshotStore::record(path, text, seen_lines)` at
  `jcode-hashline/src/snapshots.rs:113` matches the call shape proposed here.
- The four rust-analyzer readiness constants at `client.ts:634-637`.

---

# Second review, folded in 2026-08-09 (revision 3)

The same reviewer checked revision 2. **The measurement layer held — it re-took
every number and they were right.** It found three things to fix and one
judgement to concede, all applied:

**1. Phase 2's exit criterion said "41 tests from groups A, B, and C" when
A+B+C = 32.** 41 was A+B+C+H, and H tests the tool rather than the client. A
number asserted beside a group list instead of derived from it, which is the same
defect as the first draft's 40 wearing different clothes. The exit now names the
groups and derives the count, and the group table gained a `tests` column
(`client` or `tool`) so Phase 2's scope follows from the data.

**2. Moving `request` to v1 was applied in Phase 4 and Sequencing but not in
Scope**, which still listed it under v2. Worse than a stale line: because
`request` can send an arbitrary method it is write-tier, so **v1 now ships two
tools** — a read-only `lsp` plus a minimal `lsp_edit` carrying only `request`.
Scope now says so, and question 4 ("ship v1 first?") is explicitly re-framed,
because "v1 is the read-only half" is no longer true.

**3. The blast-radius table was still incomplete, on its third pass.**
`is_edit_tool_name` (`jcode-tui-tool-display/src/lib.rs:40`) gates edit-diff
rendering and pinning at five production call sites, so an `lsp_edit` rename
would render as a plain tool row throughout the TUI. **Its own doc comment
already warned about exactly this**: "a renamed edit tool must still display its
diffs rather than degrade to a bare tool name." Also added `agent/tools.rs` (run
mode's per-name arg display) and a second table of four **checked-and-benign**
name-keyed surfaces, so a fourth reviewer does not spend a pass re-finding them.

That is three consecutive passes at this one category, each finding something.
The lesson is now stronger than "grep before planning": **an exhaustive list of
name-keyed switches should be a checked-in test**, not a table in a document that
goes stale. Worth proposing separately — a test that fails when a registered tool
name is absent from every dispatch site that enumerates names would have caught
all eleven at once.

**4. A conceded judgement, and it is the sharpest point in either review.**
Revision 2 hardened the group counts from "~15", "~10" into an exact 56. The
reviewer's objection: that is *the same genus of error* as the first draft's 40,
now erring high instead of low, because **no case titles were ever enumerated**.
A precise number taken from a skim is not more trustworthy for being precise.

Two visible symptoms it named: Group C says "port all" against a 15-case file
while claiming 10, at least three of whose cases test excluded writethrough
machinery; and Group G is "~5" in the body against 4 in the table.

Phase 1's exit is now **two-part**: enumerate the case titles per group into
`PORTING_NOTES.md` as the first commit, *then* count. The table's numbers are
labelled a target rather than evidence. This is the right shape — the enumeration
is the falsifiable artifact, and the number is a consequence of it.

**5. Phase 2's clangd smoke test was unsatisfiable as written.** This repository
is Rust; `clangd`'s root marker is `compile_commands.json`. Nothing here for it
to attach to. Phase 2 now budgets a minimal C fixture, which is two lines of JSON
and one `.c` file — but without it the criterion would have quietly become
optional, which is how exit criteria die.

**6. The estimate is now accepted as credible.** The phases sum to ~4.7-5 weeks
against the 5-week headline. The reviewer's retained caveat is worth recording
because it will probably be right: **Phase 3 is the phase most likely to
stretch**, since the live-run defect budget (eight defects on the prior port)
sits inside its 4-5 days with no explicit buffer.

**7. One methodological note worth keeping.** The "130 test cases" figure is a
*static* count. At runtime the regressions file executes 76 from 75 declarations,
because one `it` sits inside a `for` loop over `[false, true]`. Static is the
right thing to have counted; just do not be surprised when a runner disagrees by
one.

## The reviewer conceded its own error

It had reported that `test/task/subagent-lsp.test.ts` "does not exist anywhere in
/tmp/omp". It does — 293 lines, 6 cases. Its search covered `test/` and
`test/tools/` and never `test/task/`, and it then made a claim about the whole
tree that its search could not support. It named this as the same failure mode
this document's revision note describes, which is the correct reading.

Cosmetic knock-on: "5,025 lines across 7 files" is accurate as scoped, but there
is an **8th** lsp test file of 293 lines deliberately excluded, and the do-not-port
line names it.

## Verdict

The reviewer's stated position: **sound enough to implement from** with the fixes
above applied. They are applied. One judgement disagreement is retained rather
than resolved, and it is recorded above as item 4 — the enumeration requirement
is the response to it, and whether that is sufficient will be visible in Phase
1's first commit rather than arguable now.
