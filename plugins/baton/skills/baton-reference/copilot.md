# GitHub Copilot CLI

Measured 2026-10-24 with Copilot 1.0.103, plan `individual`, SKU `free_educational_quota`.
Only `--model auto` was accepted on this account; it routes to `gpt-5.6-luna`, the one model the
plan may use (`availableModels` in the `session.auto_mode_resolved` event). Every explicit model,
`gpt-5.6-luna` included, returned `Error: Model "<id>" from --model flag is not available.` before
making a request, and `auto` refuses `--reasoning-effort`.
One `-p` run costs one premium request regardless of the number of tool calls, from 200 per month.
These are measurements for this account and version, not promises about other plans.

## Headless invocation

```sh
baton run copilot --brief /path/to/folder/brief.md --mode read --effort medium --json
baton route --brief /path/to/folder/brief.md --mode read --difficulty 1 --json
```

The driver sends the brief plus a fixed read-only/result-JSON tail on stdin. Copilot accepts stdin
as the prompt when `-p` is absent; `-p '<prompt>'` is the explicit prompt alternative.

```sh
copilot --model auto \
  --allow-all-tools --no-ask-user --no-custom-instructions --disable-builtin-mcps \
  --output-format json --usage-output-file '<task dir>/usage.json'
```

`BATON_COPILOT` overrides the binary (relative paths resolve before changing cwd), and
`BATON_COPILOT_MODEL` or `--model` overrides the default model. Baton accepts efforts `low`,
`medium`, `high`, `xhigh`; the measured CLI additionally accepts `none`, `minimal`, and `max`.
The effort is passed only with an explicit model. The cwd is the folder Codex's read mode uses
(the git root, else the current folder), stored as `meta.worktree`. There is no edit mode or
`baton review --by copilot`, and no automatic retry after failed acceptance.

Workers receive the `codex::scrub` environment plus `COPILOT_HOME`, `GH_TOKEN`, `GITHUB_TOKEN`,
and `COPILOT_GITHUB_TOKEN` when present. `COPILOT_AUTO_UPDATE=false` is always set.

## Read-only enforcement

Baton probes `unshare -rm sh -c 'mount -t tmpfs tmpfs /tmp'` once per process. If unavailable,
it refuses to launch Copilot. On success it wraps the command as:

```sh
unshare -rm sh -c 'mount -t tmpfs tmpfs /tmp && mount --bind "$1" "$1" && mount -o remount,bind,ro "$1" && shift && exec "$@"' sh '<folder>' copilot ...
```

The folder is mounted read-only and `/tmp` is private; Copilot's home remains writable. This
recipe was verified on login44: reads succeed and `touch` in the folder returns
`Read-only file system`. Paths under the old `/tmp` are hidden by the private tmpfs, so keep
real briefs, task state and executables outside `/tmp`.
`BATON_COPILOT_NO_NS=1` skips the namespace **only for tests with fake workers**. Normal runs
and the relay agent never set it.

## Events and results

Stdout is JSONL, one event per line. Baton saves it in `events.jsonl` and uses
`assistant.turn_start`, `tool.execution_start`, `tool.execution_complete`, and `assistant.message`
for progress. The final `assistant.message.data.content` supplies the answer:

```json
{"type":"assistant.message","data":{"model":"gpt-5.6-luna","content":"OK","toolRequests":[]}}
{"type":"session.usage_checkpoint","data":{"totalNanoAiu":327519000,"totalPremiumRequests":1}}
{"type":"result","sessionId":"872c...","exitCode":0,"usage":{"premiumRequests":1,"totalApiDurationMs":1167,"sessionDurationMs":3325,"codeChanges":{"linesAdded":0,"linesRemoved":0,"filesModified":[]}}}
```

The last JSON object in the final message supplies `status` (`done`, `blocked`, `needs_decision`),
`notes`, and `questions`. Without a parseable result object, the whole message becomes `notes`;
`result.exitCode == 0` means done, otherwise failed. `files_changed` is always empty. Baton runs
only the brief's normal read-mode acceptance checks. `result.usage.premiumRequests` is recorded
as `tokens.premium_requests` in the result and ledger, replacing the provisional checkpoint count.

Other measured events: `assistant.turn_end`, `assistant.message_start`, `assistant.message_delta`,
`assistant.idle`, `model.call_start`, `model.call_finished`, `model.call_final_result`, `user.message`,
`session.tools_updated`, `session.auto_mode_resolved`, `session.mcp_server_status_changed`.
An unavailable model prints its error on stderr and emits no result event.

## Quota

`baton usage` reads `gh api /copilot_internal/user`, with the same five-minute cache and error
isolation as other sources. The row exposes
`quota_snapshots.premium_interactions.{entitlement,remaining,percent_remaining}` and
`quota_reset_date`; for the measured fixture these are 200, 124, 62.3, and `2026-11-01`.
A missing/failing `gh` produces an error row. A run refuses with fewer than 20 remaining
premium requests (`BATON_COPILOT_FLOOR` overrides the floor), because this account permits billed
overage. Routing considers Copilot only for read briefs of difficulty 1–2, strictly above the
floor; edit routing is unchanged. `baton doctor` reports Copilot installation/version.
