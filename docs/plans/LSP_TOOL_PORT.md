# Plan: port omp's `lsp` tool to Rust, behind omp's tests

Status: **draft**, written 2026-08-09.

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

One artifact of the stub survives and must be reconciled:
`jcode-tui/src/tui/ui_tools.rs:1484` still has an `"lsp" =>` arm reading
`operation`, `file_path`, `line`. It is dead code today, and its parameter names
are the **stub's**, not omp's (`action`, `file`, `line`, `symbol`). Left alone it
would render every call as a blank label.

## Scope

**In, v1:** a real stdio JSON-RPC client with process lifecycle; config loading
with auto-detection; and the read-only actions that carry most of the value:
`diagnostics` (single file), `definition`, `references`, `hover`, `symbols`
(document + workspace), `type_definition`, `implementation`, `status`.

**In, v2:** the write actions — `rename`, `code_actions`, `rename_file` — plus
`reload`, `capabilities`, `request`. These are separated because they apply
`WorkspaceEdit`s, which is a second body of work (edit application, overlap
validation, resource ops) and a second approval surface.

**Out, and each for a stated reason:**

| omp piece | lines | why out |
|---|---|---|
| `mux/` (daemon, server, protocol) | 1,241 | a broker sharing one server across sessions. Real value at scale; we do not have the broker, and a private process per project works |
| `lspmux.ts` | 233 | detection of an external third-party multiplexer |
| `clients/biome-client.ts`, `swiftlint-client.ts` | 383 | CLI-shaped linters wearing an LSP-ish interface. Not LSP, and each is a bespoke JSON format |
| `writethrough.ts` | 561 | format-and-diagnose on every `write`/`edit`. **A separate feature** (see below) |
| `workspace-diagnostics.ts` | 170 | `file: "*"` shells out to `cargo check`/`tsc`/`go build`. That is `bash`, which we have |
| `deferred-diagnostics.ts` | 66 | needs a post-turn delivery channel we do not have |
| `format-options.ts` | 119 | formatting is not in scope for v1/v2 |

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

| thing | number | source |
|---|---|---|
| omp `src/lsp/` total | **11,047 lines** across 22 files | `wc -l` on the clone |
| in-scope subset | **~4,300 lines** (`client` 1,465, `tool` 1,352, `config` 549, `diagnostics` 516, `utils` 747, `types` 479, `edits` 288, `servers` 296) | same |
| out-of-scope subset | ~6,700 lines | the table above |
| omp lsp tests | **5,027 lines** across 7 files | `wc -l` |
| `lsp-regressions.test.ts` alone | 3,451 lines, **77 cases** | `grep -c` |
| their `defaults.json` | **49 servers** | count |
| language servers on this machine | **1** (`rust-analyzer`) | `which` |
| jcode crates with LSP code today | **0** | `grep -ril lsp crates/` |

Two of these deserve to be read together. **The in-scope implementation is
~4,300 lines of TypeScript and the tests are 5,027.** The tests are larger than
the code, which is the strongest available evidence that the difficulty here is
not writing an LSP client — it is the accumulated set of ways real language
servers misbehave. That is exactly the asset the port is for.

And **only one language server is installed here**, which sets a hard limit on
what live verification can prove. See "Verification".

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
`McpHandle` (391 lines) has: an `AtomicU64` id allocator, a
`Mutex<HashMap<id, oneshot::Sender>>` pending map, an `mpsc` writer task
serialising outbound messages, and a timeout on each request. That is the same
skeleton `client.ts` needs.

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

## Phase 0 — reconcile the deleted stub (½ day)

Independent of everything else, and it prevents shipping a tool that renders as
a blank row.

1. `ui_tools.rs:1484` reads `operation`/`file_path`. Retarget to omp's
   `action`/`file`/`line`/`symbol`, or delete the arm and let the default
   renderer handle it. **Deleting is defensible** — the file-tool port's
   deletion pattern says to retain display arms for names that appear in stored
   transcripts, and `lsp` calls do appear in old sessions from before
   `3d391a517`. Retarget, and comment why the arm predates the tool.
2. `jcode-base/src/safety.rs`: `classify` fails closed, so an unlisted `lsp`
   requires permission. That is right for the write actions and **wrong for the
   read-only ones**: every `hover` would interrupt the user. See "The approval
   problem" — this is a real design question, not a list edit, and it is the
   reason this item is in Phase 0 rather than discovered in Phase 3.
3. Record the test baseline (`cargo test --workspace` pass/fail counts) so
   "suite unchanged" is checkable. Note the fork currently has **8 known
   `jcode-tui` failures**, four confirmed to reproduce on pristine `v0.73.0`.

**Exit:** the renderer arm matches the schema this plan will ship; a baseline is
written down.

## Phase 1 — port the tests (3-4 days)

**This phase produces failing tests and no implementation.** Same as the file
tools, and for the same reason: it tells us in days rather than weeks whether
these behaviours fit jcode.

### The fake server comes first

`test/fixtures/fake-lsp-server.ts` (176 lines) is the foundation of all 77
regression tests. It is a real process speaking real framed JSON-RPC over stdio,
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

Of 5,027 lines and ~120 cases, roughly **70 are in scope for v1/v2**. Grouped by
what they pin rather than by file, because `lsp-regressions.test.ts` mixes
concerns freely:

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

**`sortAndValidateTextEdits` is 36 lines and every one of them is a bug someone
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

**Do not port:** the `mux` suite (387 lines), `lsp-format-options.test.ts` (153),
the `writethrough`/batching suite (182), workspace-diagnostics project detection
(`go.work` etc.), and `subagent-lsp.test.ts`.

### How to port a test

