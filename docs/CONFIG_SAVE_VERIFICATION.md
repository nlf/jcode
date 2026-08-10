# `Config::save` formatting preservation: every claim, and how it was checked

Written 2026-08-10 alongside `f5eb4f860..4b771927a`. Each row is a claim the
change makes, the concrete check, and the result actually observed. Aggregate
test counts are deliberately absent: they establish that tests pass, not that
any particular claim is true.

Re-runnable: `python3 scripts/mutation_sweep_config_save.py` and
`cargo test -p jcode-base --lib format_tests`.

## Behavior claims

| # | claim | check | observed |
|---|---|---|---|
| 1 | comments on untouched keys survive | `a_comment_on_an_untouched_setting_survives_a_save` | pass; **fails** on pre-fix code |
| 2 | comments above the changed key survive | `a_comment_on_the_changed_setting_survives_a_save` | pass; **fails** on pre-fix code |
| 3 | trailing comments on the changed line survive | `a_trailing_comment_on_the_changed_line_survives` | pass; **fails** on pre-fix code |
| 4 | key order is preserved | `key_order_survives_a_save` | pass; **fails** on pre-fix code |
| 5 | keys `Config` does not model survive | `a_key_the_struct_does_not_model_survives_a_save` | pass; **fails** on pre-fix code |
| 6 | a no-op save changes no bytes | `a_save_that_changes_nothing_leaves_the_file_byte_identical` | pass; **fails** on pre-fix code |
| 7 | repeated saves stay stable | `a_second_save_does_not_write_the_whole_struct_out` | pass; **fails** on pre-fix code |
| 8 | array-of-tables is not flattened | `an_array_of_tables_survives_a_save_unchanged` | pass; **fails** on pre-fix code |
| 9 | the shipped template round-trips | `the_shipped_template_survives_a_save_with_its_comments` | pass; **fails** on pre-fix code |
| 10 | a new key can be added | `a_new_setting_lands_in_a_section_that_did_not_exist` | pass; **guard** (passes either way) |
| 11 | a removal clears the file text | `a_removal_deletes_the_key_from_the_file_text` | pass; **guard** |
| 12 | a corrupt file is still saveable | `a_corrupt_config_file_can_still_be_saved_over` | pass; **guard** |
| 13 | a first save creates the file | `a_save_with_no_existing_file_creates_one` | pass; **guard** |
| 14 | concurrent edits still survive (pre-existing contract) | `saving_one_setting_does_not_revert_a_concurrent_edit` | pass, unchanged |
| 15 | deliberate removal still applies (pre-existing contract) | `save_still_applies_a_deliberate_removal` | pass, unchanged |

"fails on pre-fix code" was measured by running these same tests against a
worktree at `f5eb4f860~1` via `--manifest-path`, not by reasoning. 9 fail, 4
are guards. The guards are recorded as guards precisely because they are *not*
evidence this change did anything.

## Implementation claims, by mutation

Each helper was broken and the suite re-run. A mutation nothing catches is code
with no check behind it.

| broken behavior | caught by | result |
|---|---|---|
| `set_preserving_decor` drops decor | `a_trailing_comment_on_the_changed_line_survives` | caught |
| `clear_loaded_snapshot` skipped on missing file | `a_save_with_no_existing_file_creates_one` | caught |
| `changed_keys` reports untouched keys as changed | `a_save_that_changes_nothing...` +3 | caught |
| `apply_changes` ignores removals | `a_removal_deletes_the_key_from_the_file_text` | caught |
| `descend_or_create` will not create a table | `a_new_setting_lands...` +2 | caught |
| `save` re-snapshots from written text | `a_second_save_does_not_write_the_whole_struct_out` | caught |
| `save` re-serializes instead of patching | 10 of the 13 | caught |

**0 surviving mutations.** Two rows here were holes when first measured, and
both were real defects rather than missing tests. See "What this found".

## Non-behavior claims

| claim | check | observed |
|---|---|---|
| `toml_edit` adds no new crate | `git diff` on `Cargo.lock`; `cargo tree -i toml_edit` | one *edge* added, no `[[package]]`; already present at 0.20.2 via `toml` 0.8.2, versions identical |
| deleting `merge_changed_keys` breaks no caller | `cargo check --workspace --all-targets` | exit 0 |
| the copilot test no longer writes the real config | md5 around that one test, by full path | unchanged; **and with the fix reverted it does write**, so the counterfactual holds |
| the budget edit touched only my file | parsed both JSONs and diffed `tracked_files` | exactly 1 file's counts changed (+2), file set identical, total +2. The `cloud_relay.rs` hunk in the raw diff is pure key reordering, values identical |
| panic budget clean | `scripts/check_panic_budget.py` | no `config_file.rs` entry after removing the added `expect()` |

## Live, through the running daemon

Not a test: the shipped binary, the real `~/.jcode/config.toml`.

| path | command | observed |
|---|---|---|
| flat key | `/alignment centered` | exactly 1 line of 244 changed, all planted comments intact |
| nested table | `/colors border #665c54` | exactly 1 line changed inside `[display.colors]` |
| no-op | `/colors border` to its current value | **zero bytes changed** |

Config restored to md5 `96f9c5c4` afterwards, byte-identical to before.

## What this found

Three defects, two of which no failing test would have surfaced:

1. **The second save wrote the whole struct out.** Invisible to all five
   single-save tests. Caught by round-tripping the real template and counting
   lines (667 -> 754).
2. **A stale baseline silently dropped settings** when the config was missing
   or corrupt (`b66118b09`). Surfaced as a ~1-in-3 flake; the fix's own test
   was passing for the wrong reason until it seeded the hostile baseline itself.
3. **`set_preserving_decor` had no test reaching it** (`c31b392e6`). Deleting
   its body broke nothing, including the test named for that exact case.

## Method notes worth keeping

- **A live check that passes against the unfixed build is not evidence.** I
  first "verified" this via `jcode provider add`, which produced a perfect
  result on both binaries because it hand-appends TOML and never calls
  `Config::save()`.
- **`cd /tmp/wt && cargo test` does not do what it looks like.** The `cd` does
  not persist, so the tests ran against the new binary and reported a
  believable-but-wrong number. Use `--manifest-path`.
- **`OK: queued` is not a result.** Two live attempts used commands that do not
  exist (`/centered`, `/colors set border`); both were accepted and did
  nothing. Check the effect, never the acknowledgement.
- **A test whose name implies coverage it lacks is worse than a missing test.**
