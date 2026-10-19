# Codex CLI for baton (codex-cli 0.163.0, model gpt-6.1-sol)

Raw help of every command and subcommand: `help/codex/` (77 files). Prompt composition measured
from captured request bodies: `codex-prompt-anatomy.md`. Sources: learn.chatgpt.com/docs (CLI,
non-interactive mode, config reference, hooks, build-plugins, app-server), local experiments.

## Command map

| Command | Use for baton |
|---|---|
| `codex exec [PROMPT\|-]` | headless run; prompt from stdin with `-` |
| `codex exec resume <thread_id> [-]` | continue a thread with the prompt cache warm (fix rounds) |
| `codex exec fork <thread_id>` | branch a thread |
| `codex exec review` / `codex review [--uncommitted\|--base B\|--commit SHA] [--title T]` | built-in review of a diff |
| `codex app-server` | JSON-RPC over stdio (newline-delimited, `jsonrpc` field omitted): threads, turns, streaming events, approvals, account data |
| `codex app-server generate-json-schema \| generate-ts` | protocol types for this exact build |
| `codex remote-control start\|stop\|pair` | app-server daemon reachable from the ChatGPT app (experimental) |
| `codex agents` | browse sessions on the shared app-server daemon |
| `codex queue` | queue a message for an existing session |
| `codex cloud list\|status\|diff\|apply\|exec` | Codex Cloud tasks |
| `codex plugin add\|list\|remove`, `codex plugin marketplace add\|list\|remove\|upgrade` | plugins |
| `codex mcp add\|get\|list\|remove\|login\|logout` | MCP servers |
| `codex features list\|enable\|disable` | feature flags |
| `codex login [--device-auth\|--with-api-key\|--with-access-token]`, `codex login status` | auth; device code works over SSH |
| `codex doctor`, `codex debug models\|prompt-input\|app-server` | diagnostics; `debug prompt-input` prints what a prompt would send |
| `codex sandbox`, `codex apply`, `codex resume\|fork\|archive\|delete` | sandboxed commands, apply a diff, session management |

## `codex exec` flags that matter

`-m MODEL`, `-c key=value` (TOML), `--enable/--disable FEATURE`, `-p PROFILE` (layers
`$CODEX_HOME/<name>.config.toml` on the user config), `--ignore-user-config` (skip
`config.toml`; auth still from `CODEX_HOME`), `--ignore-rules`, `-s read-only|workspace-write|danger-full-access`,
`--approve-for-me` (route approvals through automatic review in workspace-write), `-C DIR`,
`--add-dir DIR`, `--worktree` (managed git worktree), `--skip-git-repo-check`, `--ephemeral`
(no rollout file), `--output-schema FILE` (final message constrained to a JSON Schema),
`-o FILE` (final message to a file), `--json` (JSONL events on stdout), `-i FILE` (images),
`--strict-config`. `--full-auto` was removed: use `-s workspace-write`.

## Headless recipes

```sh
# edit in a worktree, structured result, lean prompt
codex exec --ignore-user-config -m gpt-6.1-sol -c model_reasoning_effort="medium" \
  -c model_reasoning_summary="none" -c model_verbosity="low" -c web_search="disabled" \
  -c features.apps=false -s workspace-write -C "$WT" \
  --output-schema schema.json -o last.json --json - < brief.md > events.jsonl

# fix round on the same thread (prefix cached)
codex exec resume "$THREAD" --ignore-user-config ... --json - < fixes.md

# read-only analysis
codex exec --ignore-user-config ... -s read-only -C "$REPO" --output-schema schema.json -o last.json -
```

