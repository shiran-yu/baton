# baton — design

Claude Code leads. Codex CLI and Antigravity CLI (`agy`) do bounded work. Any of the three can call
the others through one binary, `baton`, written in Rust. Efficiency rules every decision: the lead
reads a few hundred tokens per delegated task, never a worker's transcript.

## 1. Flow

1. The lead writes a brief (§3) and calls `baton route` (§8), or names the worker.
2. The lead dispatches through a thin subagent (`baton:codex`, `baton:agy`, §7) so the run shows
   up as a row in Claude Code's agent panel, or runs `baton run` in a background Bash task.
3. `baton run` starts the worker with its profile (§5) in a fresh git worktree, streams
   progress to a status file, and when the worker stops it verifies the claim itself: the diff
   is not empty for an edit task, every changed path is allowed by the brief, and the acceptance
   commands pass when baton reruns them.
4. If verification fails, baton itself sends the failing acceptance output back to the same worker
   (Codex resumes its thread; agy gets the next turn on its open stream session, where the prompt
   cache is warm) and verifies again, up to `--auto-fix` rounds (default 1). The lead only ever
   sees the final state.
5. `baton run` prints one result object (§4) and appends a ledger line (§9).
6. Optionally a second worker reviews read-only (`baton review`), returning findings in the same
   contract.
7. The lead reads the result, the diff stat and the findings, then accepts (`baton accept` merges
   the worktree branch), sends fixes (`baton fix`), or takes over. Two fix rounds at most.

## 2. Commands

```
baton run <codex|agy|copilot|auto> --brief FILE [--mode edit|read] [--effort low|medium|high|xhigh]
          [--model SLUG] [--task ID] [--base REF] [--timeout SECS] [--no-worktree] [--json]
          [--auto-fix N] [--detach]
baton wait <task> [--max SECS]         block until the task ends or SECS pass; prints the result
                                       line, or one progress line and exit 7 while it still runs
baton fix <task> --notes FILE          send the fix list back to the same worker (Codex resumes its
                                       thread; agy starts fresh with the notes and the current diff)
baton review <task> --by <codex|agy|claude>   read-only review of a task's diff
baton accept <task> [--squash]         merge the task branch into the base branch, remove the worktree
baton drop <task>                      remove the worktree and branch
baton show <task> [--diff|--log|--brief]
baton ps [--json]                      running and recent tasks
baton usage [--json] [--refresh]       remaining quota of lead and workers, cached 5 minutes
baton route --brief FILE [--json]      recommended worker, model and effort, with the reason
baton ledger [--since DAYS] / baton scorecard   per worker, per task type
baton statusline subagent              Claude Code subagentStatusLine handler (stdin -> rows)
baton statusline line                  one compact usage line for a main status line
baton statusline hint                  that line plus the first worker eligible for a D2 task (prompt hook)
baton doctor [--quick]                versions, auth-file presence, global agy hooks and usage;
                                       --quick checks executables and writable state only
baton doctor --scan --out DIR         capture installed CLIs and subcommands through depth 3
baton sync [--dry-run] [--to codex,agy,copilot]   mirror installed Claude Code plugins into Codex, agy and Copilot
baton hook-adapt --from <codex|agy> -- CMD...   run a Claude-format hook under another host
baton setup                            fresh machine: install codex and agy, log in, write profiles,
                                       register marketplaces, sync, self-test
```

Exit codes: 0 done, 2 usage error, 3 worker claimed success but verification failed,
4 auth or quota problem, 5 timeout, 6 needs a decision (see `questions`), 7 still running (`wait`).

## 3. Brief

Markdown with these level-2 headings, in any order. `baton run` refuses a brief that lacks one.

| Heading | Content |
|---|---|
| `## Goal` | what done means, in a few sentences |
| `## Files` | paths or globs the worker may change; `none` for a read-only task |
| `## Forbidden` | things it must not do |
| `## Acceptance` | shell commands, one per line in a code block; all must exit 0 |
| `## Context` (optional) | pointers the worker needs; paths, not pasted file contents |

