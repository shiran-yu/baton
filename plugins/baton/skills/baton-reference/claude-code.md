# Claude Code CLI for baton (2.1.303)

Raw help of every command: `help/claude/`. Sources: code.claude.com/docs (headless, cli-reference,
sub-agents, statusline, agent-view, cross-session-messaging, plugins/*, hooks, prompt-caching).

## Headless calls (codex or agy calling the lead's model)

```
claude -p "<prompt>" --model sonnet|opus|haiku --effort low|medium|high|xhigh|max
  --tools "Read,Grep,Glob"            # exact tool set; "" for none
  --strict-mcp-config --mcp-config <file>   # no user MCP servers
  --disable-slash-commands --no-session-persistence
  --exclude-dynamic-system-prompt-sections  # per-user context out of the cacheable prefix
  --max-turns N --output-format json|stream-json --json-schema '<schema>'   # result in structured_output
```

- `--bare` skips hooks, skills, plugins, MCP, auto memory and CLAUDE.md: the leanest start. It
  reads no OAuth or keychain, so it needs `ANTHROPIC_API_KEY`; a subscription login cannot use it.
- `--system-prompt[-file]` replaces the default prompt and drops its tool and safety guidance;
  `--append-system-prompt[-file]` keeps it.
- `--resume` / `--continue` reuse the prompt cache while the prefix is unchanged and within the TTL
  (5 minutes for `-p` by default; `CLAUDE_CODE_PROMPT_CACHE_TTL=1h` for API keys).
- `--output-format json` reports `usage` and `total_cost_usd`.

## Background jobs and agent view

- `claude --bg [--name N] --exec '<cmd>'` runs a command as a PTY job with no model call. It shows
  in `claude agents` (agent view); `claude attach <id>` watches it live, `claude logs <id>` shows
  recent output (kept in memory, gone about 5 minutes after exit), `claude stop|rm <id>`.
  The working directory must be a trusted workspace.
- `claude --bg` without `--exec` starts a background model session.
- `claude agents --json [--all]` lists interactive and background sessions.

## Subagents and the agent panel

- Plugin subagents load from `agents/*.md` as `<plugin>:<name>`. Frontmatter: `name`, `description`
  (required), `tools`, `disallowedTools`, `model` (sonnet/opus/haiku/fable, a full id, or inherit),
  `maxTurns`, `effort`, `background`, `isolation`, `color`. Plugin subagents ignore `hooks`,
  `mcpServers`, `permissionMode`, `initialPrompt`.
- Running subagents appear as rows below the prompt; Enter opens a transcript, `/tasks` lists
  them with background shells.
- The Bash tool's foreground timeout is at most 600,000 ms, so a relay subagent waits in slices
  (`baton wait --max 570`).
- `subagentStatusLine` (settable from a plugin's `settings.json`): a command run on each refresh
  tick with JSON on stdin: base hook fields, `columns`, and `tasks[]` with `id`, `name`, `type`,
  `agentType`, `status`, `description`, `label`, `startTime`, `model`, `effort`,
  `contextWindowSize`, `tokenCount`, `tokenSamples`, `cwd`. Print one
  `{"id": "<task id>", "content": "<row text>"}` line per row to override it, an empty `content`
  to hide it, nothing to leave it. It cannot add rows.

## Status line

stdin JSON includes `rate_limits.five_hour.used_percentage`, `rate_limits.five_hour.resets_at`,
`rate_limits.seven_day.used_percentage`, `rate_limits.seven_day.resets_at` (Unix seconds), for
Pro/Max after the first API response; either window may be missing. Also
`context_window.used_percentage`, `session_id`, `agent.name`. `refreshInterval` adds a timer.

## Plugin mechanics

- `bin/` of an enabled plugin is on the Bash tool's PATH (not on hook or status line PATHs).
- `${CLAUDE_PLUGIN_ROOT}` (changes on update), `${CLAUDE_PLUGIN_DATA}` (survives updates),
  `${CLAUDE_PROJECT_DIR}` in hooks; not in Bash-tool commands.
- `dependencies` in plugin.json or a marketplace entry: `"name"` or
  `{"name", "marketplace", "version"}`; a cross-marketplace dependency needs
  `allowCrossMarketplaceDependenciesOn: [...]` in the root marketplace. Uninstall keeps
  auto-installed dependencies until `claude plugin prune`.
- Same plugin name from two marketplaces: only the first-loaded copy registers its hooks.
- SessionStart hooks run on every start, resume, clear, compact and fork: keep them fast. One-time
  installs belong in the Setup hook (`claude --init`).
- `experimental.monitors` in the manifest: persistent background processes shown in the task panel.
