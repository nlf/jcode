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

Both caps are now enforced with nothing over them, so a new tool that exceeds
one will fail the suite rather than quietly adding always-on cost. Two patterns
are worth copying when that happens: `macos_computer_use` keeps its description
short by deferring the full action set to its own `discover` action, and
`batch` keeps its worked example in the `tool_calls` parameter description
rather than the tool description.

A related trap, hit three times while doing this: several tests asserted on
exact sentences of tool copy, so a later reword that preserved the meaning left
them failing on main. Assert what the text has to *say*, not the sentence it
says it in.

## PDF pages come from the extractor, not from splitting text

Use `jcode_pdf::extract_text_by_page`. Do not split the output of
`extract_text` on `\x0c`: `pdf_extract`'s `PlainTextOutput::end_page` is a
no-op and emits no page separator of any kind, so splitting sees every document
as a single page. A comment in `read.rs` asserted the opposite, and the first
version of the `pages` parameter was built on it, which meant a request for
page 3 of 5 answered that the page did not exist.

Worth noting how that survived review: the parser had unit tests and they all
passed, because they only ever exercised the *selection string* ("2-5" parses
to [2,3,4,5]) and never a real document. The bug was one layer below, in what
the pages were being selected *from*. A test that stops at the boundary of the
thing you changed will not tell you the premise underneath it is false.

## Verify tool behaviour in-process, not by asking an agent

`crates/jcode-app-core/src/tool/grep_glob_tests.rs` has an `end_to_end` module
that runs `GrepTool`/`GlobTool` against fixture files and asserts on the real
output. Prefer that over `jcode run '<prompt asking the model to call X>'`.

Live probing was actively misleading when checking whether the adapter passes
`regex: true` through. Several runs reported zero matches for an alternation,
which looked exactly like a regex bug, but the agents had called `agentgrep`
(literal by default) while reporting they had called `grep`, and one
contradicted its own earlier answer within a single response. The in-process
test settled it in one run: a bare alternation returns 2 matches in 2 files.

Live runs are still the right tool for the question they actually answer,
which is *which* tool a model reaches for. That is what the bash-count
measurement above uses them for. They are a poor instrument for what a tool
then does.

## Known unrelated failures

Three `jcode-base` tests fail on this machine and are unrelated to any of the
above. Confirmed failing at `0e09472c2`, which predates this work:

- `platform::platform_tests::spawn_detached_creates_new_session`
- `provider::tests::test_hosted_model_guard_does_not_gate_models_by_legacy_tier`
- `session::tests::cases::streaming_guard_creates_visible_macos_sleep_assertion`

Recorded so the next person does not re-derive that they are not their fault.
They are real failures and worth fixing, just not here.