Risk lint: a brief that mentions `keyfob`, order placement or account endpoints of the trading
venues, or asks to read secrets is refused for codex and agy (exit 2). Those stay with the lead.

## 4. Result contract

Every worker returns, and `baton run` prints, this object (schema in `plugins/baton/schema/result.json`):

```json
{
  "task": "t-20261019-1432-ab12", "agent": "codex", "model": "gpt-6-astra", "effort": "xhigh",
  "status": "done | partial | blocked | needs_decision | failed",
  "verified": true,
  "files_changed": ["src/x.rs"], "diffstat": "+120 -40 in 3 files",
  "out_of_scope": [],
  "acceptance": [{"command": "cargo test", "passed": true, "tail": "test result: ok. 42 passed"}],
  "notes": "one or two sentences from the worker",
  "questions": [],
  "tokens": {"input": 190369, "cached": 156288, "output": 5927},
  "seconds": 312, "worktree": "~/.local/share/baton/wt/t-...", "branch": "baton/t-..."
}
```

`verified` is baton's own check (§1.3), not the worker's claim. `status` from the worker is kept;
a `done` with failed verification becomes exit code 3.

## 5. Worker profiles

Each worker runs its strongest model at its highest effort, with the user's own config, plugins
and skills: reliability over cost (user, 2026-10-29: "模型都要用最好的…贵没关系, 主要是可靠").
`--model` and `--effort` override; neither the task's difficulty nor routing (§8) lowers them.
The token levers measured before that decision (`--ignore-user-config`, lean agy agents) stay
documented, unused, in `plugins/baton/skills/baton-reference/codex-prompt-anatomy.md` and
`plugins/baton/skills/baton-reference/agy.md`.

**codex**
```
codex exec -m gpt-6-astra -c model_reasoning_effort="xhigh"
  -c model_reasoning_summary="none" -c model_verbosity="low" -c features.apps=false
  [-c web_search="disabled"]                                   # read mode only: local files
  -s read-only|workspace-write -C <worktree>
  --output-schema <schema> -o <task>/last.json --json -        # brief on stdin
codex exec resume <thread_id> ... -                            # baton fix
```
`gpt-6-astra` is the strongest model `codex debug models` lists ("Frontier intelligence for the
most demanding work", 2026-10-29); the user's interactive `~/.codex/config.toml` already runs it at
`xhigh`. The model also offers `max` and `ultra`, which baton does not use. Without
`--ignore-user-config` the worker loads that config: enabled plugins and their skills, plugin MCP
servers, and the plugin hooks the user trusted in Codex's `/hooks` (academic ones today; the
trading `desk` guard is not trusted, so the risk lint of §3 still keeps trading briefs away).
Sandbox and approval policy are baton's; web search is off in read mode and Codex's default in
edit mode. Measured on a two-line read brief (2026-10-29): 56,319 input tokens (37,248 cached),
469 output, 29 s. Non-fatal warnings on every run: the ibkr MCP server needs a login, a plugin
`hooks.json` with a Claude `modules` field is skipped, and Codex's skills budget drops every
description and 139 of the user's ~280 skills, as it does in the user's own sessions
(`-c skills.max_context_tokens=100000` would list all of them).
No `--ephemeral`: the rollout under `~/.codex/sessions/` carries live `token_count` events and
`rate_limits`, which baton tails for progress and quota.

**agy** (measured details in `plugins/baton/skills/baton-reference/agy.md`)

- Managed agy worktrees live under `$BATON_AGY_WT_DIR`, default `/tmp/baton-wt` (canonicalised, so
  `/private/tmp/baton-wt` on macOS): agy resolves its workspace to a trusted ancestor such as `~`,
  which would hide the worktree's worker agent; and outside a trusted workspace agy soft-denies even
  reads, so `baton setup` adds `read_file(<root>/)` and `write_file(<root>/)` to agy's
  `permissions.allow`. Codex worktrees stay under `$BATON_HOME/wt/`.
