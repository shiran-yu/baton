---
name: baton
description: Hand bounded coding work from Claude Code to Codex or Antigravity (agy) and check it, to save the lead's usage. Use when a task is well specified and checkable by commands (implement with tests, refactor, bugfix with a repro, docs, mechanical edits, a second-opinion review), when the user asks to use Codex, agy or Antigravity, or asks how much quota each agent has left.
---

# baton: lead the workers

You are the lead. Codex and agy do bounded work; you decide, brief, and judge. Every token you
spend reading a worker's transcript is a token the delegation was meant to save, so read only
what baton returns.

## When to delegate

Delegate when all hold: the outcome is checkable by commands (tests, build, lint, a script), the
files to touch are known, and no design decision is left open. Keep for yourself: anything that
touches trading, money, `keyfob` or secrets (baton refuses those briefs anyway), architecture and
API decisions, ambiguous requests, and edits too small to brief (a brief costs ~300–600 tokens).

## 1. Check capacity and pick the worker

```sh
baton usage                       # 5 h and weekly remaining for claude, codex, agy
baton route --brief <file>        # worker, model, effort and the reason
```

Follow the route unless you know better; say why if you override it.

## 2. Write the brief

Save it under the repo's `.baton/` (git-ignored) as Markdown with these headings:

~~~markdown
## Goal
What done means, in a few sentences.

## Files
src/parser/**
tests/parser_*.rs

## Forbidden
New dependencies; changing the public API; touching CI.

## Acceptance
```
cargo test -q
cargo clippy --all-targets -- -D warnings
```

## Context
Pointers, not pasted code: "the tokenizer lives in src/lexer.rs; follow its error style".
~~~

Make acceptance commands strong enough that passing them means done: baton reruns them itself.

## 3. Dispatch

Visible in the agent panel (recommended): call the Agent tool with `subagent_type` `baton:codex` or
`baton:agy`, a description naming the job, and a prompt of one line:
`brief: /abs/path/.baton/brief-x.md  flags: --effort medium`. The panel row shows live progress
(`codex · medium · 3 files · tests 12/14 · 80k tok · 5h 14%`).

Zero lead tokens while it runs: `baton run codex --brief <file> --json` with Bash
`run_in_background: true`; you are notified when it exits. Several independent tasks can run at
once; each gets its own worktree.

## 4. Judge

The result is one JSON object: `status`, `verified` (baton's own check: diff present, files in
scope, acceptance rerun), `files_changed`, `diffstat`, `out_of_scope`, `acceptance`, `notes`,
`questions`, `tokens`.

- `verified: true` and the diffstat fits the goal → read the diff (`baton show <task> --diff`),
  skim for design problems the tests cannot see, then `baton accept <task>`.
- For larger diffs, get a cheap second opinion first: `baton review <task> --by agy` (or `--by codex`).
- Fixable problems → write the fix list to a file and `baton fix <task> --notes <file>`. Two
  rounds at most, then take it over yourself.
- `needs_decision` → answer the `questions` in a notes file and `baton fix`.
- `verified: false` with `out_of_scope` → decide whether the extra files were needed; widen
  `## Files` and fix, or drop.

Tell the user in one line what you delegated, to whom, and the outcome with its token count.

## Quota discipline

- `baton usage` before a batch of work; `baton route` already weighs remaining quota and pace.
- Codex and agy calls carry a fixed prompt of 12–15k tokens each (mostly cached for Codex). Batch
  related changes into one brief rather than many small ones.
- Do not resume agy conversations; baton starts fresh runs for agy fixes on purpose.

## Reference

Command maps, lean flags and measured token costs of all three CLIs: the `baton-reference` skill.
