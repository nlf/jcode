#!/usr/bin/env python3
"""Count tool calls in a jcode --ndjson stream, including calls nested in batch.

The human-readable transcript is not a sound source for this. A top-level call
prints `[toolname]`, but a call inside `batch` appears only in a truncated
preview that shows the first sub-call, so a batch containing `read` and `bash`
prints only `--- [1] read ---` and the bash call is invisible.

This reassembles the streamed `tool_input` deltas per tool_start id and reads
the real names out of the resulting JSON, so nested calls are counted.
"""
import json
import sys
from collections import Counter


def main() -> int:
    current_id = None
    names = {}
    buffers = {}

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        kind = event.get("type")
        if kind == "tool_start":
            current_id = event.get("id")
            names[current_id] = event.get("name")
            buffers[current_id] = []
        elif kind == "tool_input" and current_id is not None:
            buffers[current_id].append(event.get("delta", ""))

    calls = Counter()
    for call_id, name in names.items():
        calls[name] += 1
        if name != "batch":
            continue
        # A batch call carries its sub-calls in its own input payload.
        try:
            payload = json.loads("".join(buffers.get(call_id, [])))
        except json.JSONDecodeError:
            print(f"WARNING: could not parse batch input for {call_id}", file=sys.stderr)
            continue
        for sub in payload.get("tool_calls", []):
            sub_name = sub.get("tool")
            if sub_name:
                calls[sub_name] += 1

    bash = calls.get("bash", 0) + calls.get("Bash", 0)
    listing = " ".join(f"{n}x{c}" if c > 1 else n for n, c in sorted(calls.items()))
    print(f"bash={bash} | {listing}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
