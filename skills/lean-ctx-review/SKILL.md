---
name: lean-ctx-review
description: Review how the lean-ctx ctx_* MCP tools performed in the current session and file upstream issues for confirmed problems. Use when the user asks to review/audit lean-ctx tool performance, says "lean-ctx review", asks why ctx_* tools failed or fell back to native tools, or wants to report a lean-ctx bug.
---

# lean-ctx session review

Upstream repo: `yvgude/lean-ctx` (https://github.com/yvgude/lean-ctx).

Reads a Claude Code session transcript, so it applies to agents that write
`~/.claude/projects/*/*.jsonl`. Other harnesses need their own extractor; the
triage rules below are harness-independent.

## 1. Extract

Run from this skill's directory (needs `python3`):

```bash
python3 scripts/scan_session.py
```

Defaults to the newest transcript under `~/.claude/projects/*/` — the live
Claude Code session. Pass a path to review a different one. `-d N` dumps call
`#N` in full.

The script reports **facts, not verdicts**: every `ctx_*` call in order, which
results the harness flagged `is_error`, which calls repeated identical
arguments, which asked for `raw`/`fresh` output, and how many native
`Read`/`Grep`/`Bash` calls happened. It deliberately does not guess which of
those is a problem — that judgment is yours, below.

Transcripts run to megabytes, so read the table, not the raw JSONL.

## 2. Triage

**Start from `is_error`.** In practice every genuine tool failure carries it.
Then read the call list for the softer signals: identical retries, `raw`/`fresh`
re-reads, native calls clustered right after a `ctx_*` call, or a sequence that
only completed once `ctx_*` was abandoned.

### Before absolving anything, do these two things

Skipping them is how this review returns a false "nothing to report". Both cost
one command each.

**Search the tracker for the guard or tool you are about to excuse.** Do this
during triage, not at step 4. A closed issue defines intended behaviour: if a
shipped fix says a form is supported and you just watched it fail, that is a
regression and one of the most valuable things this review finds. It also stops
you re-filing a settled design decision.

```bash
gh issue list --repo yvgude/lean-ctx --search "<guard or tool keyword>" --state all
```

**Probe the stated rationale — never accept a message's self-description.**
An error explains itself, and that explanation can be wrong. Three checks, each
one command:

- *Does the sanctioned alternative achieve the same thing?* If the block is
  bypassable through a route the message itself recommends, it constrains syntax,
  not capability. Sometimes that is deliberate (structured input parses reliably
  where a shell string does not) — decide which, do not assume.
- *Does the stated reason match the observed rule?* Vary one axis at a time. A
  guard blaming payload size that actually keys on path, or blaming a pipe on a
  command that is piped, is misdiagnosing itself.
- *Does the message contradict itself?* A rejection that also states the rejected
  form is allowed is a finding on its own, whichever way the policy should go.

A wrong diagnostic is a real bug even when the block is correct: it sends the
next caller down a route that cannot work.

### Not a finding

- **Words inside returned content.** A file containing `http.StatusConflict`, a
  `git rebase` printing `CONFLICT (content):`, `gh` returning
  `"mergeable":"CONFLICTING"`, a test run printing `not found`. The tool
  returned exactly what was asked for. Judge the tool's own behaviour, never the
  payload's vocabulary.
- **Self-reference.** Any result that quotes a previous scan, this skill, or a
  transcript will echo every error word in it.
- **Guard blocks — only after they survive the probe above.** Inline env
  overrides (`GIT_EDITOR=`), shell redirects (`>`/`>>`), non-allowlisted
  commands, paths outside the project root. "It offered an alternative and the
  alternative worked" is *not* enough to clear one: that is true of a guard whose
  reason is wrong, whose rule is different from its description, or that
  contradicts itself. Clear it only once the stated rationale holds up.
- **Malformed tool input.** `InputValidationError` on `__unparsedToolInput` is
  the caller's own JSON serialization — commonly a raw tab or newline inside a
  string — rejected before lean-ctx ever ran.
- **My own wrong arguments**, a genuinely missing file, a failing build.

### Confirmable

The tool did something wrong or unhelpful *given correct input*:

- wrong, lossy, or truncated output where the mode promises otherwise
- content injected into output documented as verbatim, or anything that breaks a
  documented output format (e.g. corrupting batch-read separators)
- a crash, hang, or schema mismatch against the documented parameters
- a documented mode behaving differently than described
- a retry that only succeeded after dropping to native tools
- **a wrong or self-contradicting diagnostic**, even where the block itself is
  right — naming the wrong cause sends the next caller down a route that cannot
  work, and costs more than the block did
- **a regression against a closed issue**: a shipped fix says the form is
  supported, and it is not

## 3. Reproduce

**No minimal repro, no issue.** Re-run the exact tool and arguments. If it
passes, try to isolate the trigger; if you cannot, do not file — report the
observation to the user instead, saying plainly that it did not reproduce and
what you tried. Guessing at internals is worse than silence.

`lean-ctx --version` for the version line.

Zero confirmed findings is a valid and common result. Say so and file nothing.

## 4. File

Only for confirmed findings, one issue per distinct problem. You searched the
tracker during triage; widen it here if the finding shifted:

```bash
gh issue list --repo yvgude/lean-ctx --search "<keywords>" --state all
```

Skip if already reported — and read the close text before re-filing, since a
closed issue may have already conceded the point you are about to raise. When a
finding contradicts a closed fix, cite it and frame the issue as the gap in that
fix. Otherwise:

```bash
gh issue create --repo yvgude/lean-ctx --title "..." --body-file <file>
```

Body: what happened, expected, minimal repro (exact tool + args), version, OS.
No speculation about internals. End every issue body with the Claude Code
attribution footer. Report the issue `html_url` back to the user.