- Model and effort, chosen at run time: the first of `claude-opus-4-6-thinking`,
  `claude-sonnet-4-6` and `gemini-3.8-flash` that `agy models` lists (free, about 3 s), at the
  highest effort it lists for that model as `<model>-<effort>` (`gemini-3.8-flash-high`). A model
  listed bare gets no `--effort`: agy refuses one for the Claude models ("--effort is not
  supported for model"). Opus draws on the `3p-weekly` bucket and leaves agy on 2026-11-02, after
  which runs fall back to Gemini 3.8 Flash at `high`. A `--model` outside that list is used as
  given; a failed listing changes nothing.
- Worker agent: `excludeDefaultComponents: false`, `inheritCustomizations: true`,
  `subagent: false`, and a YAML tool list. It inherits the user's skills, rules, plugins (24
  synced by `baton sync --to agy`) and their hooks, and agy's default prompt sections list the
  skills (with `excludeDefaultComponents: true` the skills load but go unlisted). agy still
  declares only the listed tools plus its own `manage_task`, read from the stored prompt of real
  read runs on 2026-10-29 (`init.tools` lists all 60 tools whatever the agent). A two-line read
  brief on Opus took 13,082 input tokens (10,307 cached), 735 output, 20 s, and reading a skill
  file under `~/.gemini/config/plugins/` was allowed. Edit tasks write `baton-worker` into the
  worktree as `.agents/agents/baton-worker.md` (excluded from git via `.git/info/exclude`). Read
  tasks, in git or not, write nothing into the user's folder: they use the global
  `~/.gemini/config/agents/baton-read-worker.md`, which agy loads from any cwd.
- Edit tasks get `[view_file, list_dir, find_by_name, grep_search, write_to_file,
  replace_file_content, multi_replace_file_content, finish]`; read tasks get only `[view_file,
  grep_search, finish]`, so baton expands the brief's `## Files` into at most 200 absolute paths
  (git's file list, else a walk from each pattern's literal leading folder that prunes folders no
  pattern can reach, within 50,000 entries; a cut list says so). No `run_command`: headless agy
  soft-denies shell commands that are not allowlisted, and baton reruns the acceptance commands
  itself, so the worker never needs a shell. `finish` is required, or `--json-schema` returns no
  `structured_output`.
- Read runs pass `--add-dir <root>` and open the first message with the absolute root. Reads
  outside it are refused twice: by the prompt, and by agy's headless soft-deny of file access
  outside the workspace (measured 2026-10-29, agy 1.4.9: `view_file /etc/os-release` gave
  `denied_actions: [read_file]` and ended the turn). A user `read_file(...)` allow rule or
  `allowNonWorkspaceAccess: true` in agy's settings would lift the second guard.
- One long-lived process per task, so fix rounds hit the prompt cache (agy caches only once the
  context passes ~17k tokens; `--conversation` resume reloads everything uncached):
  ```
  agy --input-format stream-json --output-format stream-json --agent baton-worker
      --model <model> [--effort <effort>] --mode accept-edits
      --json-schema <schema> --print-timeout <secs>s  < (baton writes {"event":"user",...} lines)
  ```
  cwd = worktree, stdin always a pipe baton controls (with no credentials agy waits 60 s on a TTY).
  Close stdin to end the session.
- Soft-deny looks like success (exit 0, `status: SUCCESS`, often `response: ""`). Treat as
  `blocked` when any of: `result.denied_actions` non-empty, a `step_update` with `state: ERROR`
  and "permission check failed", the stderr line `jetski: no output produced`, or an empty
  response. Always require `structured_output` to be present.
- Timeouts: `--print-timeout`, then SIGTERM, then SIGKILL. agy ignores SIGALRM; a timed-out
  run exits 0 with partial output, so baton checks `status`.
- Pin `--model`; never `-c` (it picks the last conversation for the cwd and races between
  parallel runs); never an unknown `--mode` (it only warns and runs a default turn).
- Quota: `agy -p /usage --output-format json` is free and returns `remaining_fraction` and reset
  time per bucket: `gemini-weekly` (Gemini Models) and `3p-weekly` (Claude and GPT models). The
  Claude 4.6 and GPT-OSS models leave agy on 2026-11-02; after that only `gemini-3.8-flash`
  (low, medium, high) remains, routing stops offering the `3p` bucket (`THIRD_PARTY_CUTOFF`), and
  runs fall back through `agy models`.
