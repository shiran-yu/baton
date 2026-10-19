## Goal

Add `baton sync` and `baton hook-adapt` (spec: `DESIGN.md` §10; agy facts:
`plugins/baton/skills/baton-reference/agy.md` §5; Codex facts: `codex.md` "Plugins and hooks").

1. `baton sync [--dry-run] [--to codex,agy] [--only NAME[,NAME]] [--include-trading]`
   - Read `$BATON_CLAUDE_PLUGINS` (default `~/.claude/plugins`) `installed_plugins.json`
     (`plugins` map of `name@marketplace` → list of `{installPath, version, ...}`; use the first
     entry) and `known_marketplaces.json` (`installLocation`, `source`).
   - Own plugins only by default: marketplaces named in `$BATON_HOME/sync.json`
     (`{"own_marketplaces": [...], "exclude": [...]}`), default
     `["my-develop-plugins", "yushiran-research", "trading-plugins", "baton"]`. Plugins from the
     `trading-plugins` marketplace are skipped for agy unless `--include-trading` (their guards are
     only proven under Claude Code and Codex).
   - **codex**: for each own marketplace whose `installLocation` contains
     `.agents/plugins/marketplace.json`, ensure the Codex marketplace exists (`codex plugin
     marketplace list` output parsed; else `codex plugin marketplace add <installLocation>`), then
     for each installed Claude plugin that the Codex catalog lists and `codex plugin list --json`
     does not show as installed, run `codex plugin add <name>@<codex marketplace name>`. Report
     plugins without a Codex entry as `no codex manifest` (do not generate one).
   - **agy**: for each installed Claude plugin, build a native agy plugin under
     `$BATON_HOME/agy-plugins/<name>/` (rebuild when the Claude version changed; record it in
     `.baton-sync.json` there):
     - root `plugin.json` with `name`, `description`, `version` only;
     - `skills/` copied as is; `commands/*.md` → `skills/<name>-<cmd>/SKILL.md` with a `name:` line
       added to the frontmatter;
     - `agents/*.md`: rewrite frontmatter `tools` from the Claude comma list to a YAML list of agy
       names (Read→view_file, Write→write_to_file, Edit→replace_file_content and
       multi_replace_file_content, Grep→grep_search, Glob→find_by_name, LS→list_dir,
       Bash→run_command, WebFetch→read_url_content, WebSearch→search_web, Task→invoke_subagent;
       unknown names dropped), `model` sonnet/opus/haiku/inherit → `inherit`, drop keys agy lacks
       (`maxTurns`, `color`, `effort`, `isolation`, `background`, `disallowedTools`);
     - `.mcp.json` → `mcp_config.json` (same `mcpServers` map; a remote `url` becomes `serverUrl`;
       `${CLAUDE_PLUGIN_ROOT}` replaced by the absolute install path);
     - hooks: read the Claude `hooks/hooks.json` (ignore `modules`); map events SessionStart →
       SessionStart, UserPromptSubmit → PreInvocation, PreToolUse → PreToolUse, PostToolUse →
       PostToolUse, Stop → Stop (drop others); map tool matchers with the same table (`Bash` →
       `run_command`, `Edit|Write` → `write_to_file|replace_file_content|multi_replace_file_content`,
       `*`/empty unchanged); write root `hooks.json` as
       `{"<plugin>": {"<Event>": [...]}}` with tool events as matcher groups and other events as flat
       handler lists; every command becomes
       `<abs path of baton> hook-adapt --from agy --event <ClaudeEvent> -- <original command with
       ${CLAUDE_PLUGIN_ROOT} and $CLAUDE_PLUGIN_ROOT replaced by the absolute install path>`;
     - `bin/` copied (agy puts a plugin's `bin/` on the agent PATH);
     - then `agy plugin validate --json <dir>` (report problems) and, unless `--dry-run`,
       `agy plugin install <dir>`.
   - Output one line per plugin and host: `installed`, `up to date`, `skipped (<reason>)`, or the
     error; `--dry-run` prints the commands and writes nothing outside `$BATON_HOME`.
2. `baton hook-adapt --from <agy|codex> --event <ClaudeEvent> -- CMD [ARGS...]`
   - Read the host's hook JSON on stdin, convert it to Claude Code's hook input, run CMD with that
     on stdin (inherit the environment, set `CLAUDE_PLUGIN_ROOT` to the plugin path when known
     from the command), and convert CMD's stdout back to the host's contract. Never fail open for
     PreToolUse: if CMD crashes or prints invalid JSON, answer `deny` with a reason.
   - **agy → Claude input**: `conversationId` → `session_id`, `workspacePaths[0]` → `cwd`,
     `transcriptPath` → `transcript_path`, `hook_event_name` = the Claude event; for tool events
     `toolCall.name` mapped back (run_command → Bash, write_to_file → Write,
     replace_file_content/multi_replace_file_content → Edit, view_file → Read, grep_search → Grep,
     find_by_name → Glob, list_dir → LS) as `tool_name`, and `toolCall.args` as `tool_input`, with
     `command` set from the first string among args `CommandLine`, `command`, `Command`, `cmd`;
     for PreInvocation (UserPromptSubmit) run CMD only when `invocationNum` is 0, with `prompt` set
     to "" (agy does not pass the prompt).
   - **Claude output → agy**: PreToolUse `hookSpecificOutput.permissionDecision` allow → `{"decision":"allow"}`,
     deny → `{"decision":"deny","reason":...}`, ask → `{"decision":"deny","reason":...}` (headless agy
     cannot prompt; never auto-approve), exit code 2 → deny with stderr as reason;
     `additionalContext` (UserPromptSubmit/SessionStart) or plain stdout →
     `{"injectSteps":[{"ephemeralMessage":"<text>"}]}`; Stop `decision: block` →
     `{"decision":"continue","reason":...}`; PostToolUse → `{}`.
   - **codex**: input is already Claude-shaped; pass it through, and on output turn `ask` into
     `deny` (Codex runs the tool on `ask`), exit 2 stays exit 2.

## Files

- `src/**`
- `tests/**`

## Forbidden

- New dependencies; `unsafe`; network; running the real CLIs in tests (fake `codex`/`agy` scripts on
  `BATON_CODEX`/`BATON_AGY`, temp dirs for `BATON_HOME` and `BATON_CLAUDE_PLUGINS`); editing outside Files.

## Acceptance

```
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests: sync dry run over a fixture `installed_plugins.json` with three plugins (one with skills,
agents, hooks and .mcp.json; one trading; one with a Codex catalog entry); the generated agy plugin's
files (agent tools list, hooks.json shape and adapted commands, mcp_config.json); codex commands
issued to a fake codex; hook-adapt for agy PreToolUse allow/deny/ask/crash, agy PreInvocation with
invocationNum 0 and 1, codex ask → deny, and a Claude guard script fixture that denies `rm -rf`.

## Context

- Return the final message in the given JSON schema.
