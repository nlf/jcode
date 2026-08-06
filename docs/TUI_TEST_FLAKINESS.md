# jcode-tui test flakiness: root cause

`cargo test -p jcode-tui --lib` fails 1-4 tests per run, with a varying set.
This is a parallelism race on process-global state, not a logic bug.

## Update 2026-08-06: it now deadlocks, it does not just flake

At the default thread count the suite no longer finishes at all. Sampling a
hung run (`sample <pid>`) shows a **lock-order inversion** between the two test
mutexes:

- `jcode_base::storage::lock_test_env` (env lock), taken by tests that scope
  their own `JCODE_HOME`.
- `tui::ui::render_state_test_lock` (render lock), taken by rendering tests.

Both orders exist in the suite:

| Order | Taken by |
|---|---|
| env → render | tests holding `lock_test_env` that then call `create_test_app` (→ `clear_test_render_state_for_tests` → render lock) |
| render → env | rendering tests holding the render lock whose body then calls `lock_test_env` |

One sampled run had one thread holding render and blocked on env, while three
held env and blocked on render. Every test worker was parked; no thread was
running.

### What does not work (measured, reverted)

**Making the render lock acquire the env lock first** to force a single global
order. The env lock must then be reentrant, because test bodies call
`lock_test_env` again after the render lock already took it. Faking reentrancy
by handing back a guard over a *different* mutex reintroduces the deadlock:
that substitute mutex is itself global, so two threads on the reentrant path
block on each other. Verified by sampling: the suite advanced further (from the
`test_a*` tests to the `colors::`/`helpers::` block) and then stalled with all
12 workers parked and zero CPU growth over 45s. Reverted.

A real fix needs `lock_test_env` to return an owned reentrant guard (tracking
depth per thread and releasing only at depth zero), or the shared state removed
per "Suggested direction" below. The latter is still preferred.

### Single-threaded is also not clean any more

`--test-threads=1` completes but does **not** pass 2006/2006 as recorded below.
Measured 2026-08-06:

| Commit | Result |
|---|---|
| `02439b492` | 2125 passed, 11 failed |
| `4dc8e5c61` | 2147 passed, 9 failed |

The 9 are a strict subset of the 11: the two `test_copy_badge_*` failures were
platform-dependent tests hardcoding the non-macOS `[Alt]` label (14 columns)
where macOS renders `[⌥]` (12), so they passed on Linux CI and failed on every
macOS checkout. Fixed in `4dc8e5c61`. The remaining 9 are unrelated to widget
or render layout and are still open.

## Evidence

- `cargo test -p jcode-tui --lib -- --test-threads=1` passes **2006/2006** (16 ignored).
- The failing set changes between runs at the default thread count.
- Individually, each failing test passes when run alone.

Counts were taken on 2026-07-27 and will drift as tests are added. Reproduce
on an otherwise idle machine: under memory pressure (this host has 15 GiB and
was running concurrent workspace builds) `cargo` gets SIGTERMed mid-compile,
which is a different failure from the race described here.

## Root cause

`create_test_app()` (and its `create_named_provider_test_app` sibling) in
`crates/jcode-tui/src/tui/app/tests/support_failover/part_01.rs` calls:

```rust
crate::tui::ui::clear_test_render_state_for_tests();
```

That wipes **process-global** render state: the flicker frame history, layout
snapshots, status-area snapshots, copy targets, and scroll positions.

Rendering tests guard exactly that state with `render_state_test_lock()`. But
`create_test_app` clears it *without* taking the lock, so any of its ~810 call
sites can reset a concurrently-running render test's state mid-assertion.

The mechanism for the most frequent victim
(`test_changelog_overlay_repeated_renders_are_stable`) is documented in
`clear_test_render_state_for_tests` itself: a recorded flicker event adds a
"⚠ flicker detected" notification line to later renders, shifting every
layout-sensitive assertion by a row.

### Bisected proof

Bisecting the 959 `tui::app::tests::` tests against the changelog test
identifies `test_tui_login_providers_have_real_tui_handlers`, which calls
`create_test_app()` in a loop (once per login provider). Running just those two
does not reproduce; the race needs enough concurrent load to interleave, which
is why it presents as order-dependent flakiness.

## What does not work

**Taking `render_state_test_lock` inside `create_test_app`.** This is correct
but serializes all ~810 call sites: suite runtime goes from ~12s to over 10
minutes. Measured, then reverted.

**Asserting a floor instead of an exact count** in the changelog test's
`buffered_samples` check, and **calling `clear_test_render_state_for_tests`**
at the top of that test. Both measured over 5 runs: the test still failed 5/5
with *and* without the change. Reverted rather than committed as churn.

## Suggested direction

The real fix is to stop sharing this state across tests rather than to
serialize access to it:

1. Make the render state thread-local rather than process-global, so parallel
   tests cannot observe each other's resets. Production has one render thread,
   so this should not change runtime behavior.
2. Failing that, have `create_test_app` skip the render-state clear entirely.
   Only rendering tests depend on it, and they already clear it under the lock.
   This needs an audit of which app tests implicitly rely on the current clear.

Option 1 is preferred: it removes the shared mutable state instead of adding
coordination around it.

## Scope note

This is pre-existing and independent of the render-path performance work in
commits `0ba0154c6`, `2b8e78e34`, `8b44fc83b`, `8142f1a0b`. Verified by
stashing those changes and reproducing the same failure rate.