- Global hooks in `~/.gemini/config/hooks.json` (for example Orca's status hooks) run on every
  worker call and add process start-up time; `baton doctor` reports them.

**claude** (when codex or agy call the lead's model)
```
claude -p --model <sonnet|opus> --effort <effort> --tools "Read,Grep,Glob,Edit,Write,Bash"
  --strict-mcp-config --disable-slash-commands --no-session-persistence
  --exclude-dynamic-system-prompt-sections --max-turns <n>
  --output-format stream-json --json-schema <schema>
```
`--bare` would be leaner but needs an API key; a subscription login cannot use it.

Environment: workers get a scrubbed environment (no `*_API_KEY`, `*_TOKEN`, `*_SECRET`,
`KEYFOB*`), `HOME`, `PATH`, `LANG`, and `BATON_TASK`.

## 6. Status file and live progress

`$BATON_HOME/tasks/<id>/status.json`, rewritten at most twice a second:

```json
{"task": "...", "agent": "codex", "model": "gpt-6-astra", "effort": "xhigh", "phase": "working",
 "commands": 9, "last": "cargo test -q", "files": 3, "tests": "12/14",
 "tokens": {"input": 80211, "cached": 70100, "output": 2100},
 "quota": "5h 14% · wk 3%", "started": 1792416557, "updated": 1792416800}
```

`usage.json` stores `{fetched_at, agents: [...]}` for 300 seconds. Each agent has `windows`
(normalized `agent`, `window`, `used`, `resets_at`, `window_secs`, `pace`) or an `error`;
Codex rollout fallback carries `stale: true`. Fetches run in parallel; one source failure does
not hide the others. Claude account files contribute quota fields only. `--refresh` bypasses
the cache. Statusline handlers only read this cache and never start quota commands.

Phases: `starting`, `working`, `verifying`, `done`, `failed`. Human-readable lines go to
`log.txt` and to stdout, so a background Bash task or `claude attach` shows them live.

## 7. Claude Code integration

- Thin subagents `agents/codex.md` and `agents/agy.md`: `model: haiku`, `tools: Bash`,
  `maxTurns: 3`. The body says: run exactly `baton run <agent> --brief <path> --task <id>`
  from the prompt, wait, and return its final JSON line unchanged.
- The plugin's `settings.json` sets `subagentStatusLine` to `baton statusline subagent`. For each
  panel row whose `agentType` is `baton:codex`, `baton:agy`, `codex` or `agy`, it finds the task
  id in `description` or `label` (`baton t-...`), otherwise matches an unclaimed task of that
  agent among the newest 20 status files, starting −5 s…+600 s from the row's `startTime`.
  It prints `{"id": <row id>, "content": "codex · xhigh · 3 files ·
  tests 12/14 · 80k tok · 5h 14%"}`. Other rows are left untouched (no output for them).
- Skill `baton`: the lead's playbook (brief template, when to delegate, routing, review loop).
- Skill `baton-reference`: the command references of all three CLIs, read on demand.
- `hooks/hooks.json`: SessionStart runs `baton doctor --quick` (a few file stats, no network,
  under 20 ms) and prints one line only when something is missing.

## 8. Routing

`baton route` scores each candidate (worker, model, effort):

    score = p_success(type, difficulty) × value(difficulty) − cost_tokens / remaining_tokens × pace

- `type`: implement, refactor, bugfix, test, docs, mechanical, review, research (from the brief's
  verbs and file kinds; the lead can pass `--type`).
- `difficulty` D1–D4 from the number of files, whether acceptance commands exist, and ambiguity
  markers; the lead can pass `--difficulty`.
- `p_success` starts from priors (benchmarks and design intent) and is updated from the ledger
  with a Beta(2,2) prior per (worker, type, difficulty): first-pass verified successes.
- `cost_tokens`: median tokens of past tasks at that difficulty, else a prior.
- `pace` = used fraction of the window ÷ elapsed fraction of the window (≥1 means burning faster
  than the window refills), per window (5 h and weekly), the worse of the two.
- Hard rules: risky briefs go to the lead; D4 or no acceptance commands go to the lead; a worker
  whose remaining quota is below the task's expected cost is skipped.
- Quota and task type pick the worker; model and effort are always that worker's best (§5),
  whatever the difficulty (user, 2026-10-29). Codex: `gpt-6-astra` at `xhigh`. agy: Claude Opus
  on the `3p-weekly` bucket until 2026-11-02, Gemini when that bucket is short or after the date,
  with the effort found at run time; a weaker agy model is skipped while a stronger one is
  eligible. Copilot keeps its own difficulty ladder.

## 9. Ledger

`$BATON_HOME/ledger.jsonl`, one line per run, fix and review: task, agent, model, effort, type,
difficulty, status, verified, rounds, tokens, seconds, quota before and after. `baton scorecard`
aggregates it.

## 10. Plugins in three hosts

- One plugin directory serves all three: `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json`
  (with `"hooks": "./hooks/codex.json"` when the Claude hooks use `modules` or `ask`) and a
  root `plugin.json` for agy.
- `baton sync` reads `~/.claude/plugins/installed_plugins.json`, and for each of the user's own
  plugins installs it into Codex (`codex plugin marketplace add` / `codex plugin add`) and into agy
  (`agy plugin install <dir>`), generating the missing manifest and converting hooks:
  `modules` are dropped (Claude Code only), `ask` decisions become `deny` under Codex, and the
  hook command is wrapped in `baton hook-adapt --from <host>` so a Claude-format hook script sees
  Claude-format input under any host. Trading plugins are mirrored to agy only after their guard
  passes the adapter's tests.

## 11. State

`BATON_HOME` defaults to `~/.local/share/baton`: `tasks/<id>/` (brief.md, status.json, log.txt,
events.jsonl, result.json), `wt/<id>/` worktrees, `usage.json`, `ledger.jsonl`.

## Copilot read worker

`baton run copilot --brief FILE --mode read` adds a third worker through `worker::Worker`.
Omitting read mode is a usage error (`copilot runs read briefs only`), and Copilot cannot review
managed edit tasks. The default model is `auto`, which takes no effort flag; `--model`, `BATON_COPILOT_MODEL` and
`BATON_COPILOT` provide model/binary overrides. Efforts are low, medium, high and xhigh for an explicit model.

The driver reuses Codex's scrubbed environment, process-group cleanup and namespace probe without
changing Codex or agy invocations. It additionally retains Copilot home and GitHub authentication
environment variables and forces `COPILOT_AUTO_UPDATE=false`. A successful user/mount namespace
probe is required and cached once per process. The wrapper mounts a private tmpfs `/tmp`, binds
`meta.worktree` (the git root, else the current folder, as in Codex's read mode), remounts that bind read-only, then execs Copilot. Probe or
mount failure never falls back to a writable run. `BATON_COPILOT_NO_NS=1` is a test-only hook
for fake executables, never set by real runs. The complete recipe and measured CLI behavior are
in `plugins/baton/skills/baton-reference/copilot.md`.

The brief and fixed read-only/result instructions go on stdin. JSONL assistant/tool events drive
progress. The last JSON object in the final assistant message gives status, notes and questions;
otherwise the whole message is notes and the result event's exit code determines success. The
result always has an empty `files_changed`; normal read-mode acceptance checks still run, with
no automatic fix rounds. Premium request usage from the result event is serialized as
`tokens.premium_requests` in status, results and ledger; checkpoints are cumulative, not additive.

Usage reads `gh api /copilot_internal/user` and normalizes premium interactions to a monthly
window, also retaining entitlement, remaining requests, percent remaining and reset date. It uses
the existing five-minute cache and source error handling. Runs refuse below a default floor of
20 requests (`BATON_COPILOT_FLOOR` overrides), protecting an account that permits billed overage.
`baton route --mode read` may add Copilot for difficulty 1–2 only, strictly above that floor;
default edit routing retains its candidates and scores. Copilot's routing cost is one premium
request rather than an estimated token count. `baton doctor` includes installation and version.
