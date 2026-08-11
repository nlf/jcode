#!/usr/bin/env python3
"""Replay recorded sessions against the stale-todo nudge policy.

The threshold and backoff in `Agent::should_inject_stale_todo_nudge` are the
whole design of that feature: fire too eagerly and the nudge becomes wallpaper
the model learns to skip, fire too rarely and it never catches the failure it
exists for. Neither end can be judged from a unit test, because both depend on
what real sessions actually look like.

So this replays the policy over recorded sessions and reports the firing rate.
Re-run it after changing FIRST_INTERVAL, MAX_INTERVAL, or the decision function
in `crates/jcode-app-core/src/agent/turn_loops.rs`, and keep the two in sync.

It caught a real bug the unit tests did not: measuring the backoff interval from
the last todo *write* rather than from the last *nudge* leaves the condition
permanently true once crossed, so it re-fires every call. That produced 155
nudges in a 541-call session, worse than no backoff at all, and it read as
correct in the source.

Usage:
    python3 scripts/replay_stale_todo_nudge.py [--sessions N] [--min-calls N]

Baseline recorded 2026-08-11 over the 120 largest sessions:
    55 sessions >=25 calls, 14283 tool calls
    one nudge per 46 calls overall
    one nudge per 33 calls among the 18 sessions that ever wrote a todo list
    37 sessions never wrote a list and are silent (the has_todos guard)
"""

import argparse
import glob
import json
import os

# Keep in sync with Agent::TOOL_CALLS_BEFORE_STALE_TODO_NUDGE and
# Agent::MAX_STALE_TODO_NUDGE_INTERVAL.
FIRST_INTERVAL = 12
MAX_INTERVAL = 96


def interval(nudges_sent: int) -> int:
    """Mirror of Agent::stale_todo_nudge_interval."""
    return min(FIRST_INTERVAL * (2 ** min(nudges_sent, 5)), MAX_INTERVAL)


def tool_calls_of(message) -> list[list[str]]:
    """Tool calls in one message, each as the names it really executes.

    A `batch` runs its sub-tools, so a `todo` inside a batch has to count as
    touching the list; missing that would nudge a model right after it did the
    right thing.
    """
    content = message.get("content")
    if not isinstance(content, list):
        return []
    calls = []
    for block in content:
        if not isinstance(block, dict) or block.get("type") != "tool_use":
            continue
        names = [block.get("name")]
        if block.get("name") == "batch":
            for sub in (block.get("input") or {}).get("tool_calls") or []:
                if isinstance(sub, dict) and sub.get("tool"):
                    names.append(sub["tool"])
        calls.append(names)
    return calls


def is_real_user_turn(message) -> bool:
    """A message the user actually typed, which is where the counter resets.

    Session-context and memory injections are also role=user, so matching on
    role alone would split every turn and hide every long one.
    """
    if message.get("role") != "user":
        return False
    content = message.get("content")
    text = None
    if isinstance(content, str):
        text = content
    elif isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = block.get("text")
    return bool(text) and not text.startswith("<system-reminder>")


def replay(path: str):
    try:
        with open(path) as handle:
            data = json.load(handle)
    except (OSError, ValueError):
        return None
    messages = data.get("messages") if isinstance(data, dict) else None
    if not messages:
        return None

    turns, current = [], []
    for message in messages:
        if is_real_user_turn(message):
            if current:
                turns.append(current)
            current = []
        current.extend(tool_calls_of(message))
    if current:
        turns.append(current)

    total = nudges = writes = 0
    # The runtime guard suppresses the nudge until a list exists at all.
    has_list = False
    for turn in turns:
        # Counters are per turn-loop: a new user turn starts clean.
        since = sent = last_nudge_at = 0
        for names in turn:
            total += len(names)
            if "todo" in names:
                writes += 1
                has_list = True
                since = sent = last_nudge_at = 0
            else:
                since += len(names)
                if has_list and since >= last_nudge_at + interval(sent):
                    nudges += 1
                    sent += 1
                    last_nudge_at = since
    return total, nudges, writes


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sessions", type=int, default=120,
                        help="how many of the largest sessions to replay")
    parser.add_argument("--min-calls", type=int, default=25,
                        help="ignore sessions shorter than this")
    args = parser.parse_args()

    home = os.environ.get("JCODE_HOME", os.path.expanduser("~/.jcode"))
    paths = sorted(glob.glob(os.path.join(home, "sessions", "session_*.json")),
                   key=os.path.getsize)[-args.sessions:]

    rows = []
    for path in paths:
        result = replay(path)
        if result and result[0] >= args.min_calls:
            rows.append((*result, os.path.basename(path)))
    if not rows:
        print("no sessions matched")
        return

    total = sum(row[0] for row in rows)
    nudges = sum(row[1] for row in rows)
    with_list = [row for row in rows if row[2] > 0]
    listed_calls = sum(row[0] for row in with_list)
    listed_nudges = sum(row[1] for row in with_list)

    print(f"{len(rows)} sessions, {total} tool calls")
    print(f"nudges: {nudges} -> one per {total // max(nudges, 1)} tool calls")
    print(f"  sessions that ever wrote a todo list: {len(with_list)}")
    print(f"    {listed_calls} calls -> {listed_nudges} nudges "
          f"(one per {listed_calls // max(listed_nudges, 1)})")
    print(f"  sessions with no list, silent by the has_todos guard: "
          f"{len(rows) - len(with_list)}")
    print("\nheaviest sessions:")
    for row in sorted(with_list, key=lambda row: -row[1])[:10]:
        print(f"  {row[1]:3d} nudges /{row[0]:5d} calls, {row[2]:3d} writes  {row[3][:44]}")


if __name__ == "__main__":
    main()
