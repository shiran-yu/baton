---
name: baton-reference
description: Command maps, headless recipes, lean flags, quota reading and measured token costs for the Claude Code, Codex, Antigravity (agy) and GitHub Copilot CLIs. Read the one file you need when driving one of these CLIs from another, writing a baton driver, or checking a flag.
---

# CLI reference for baton

Read only the file for the CLI and topic at hand.

| File | What it holds |
|---|---|
| `codex.md` | Codex CLI 0.163: every command and flag, `codex exec` headless recipe, JSONL events, config keys that change prompt size, rate limits (rollout `rate_limits`, app-server `account/rateLimits/read`), plugins, hooks (no `ask`, `PLUGIN_ROOT`), image generation |
| `codex-prompt-anatomy.md` | What one Codex call sends, component by component, from captured request bodies |
| `agy.md` | Antigravity CLI: commands, headless `-p` recipe, `stream-json` events, soft-deny behaviour, custom agents, permissions syntax, plugins, hooks, `/usage` quota buckets |
| `copilot.md` | Copilot CLI 1.0.103: read-only headless flags, JSONL events and premium requests, namespace enforcement, quota floor and measured account/model limits |
| `claude-code.md` | Claude Code CLI: headless `-p` flags, `--bg` jobs and agent view, subagents and `subagentStatusLine`, statusline `rate_limits`, plugin mechanics |
| `help/<cli>/<command>.txt` | Raw `--help` of every command, recaptured by `baton doctor --scan` when a CLI's version changes |
