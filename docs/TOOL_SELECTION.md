# Tool selection: why models reached for bash, and what fixed it

A read-only exploration of this codebase used to run almost entirely through
`bash`. Six defects caused it, spanning the tool registry, the OAuth schema
curation, the destructive-command gate, the system prompt, and the tool
descriptions. This records what was wrong, what changed, and the measurement
that says it worked.

## The measurement

Same prompt, same model, before and after. Prompt: *"Read-only: find where the
ReadTool schema is defined and tell me which parameters it advertises."*

| | bash calls per run | tools actually used |
|---|---|---|
| Before | 2, 2, 3 | `bash`, `agentgrep` |
| After | 0, 0, 0, 0, 0 | `grep`, `glob`, `read`, `ls`, `batch` |

Reproduce by building a binary at `5b3924637~1` and one at HEAD, then running
each against its own socket:

```
./target/selfdev/jcode run --no-update --socket /tmp/probe.sock '<prompt>'
```

## What was wrong

**1. There was no `glob` or `grep` tool.** `Registry::base_tools` never
registered them. The Anthropic OAuth path curates Claude-Code builtin
definitions including `Glob` and `Grep`, then filters each through
`has_backing`, which requires a matching local tool. Both were silently
dropped, so a model with strong priors for them found them absent and fell
back to the thing that always works.

**2. Nothing advocated for the native tools.** The system prompt had no
tool-selection guidance, and no "prefer X over bash" phrasing existed anywhere
in the crates.

**3. The descriptions were stubs.** "Search code and file names." "List
directory contents." Against a known-universal `Bash`, a twelve-word stub
loses the expected-value comparison every time. The narrow tool has to earn
its place.

**4. `Read` advertised a schema it partly rejected.** The OAuth path
advertised `offset`/`limit`/`pages`; the local schema declared
`start_line`/`limit`; `pages` was in neither `ReadInput` nor the local schema,
so serde dropped it and a page request silently returned the whole document.

**5. The destructive gate refused read-only commands.** Four separate defects,
all the same family of mistaking a non-path for a deletion target. A segment
containing any truncating redirect skipped the `triggered` check and scanned
*every* operand as a deletion target, so `ls crates 2>/dev/null` was assessed
as destroying both `crates` and the null sink. The `/dev/null` allowance sat
behind `is_catastrophic_target`, which matches `/dev` recursively, so it was
unreachable. `2>&1` parsed as a redirect to a file named `1`. Heredoc bodies
were tokenized as commands, so writing a test fixture that merely mentioned
`rm -rf` was refused.

This compounded item 3: the refusal read as a syntax problem, so the model
retried bash with variations rather than switching tools.

**6.** The cross-cutting consequence, used as the acceptance test above.

## What changed

- `crates/jcode-app-core/src/tool/grep_glob.rs`: `grep` and `glob` as thin
  adapters onto `agentgrep`, so search behaviour stays in one place. Two
  translation details are load-bearing and pinned by tests: Claude-Code's
  `Grep` is regex by default while agentgrep's is literal, and a glob must
  reach agentgrep's glob filter rather than its ranking query.
- `crates/jcode-provider-anthropic/src/lib.rs`: curated builtins now inherit a
  richer local description when the registry has one, so tool-selection
  guidance survives curation. The curated schema is kept, since the endpoint
  expects the builtin shape.
- `crates/jcode-command-risk/`: the four gate fixes, with regression tests
  asserting both directions. Read-only commands run; `rm -rf ~`, `rm /dev/null`,
  `cat foo > /dev/sda` and `rm -rf ~ 2>/dev/null` are all still refused.
- `crates/jcode-app-core/src/tool/read.rs`: `pages` accepted and honoured, and
  the advertised schema now matches what deserializes.
- `crates/jcode-base/src/prompt/system_prompt.md`: a Tool selection section.

## Constraints worth knowing before editing this

Tool descriptions are capped at ~20 estimated tokens
(`tool_descriptions_stay_under_token_cap`) and parameter descriptions at ~25,
because both are always-on prompt cost. Guidance longer than that belongs in a
parameter description or the system prompt, not the tool description. The
anti-bash steer in each competing tool is deliberately short for this reason,
and `tools_competing_with_bash_name_it_as_the_wrong_choice` keeps it present.

Note that `batch` and `macos_computer_use` exceed the description cap, and
`integration_tools`, `macos_computer_use` and `todo` exceed the parameter cap.
Those predate this work and still fail on a clean worktree.