JSONL events (`--json`): `thread.started {thread_id}`, `turn.started`, `item.started` and
`item.completed` with `item.type` in `agent_message`, `reasoning`, `command_execution`
(`command`, `aggregated_output`, `exit_code`, `status`), `file_change`, `mcp_tool_call`,
`web_search`, `todo_list`, `error`; `turn.completed {usage: input_tokens, cached_input_tokens,
cache_write_input_tokens, output_tokens, reasoning_output_tokens}`; `turn.failed {error}`.
Usage arrives only at the end of a turn; for live tokens tail the rollout file (below).
Commands run as `/bin/zsh -lc "<cmd>"` (the user's shell).

Measured: a real 6-file change (generalising a hook script, 16 commands) used 190,369 input
tokens (156,288 cached), 5,927 output, at effort medium.

## Prompt size levers (see `codex-prompt-anatomy.md`)

- `--ignore-user-config`: drops the user's plugins' skills list (44 KB with 37 plugins) and hooks.
  One-line call: uncached input 14,292 → 2,459 tokens.
- `model_instructions_file` replaces the 22 KB base instructions; they are in the cached prefix,
  so the saving is in cache reads, and the default text carries tool and testing rules.
- `features.apps=false` trims 1 KB; `features.multi_agent=false` did not change the tool payload.
- `skills.max_context_tokens`, `plugins.<id>.enabled`, `mcp_servers.<id>.enabled`,
  `mcp_servers.<id>.enabled_tools`, `project_doc_max_bytes` for interactive profiles.
- Output side: `model_reasoning_summary="none"`, `model_verbosity="low"`, lower effort.

## Quota without spending tokens

```text
codex app-server   (stdio)
-> {"id":1,"method":"initialize","params":{"clientInfo":{"name":"baton","version":"0.1.0"}}}
-> {"method":"initialized"}
-> {"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}}
<- {"id":2,"result":{"rateLimits":{"limitId":"codex","planType":"plus",
      "primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":1792432873},
      "secondary":{"usedPercent":3,"windowDurationMins":10080,"resetsAt":1792957281},
      "rateLimitReachedType":null},"rateLimitsByLimitId":{"codex":{...}},"ordinaryUsageAllowed":true}}
```

Answers in about 0.4 s. `account/rateLimits/updated` notifications follow during turns. Offline
fallback: the newest `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` carries `token_count` events
with `rate_limits` (`primary`/`secondary` with `used_percent`, `window_minutes`, `resets_at`).

## Plugins and hooks

- Manifest `.codex-plugin/plugin.json` (or root `plugin.json` under `extensions.com.openai`):
  `skills`, `hooks` (path, array of paths, or inline; an explicit value replaces discovery of
  `hooks/hooks.json`), MCP, `interface`. Codex catalog: `.agents/plugins/marketplace.json`
  with entries `{name, source: {source: local|url|git-subdir, ...}, policy, category}`.
- `hooks.json` top level allows only `description` and `hooks`: a Claude Code `modules` field makes
  the whole file fail to parse.
- Hook env: `PLUGIN_ROOT`, `PLUGIN_DATA`, and for compatibility `CLAUDE_PLUGIN_ROOT`,
  `CLAUDE_PLUGIN_DATA`. Commands run with the session cwd. Default timeout 600 s.
- stdin: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, `turn_id`,
  `permission_mode`; tool events add `tool_name` (`Bash` for shell, `apply_patch` also matches
  `Edit`/`Write`, `mcp__server__tool`), `tool_use_id`, `tool_input` (`command` string for Bash).
- PreToolUse output: `hookSpecificOutput.permissionDecision` `deny` (or `decision: "block"`, or exit
  2) and `allow` with `updatedInput`. **`ask` is not supported: Codex marks the hook failed and
  runs the tool.** Guards must deny under Codex.
- Non-managed hooks run only after the user trusts them in `/hooks`; a changed hook needs trust
  again. `--dangerously-bypass-hook-trust` skips that for one run.
- Events: SessionStart, SessionEnd, PreToolUse, PermissionRequest, PostToolUse, PreCompact,
  PostCompact, UserPromptSubmit, SubagentStart, SubagentStop, Stop, Interrupt.

## Image generation

`features.image_generation` is on by default. Ask for an image in an `exec` prompt and name the
output path; the default (auto) size came out 1254 × 1254. Images are saved into the workspace.

## Pitfalls

- `--ignore-user-config` also drops the user's guard hooks; keep risky work away from workers.
- `--ephemeral` removes the rollout file, and with it live token and quota events.
- In `exec`, the sandbox blocks writes outside `-C` and `--add-dir` (Python's default bytecode cache
  location included: set `PYTHONPYCACHEPREFIX` inside the worktree).
- `codex mcp-server` no longer exists (removed after 0.153); use `exec` or `app-server`.