Same rule as last time, restated because it is the rule most easily broken:
**read the TypeScript, extract the assertion about behaviour, write a Rust test
asserting the same thing against our interface. Do not transliterate.**

Where a behaviour depends on a surface we lack, **drop the test and record why**
in `crates/jcode-lsp/PORTING_NOTES.md`. A dropped test is a decision, not an
omission.

**Exit, stated so it cannot be satisfied vacuously:** N Rust tests, each failing
with an **assertion** — not a compile error and not `todo!()` — where N is
enumerated per group in `PORTING_NOTES.md`. Target 40 for v1 (groups A-D, F-H)
and 12 more for v2 (group E).

## Phase 2 — implement the client (1 week)

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

Three options, and this is a **question for the user** (question 1 below):

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
the failure mode is invisible: nothing breaks, memory just grows.

### The approval problem

`SafetySystem::classify` (`jcode-base/src/safety.rs:180`) takes a **tool name**
and nothing else. `AUTO_ALLOWED` is a list of eleven read-only names. Anything
else requires permission — correct, and it fails closed.

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

This is **question 2** for the user, since it changes the tool surface and
therefore the schema, the prompt, and every test's entry point. It must be
decided before Phase 1 writes a test, not after.

## Phase 3 — v1 tool surface (3-4 days)

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
(`ast_tools.rs:240-290`) — and copy its **fix** too: `50418a767 fix(ast_grep):
show whole source lines, so the tag can actually be edited from` recorded that
showing a *fragment* of a line mints a tag the model cannot safely use. Show
whole lines.

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

## Phase 4 — v2: the write actions (1 week)

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
5. **`reload`, `capabilities`, `request`.** Small. `request` is the escape hatch
   for anything unported and is worth having early precisely because it makes
   the tool's gaps survivable.

**Every write action goes through the approval gate**, which is what the
two-tool split in Phase 2 buys. And note omp's own documented bug, which we
should fix rather than inherit: `apply: true` with `query` omitted silently
lists instead of applying. Their own doc calls it out. Return an error naming
the missing selector.

## Verification, and its honest ceiling

**Only `rust-analyzer` is installed on this machine.** That bounds live
verification hard, and pretending otherwise is how a port ships broken for
every other language.

| what | how |
|---|---|
| protocol correctness | the fake server. Deterministic, covers failure modes no real server reproduces on demand |
| one real server end to end | `rust-analyzer` on this repo. Real cold start, real multi-second project load, real diagnostics |
| every other server | **not verified.** Config entries for them are untested data |
| model-facing behaviour | live agent runs, per Phase 3 |

Two consequences to accept out loud:

- **Ship a small `defaults.json`, not all 49.** Include what can be reasoned
  about and ideally tested: `rust-analyzer`, `typescript-language-server`,
  `pyright`, `gopls`, `clangd`. An untested config entry is a claim we cannot
  support, and a wrong `args` array is a server that silently never starts.
  Config is user-extensible, so a missing entry is a config line, not a wall.
- **rust-analyzer needs special handling and it is not optional.** omp has
  `waitForRustAnalyzerWorkspace` with a 5s timeout, 100ms poll, 2s settle, plus
  `rust-analyzer/reloadWorkspace` and Cargo-workspace-file opening before
  polling. **This is the server we will actually be testing against**, so
  getting it wrong means every local check is unreliable. Port those four
  constants and the polling.

## Risks, named

| risk | mitigation |
|---|---|
| **We reship the stub** — a tool that advertises what it cannot do | No phase registers an action it cannot answer. `file: "*"` refuses by name rather than returning empty |
| **Servers leak in the daemon** | The registry question is Phase 2's first decision, and a shutdown test pins it. This is the failure mode with no symptom |
| **Framing bug** | The fake server exists specifically to split bodies and headers across reads. Do not accept a mock here |
| **Wedged server hangs a turn** | Every request has a timeout, abort sends `$/cancelRequest`, a wedged write tears the client down. Group A's tests |
| **Cold start is slow enough that the model avoids the tool** | Argues for warm shared servers (registry option (a)) and for honest `timeout` handling that says *why* it timed out |
| **Only one server verifiable** | Small `defaults.json`; untested entries stated as untested |
| **Approval fatigue kills adoption** | The two-tool split, so read-only is auto-allowed |
| **Hashline contract broken silently** | `lsp` is a producer from day one, not retrofitted. `50418a767`'s lesson: show whole lines |
| **A server message reaches the TUI unsanitized** | Group G ported before v1 ships |
| **Scope creep into `writethrough`** | Explicitly out, with its own future plan |
| **Rebase surface** | `jcode-lsp` is a new crate and cannot conflict. `tool/mod.rs` and `safety.rs` take pure insertions. `ui_tools.rs` is the one hot file, and Phase 0 touches it once |

## Sequencing

```
Phase 0  ½d   reconcile the stub's renderer arm, baseline   (independent)
Phase 1  3-4d fake server + ported tests → failing suite    (the gate)
Phase 2  1w   the client: framing, correlation, lifecycle
Phase 3  3-4d v1 tool: read-only actions + hashline + live run
Phase 4  1w   v2: WorkspaceEdit, rename, code_actions
```

**~3.5 weeks to v2**, against the survey's "large build: server lifecycle,
per-language config, a tool surface over ~14 ops. Worth scoping as an epic
before committing." This is that scoping, and the estimate is consistent with
it.

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
   49 as untested data? Recommend five.
4. **Is v2 in scope now, or does v1 ship and get measured first?** v1 is
   independently useful (navigation and diagnostics are most of the value) and
   `rename` is the headline capability. Recommend shipping v1, measuring
   adoption, then v2 — mirroring how the file-tool port gated its forgiveness
   layer on measurement.
