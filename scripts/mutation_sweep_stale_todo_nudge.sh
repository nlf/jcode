#!/usr/bin/env bash
# Mutation sweep for the stale-todo nudge guards.
#
# Every test in this feature asserts on a pure decision function, which is
# exactly the shape that passes for the wrong reason: an assertion that would
# hold whatever the guard did reads identically to one that pins it. So break
# each guard in turn and record which named test notices.
#
# NLFCODE.md's own history is the argument for this: two of three defects in the
# Config::save work were found by mutation rather than by a failing test, and a
# test whose name implied coverage it did not have was worse than a missing one.
#
# Usage: scripts/mutation_sweep_stale_todo_nudge.sh
# Exits non-zero if any mutation survives.

set -uo pipefail
cd "$(dirname "$0")/.."

TURN=crates/jcode-app-core/src/agent/turn_loops.rs
TODO=crates/jcode-base/src/todo.rs
BACKUP_DIR=$(mktemp -d)
cp "$TURN" "$BACKUP_DIR/turn_loops.rs"
cp "$TODO" "$BACKUP_DIR/todo.rs"
restore() {
  cp "$BACKUP_DIR/turn_loops.rs" "$TURN"
  cp "$BACKUP_DIR/todo.rs" "$TODO"
}
trap restore EXIT

survivors=0

# $1 label  $2 file  $3 python replacement expression  $4 cargo filter
mutate() {
  local label="$1" file="$2" script="$3" pkg="$4" filter="$5"
  restore
  python3 - "$file" <<PY
import sys
path = sys.argv[1]
text = open(path).read()
$script
open(path, "w").write(text)
PY
  local out
  out=$(cargo test -p "$pkg" --lib "$filter" 2>&1)
  # Order matters: a *test* failure also prints "error: test failed, to rerun
  # ...", so checking for /^error/ first reports every caught mutation as a
  # compile failure. That is the exact trap NLFCODE.md records - reading DID NOT
  # COMPILE as a neutral result - and the first version of this script fell into
  # it, reporting all 10 mutations untested when all 10 were caught.
  if echo "$out" | grep -q "test result: FAILED"; then
    local caught
    caught=$(echo "$out" | grep -E "^test [a-z_:]+ \.\.\. FAILED" | head -3 | sed 's/^test //;s/ \.\.\..*//' | paste -sd, -)
    printf '  %-58s caught by %s\n' "$label" "$caught"
    return
  fi
  if echo "$out" | grep -qE "^error\[E[0-9]+\]|^error: expected|^error: unexpected"; then
    printf '  %-58s DID NOT COMPILE (untested, not passed)\n' "$label"
    survivors=$((survivors + 1))
    return
  fi
  if ! echo "$out" | grep -q "test result: ok"; then
    printf '  %-58s NO TEST RESULT (untested, not passed)\n' "$label"
    survivors=$((survivors + 1))
    return
  fi
  printf '  %-58s *** SURVIVED ***\n' "$label"
  survivors=$((survivors + 1))
}

echo "Mutating the stale-todo nudge guards:"

mutate "threshold 12 -> 1 (fires during healthy work)" "$TURN" \
  'text = text.replace("const TOOL_CALLS_BEFORE_STALE_TODO_NUDGE: u32 = 12;", "const TOOL_CALLS_BEFORE_STALE_TODO_NUDGE: u32 = 1;")' \
  jcode-app-core turn_loops

mutate "backoff removed (interval never doubles)" "$TURN" \
  'text = text.replace(".saturating_mul(1u32 << nudges_sent.min(5))", ".saturating_mul(1)")' \
  jcode-app-core turn_loops

mutate "backoff cap removed (unbounded interval)" "$TURN" \
  'text = text.replace(".min(Self::MAX_STALE_TODO_NUDGE_INTERVAL)", "")' \
  jcode-app-core turn_loops

mutate "interval measured from write, not last nudge" "$TURN" \
  'text = text.replace("last_nudge_at.saturating_add(Self::stale_todo_nudge_interval(nudges_sent))", "Self::stale_todo_nudge_interval(nudges_sent)")' \
  jcode-app-core turn_loops

mutate "todo-availability guard dropped" "$TURN" \
  'text = text.replace("        todo_available\n            && has_todos", "        true\n            && has_todos")' \
  jcode-app-core turn_loops

mutate "empty-list guard dropped" "$TURN" \
  'text = text.replace("            && has_todos\n", "            && true\n")' \
  jcode-app-core turn_loops

mutate "batch sub-tool todo no longer resets counter" "$TURN" \
  'text = text.replace('"'"'names.iter().any(|name| name == "todo")'"'"', "false")' \
  jcode-app-core turn_loops

mutate "nudge stops naming the open todos" "$TODO" \
  'text = text.replace("    append_named_todos(&mut message, \"Still marked pending:\", &pending);", "")' \
  jcode-base todo::

mutate "nothing-in-progress advice dropped" "$TODO" \
  'text = text.replace("Nothing is marked in progress. If you are working on one of these, mark it in_progress now.", "")' \
  jcode-base todo::

mutate "nudge no longer recognized as synthetic" "$TODO" \
  'text = text.replace("        || trimmed.starts_with(TODO_STALE_LIST_CONTINUATION_MESSAGE)\n}", "\n}")' \
  jcode-base todo::

restore
echo
if [ "$survivors" -eq 0 ]; then
  echo "All mutations caught."
else
  echo "$survivors mutation(s) survived or were untested - a guard has no test that reaches it."
  exit 1
fi
