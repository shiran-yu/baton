# agy (Google Antigravity CLI) reference for baton

Target: `agy` 1.4.2 (`~/.local/bin/agy`, Mach-O arm64 Go binary, 186 MB). Researched 2026-10-19.
Evidence labels: **[M]** measured locally today, **[D]** official docs (antigravity.google/docs/*.md), **[C]** `agy changelog`, **[B]** strings in the binary, **[I]** my inference, which needs verifying before baton depends on it.
Raw material: `baton-research/agy-help/` (help dumps, changelog), `baton-research/docs-md/` (49 doc pages as Markdown; every page has a `.md` twin, e.g. `https://antigravity.google/docs/cli/headless.md`, and the index is `https://antigravity.google/llms.txt`), `baton-research/exp/` (experiment outputs).

---

## 1. Command map

### 1.1 Top-level flags (`agy --help`) [M]
| Flag | Notes |
|---|---|
| `-p` / `--print` / `--prompt "<text>"` | Headless single run. A valueless `-p` followed by another flag is now an error [C]. |
| `-i` / `--prompt-interactive "<text>"` | Starts the TUI with an initial prompt. Combining it with `-p` gives exit 2 [C]. |
| `--output-format text\|json\|stream-json` | Print mode only. |
| `--input-format text\|stream-json` | `stream-json` reads NDJSON prompts from stdin, one turn per line. It **requires** `--output-format stream-json`, and any `-p` prompt is dropped [D]. |
| `--json-schema '<json>'\|path.json` | The root must be `"type":"object"`. Inline JSON or a file path both work. `string` or other bare type names give exit 1 [M] (the docs still claim primitives work, which is stale). |
| `--print-timeout <dur>` | Go duration (`90s`, `15m`). The help says `default 0s` = unlimited (the docs still say 5m, which is stale [C]). When it expires, the CLI returns the partial output, **exits 0** and prints a warning on stderr [C]. |
| `--model <slug>` | Unknown slug in `-p`: exit 1 and a list of models [D/C]. A base name without effort is accepted (`gemini-3.8-flash`) [M]. |
| `--effort low\|medium\|high\|xhigh\|max` | Per invocation; not persisted. The levels available depend on the model (this account: low/medium/high for Gemini 3.8 Flash) [M]. The Claude and GPT-OSS models take none: `--effort high` with `claude-opus-4-6-thinking` exits 1 with `--effort is not supported for model` [M, 2026-10-29]. |
| `--agent <name>` | Custom agent. Unknown name in `-p`: exit 1 with `error: unknown agent "x" (available: …)` [M]. |
| `--mode accept-edits\|auto\|plan` | **An unknown value only warns and runs in default mode** [M]. `auto` = "not available for this account" warning, then default mode [M]. |
| `--sandbox` | OS sandbox for terminal commands (Seatbelt on macOS). No network; only the workspace and temp are writable [D]. |
| `--dangerously-skip-permissions` | `permission_mode: always-proceed`. Since 1.3.2 it no longer auto-approves plans [C]. |
| `--continue`/`-c`, `--conversation <id>` | Resume (see 2.6). |
| `--add-dir <dir>` (repeatable) | Extra workspace roots, where reads and writes are auto-allowed. |
| `--project <id\|name>`, `--new-project` | Project scoping. The default is `default-cli-project` (`~/.gemini/config/projects/default-cli-project.json`). |
| `--disable-slash-commands` | Turns off `/x` and skill expansion in print mode. |
| `--log-file <path>` | Overrides the CLI log (default `~/.gemini/antigravity-cli/log/cli-*.log`, symlink `cli.log`). |
| `--remote-control` | Opens a Remote Control tunnel for an **interactive** session (section 6.3). |

### 1.2 Subcommands [M]
| Command | Behaviour |
|---|---|
| `agy models` | TSV `slug\tLabel`. Progress goes to stderr. Free, about 3 s. This account: `gemini-3.8-flash-{high,medium,low}`, `claude-sonnet-4-6`, `claude-opus-4-6-thinking`, `gpt-oss-120b-medium` (same on 2026-10-29, agy 1.4.10). The Claude 4.6 models and GPT-OSS are **removed on 2026-11-02** [D]. baton reads it before every run to pick the model (2.1). |
| `agy agents` / `agy agent` | **Printed nothing** in a pipe, even with a workspace agent present, and rejects `--output-format` (contrary to the changelog). Use `agy -p /agents --output-format json` instead. |
| `agy mcp add [--type stdio\|http] [-e K=V]… [-H "K: V"]… <name> <cmdOrUrl> [args…]` | Flags must come **before** `<name>`; use `--` before args that start with `-`. Also `remove/list/enable/disable <name>`. Writes **global** `~/.gemini/config/mcp_config.json`. |
| `agy plugin list [--json] \| import [gemini\|claude] \| install <target> \| uninstall <name> \| enable/disable <name> \| validate [path] [--strict] [--json] \| link <mp> <target>` | Alias: `plugins`. Subcommands **do not parse `--help`**: `agy plugin uninstall --help` printed `Uninstalled plugin "--help"`. The install target is a dir, a GitHub URL or `name[@antigravity-plugins-official]`. |
| `agy remote-control start [--name N] [--session] \| status \| stop` | Daemon (LaunchAgent on macOS). Hidden `serve` = daemon entrypoint. Today it said `not signed in` even though `-p` works. `status`: not running. |
| `agy install [--dir D] [--skip-aliases] [--skip-path]` | Configures PATH and shell profiles. |
| `agy update` | Self-update (no flags). Disable auto-update with `AGY_CLI_DISABLE_AUTO_UPDATE=true` [D]. |
| `agy changelog` | Full release notes, 941 lines; also embedded in the binary. |
| `agy mic-serve [--addr 127.0.0.1:4713]` | Forwards a local mic to a remote CLI for `/voice`. |

### 1.3 Print-mode slash commands (no agent turn, **no quota**, no conversation) [M][C]
`agy -p "/usage"` (alias `/quota`), `/credits`, `/model`, `/effort`, `/skills`, `/agents`, `/hooks`, `/permissions`, `/config`, `/help`, `/changelog`. Text output = one TSV record per line. `--output-format json` adds `command:{name,data}` with `conversation_id:""`, `num_turns:0` and zero usage. Each takes about 1–2 s. Other interactive-only slash commands fail with the flag that replaces them. Inside `--input-format stream-json` sessions, CLI-answered slash commands end the session (ERROR, exit 2). Skill slash commands (`-p "/my-skill args"`) do expand in print mode.

Useful payloads [M]:
- `/model`: `{"id":"gemini-3.8-flash-high","label":"Gemini 3.8 Flash (High)","effort":"high","is_default":false}`
- `/effort`: `{"adjustable":true,"current":"high","available":["low","medium","high"]}`; with `--model claude-opus-4-6-thinking` (or `claude-sonnet-4-6`, `gpt-oss-120b-medium`): `{"adjustable":false}`
- `/agents`: `{"agents":["lean"]}` (the cwd's workspace agents plus global ones)
- `/hooks`: `{"hooks":[{name,enabled,source,actions:[{event,matcher,type,command,timeout_seconds}]}]}`. Use it at baton start-up to detect foreign global hooks (section 7).
- `/permissions`: scopes `project`, `shared` (`~/.gemini/config/config.json`), `global` (`~/.gemini/antigravity-cli/settings.json`).
- `/config`: the effective settings (keys listed in 2.7).

### 1.4 Recent capability changes (changelog digest, newest first) [C]
- **1.4.2** Remote Control history in daemon mode fixed. **1.4.1** `plugin validate` also checks skills, agents and rules (24 KB limit, name and folder match, reserved names, `agent.json` ignored). Socket drops are retried instead of killing the turn. Crashed MCP servers are marked `Error`. Large MCP JSON outputs now count against the per-step token limit.
- **1.4.0** `plugin validate` uses the real loader and gains `--strict`/`--json`. Header variables like `${TOKEN}` are substituted for plugin MCP servers. OSC 7501 tab status (`AGY_CLI_OSC7501`).
- **1.3.6** Claude Opus/Sonnet 5.5 (ADC sign-in only here). A stalled model stream is cancelled and retried after 5 min of no data. Subagent questions are skipped in headless. `GIT_EDITOR`, `HGEDITOR` and `JJ_EDITOR` are set to `false` for agent commands.
- **1.3.5** `--mode auto` (edits and safe commands auto-approved; leaving the sandbox goes through a reviewer agent; Linux and macOS only, and **not enabled on this account**). `/compact`.
- **1.3.4** `plugin list --json`. `--model` takes a base name and the effort is defaulted. Unknown `--agent` in `-p` gives exit 1.
- **1.3.3** MCP tools can set `_meta:{"antigravity/alwaysLoad":true}`. Plugin root `agents.json`. The macOS sandbox blocks `~/Library/Application Support`. Quota exhaustion is reported immediately instead of being retried.
- **1.3.2** `/plugin` marketplace (`antigravity-plugins-official`). Manifests can live in `.antigravity-plugin/` or `.gemini-plugin/`. Plugins get `PLUGIN_DATA` and a `bin/` folder on PATH.
- **1.2.x** Headless gained JSON and stream-json output, `--json-schema`, `--input-format stream-json`, soft-deny with `denied_actions`, `AGY_ERROR` and exit 3, an unlimited default print timeout, and quota-free print slash commands. Agents gained `excludeDefaultComponents`, `inheritCustomizations`, `rules:`, `agents:` and `model:` tiers. Rules got a 20k-token budget. The default toolset dropped `find_by_name`, `grep_search` and `list_dir`, which custom agents can still list.

---

## 2. Headless driving recipe for baton

### 2.1 Invariants for every run
```sh
cd "$WORKTREE" && agy -p "$PROMPT" \
  --agent "$AGENT" --model gemini-4-argon --effort high \
  --output-format stream-json --print-timeout 15m </dev/null 2>"$RUN/stderr" >"$RUN/events.ndjson"
```
- **Model and effort (user's policy, 2026-11-13, Pro plan; code: `src/agy.rs`):**
  - Argon (`gemini-4-argon`) is the top tier, the counterpart of Claude's strongest model: judgement work (reviews, design-sensitive edits, implement/refactor/bugfix/test from D2) runs on it whenever its weekly bucket allows. When Argon is used up, Hydrogen (`gemini-4-hydrogen`) stands in.
  - Hydrogen is Sonnet's counterpart: tool work (mechanical, docs, research sweeps and extraction, any D1 task) runs on it.
  - Every model runs at the highest effort `agy models` lists for it as `<model>-<effort>`: Argon `high` (listed low/medium/high), Hydrogen `xhigh` (listed low to xhigh), so an `argon-xhigh` listed later is picked up. Gemini 3.8 Flash at its highest (`high`) is the last fallback, when neither is listed or neither has quota.
  - The caller's `--model` and `--effort` win; a model listed bare (`gpt-oss-120b-medium`) gets no `--effort`. Claude Opus/Sonnet 5.5 (3p buckets) run only when `--model` names them. Argon is the preferred model for polishing paper prose (read-only suggestions the lead verifies).
  - `agy models` on 2026-11-13 (agy 1.5.7, Linux aarch64): `gemini-4-argon-{low,medium,high}`, `gemini-4-hydrogen-{low,medium,high,xhigh}`, `gemini-pro-orchestrator-g1-pro` (the TUI default "Orchestrator (Argon)"), `gemini-3.8-flash-{low,medium,high}`, `claude-opus-5-5-{low,medium,high}`, `claude-sonnet-5-5-{low,medium,high}`, `gpt-oss-120b-medium`. The 4.6 models of 2026-10-29 are gone.
- **Skill files are outside the workspace [M, 2026-11-13]:** baton's first message forbids reading outside the root *except* a skill's SKILL.md and the files it links to under `~/.gemini/config/plugins/`. Without that exception Hydrogen at `medium` obeyed the prompt and refused to open the skill; Argon and Hydrogen at `high`/`xhigh` opened it anyway.
- **Always `</dev/null`** (or a pipe you control). With no creds, agy waits 60 s on the controlling TTY for an OAuth code [M].
- **cwd is the workspace.** Workspace reads and writes are auto-allowed only under cwd and `--add-dir` roots. `.agents/` customizations are found by walking up from cwd to the repo root. An untrusted `/private/tmp` workspace still loaded `.agents/agents/*.md` [M].
- **Timeouts:** use `--print-timeout`, plus a baton-side SIGTERM then SIGKILL. agy **ignores SIGALRM**: a `perl alarm` wrapper never killed it [M]. SIGINT gives status `INTERRUPTED` and a non-zero exit.
- **Fixed overhead:** a 1-line run took 5–8 s wall for 2.1–2.3 s of model time [M]. That is about 3–5 s of start and stop: eligibility check, hooks, MCP. Hold a stream session open to amortise it (2.6).
- **Pin the model** so a `/model` choice made in the TUI can't change baton runs. The default here is `Gemini 3.8 Flash (High)` from settings.

### 2.2 Read-only analysis
Give the agent no write or shell tools, so read-only is guaranteed by its tool list rather than by the prompt. Put it in agy's global agents dir, `~/.gemini/config/agents/baton-reader.md`, which agy loads from any cwd, so nothing is written into the folder being read. Leave out `find_by_name` and `list_dir`: with them agy searched `/` and other users' homes (below), so name the files in the prompt instead:
```markdown
---
name: baton-reader
description: Read-only code analyst driven by baton.
excludeDefaultComponents: false
inheritCustomizations: true
subagent: false
tools: [view_file, grep_search, finish]
---
# System Prompt
Investigate and answer. Never modify files. Call finish with the result.
```
`agy -p "$Q" --agent baton-reader --add-dir "$PWD" --model claude-opus-4-6-thinking --output-format json --print-timeout 10m`

What baton does now (2026-10-29, agy 1.4.9):
- Every read run, in git or not, uses the global `~/.gemini/config/agents/baton-read-worker.md` (a name distinct from the sample above) with tools `view_file, grep_search, finish`, and writes nothing into the folder or its `.git/info/exclude`. Only edit runs write `.agents/agents/baton-worker.md`, into their own worktree. Measured [M]: a run from a non-git folder outside `trustedWorkspaces` started the global agent and read the file with one `view_file` on its absolute path.
- agy was never told its workspace, and with relative brief paths it ran `find_by_name` over `/`, `/home` and other users' home directories [M]. Baton now opens the first message with the absolute workspace root, says every brief path is relative to it, and forbids listing, searching or reading outside it. Read runs also pass `--add-dir <root>` and list the brief's `## Files` globs as up to 200 absolute paths (git's file list in a repository, else a walk from each pattern's literal leading folder that skips `.git`, `target`, `node_modules` and folders no pattern can reach, within 50,000 entries; a cut list is flagged in the prompt), so the read agent needs no `find_by_name` or `list_dir`.
- Reads outside the root [M, 2026-10-29]: a baton read brief that asked for `/etc/hostname`, `/etc/os-release` and `~/.bashrc` read only the in-root file; the agent made no tool call on the three outside paths and refused them from the prompt. Run directly with the read agent and `--add-dir <root>` but no such prompt, agy called `view_file /etc/os-release` and headless soft-denied it (`denied_actions: [{action: read_file}]`, stderr `jetski: no output produced … "read_file" permission`), ending the turn before the other two. So agy's own permission layer blocks reads outside the workspace; a user `read_file(...)` allow rule or `allowNonWorkspaceAccess: true` would lift it, and then only the prompt remains.
- Customizations [M, 2026-10-29, agy 1.4.10]: baton's agents set `inheritCustomizations: true` and `excludeDefaultComponents: false`, so the user's skills, rules, plugins (24 synced by `baton sync --to agy`) and hooks load and the skills are listed. Read from the stored prompt (`~/.gemini/antigravity-cli/conversations/<id>.db`, table `gen_metadata`) of two real read runs on `claude-opus-4-6-thinking`:
  - Declared tools were exactly `view_file, grep_search, finish, manage_task` with either value of `excludeDefaultComponents`. agy adds `manage_task` itself; this morning's agents with `inheritCustomizations: false` had it too. `init.tools` still listed all 60 tools.
  - `excludeDefaultComponents: true`: the system prompt was the agent body alone, no skills (first call 2,405 input tokens); the agent said no skills were named.
  - `excludeDefaultComponents: false`: the prompt carried `<user_information>`, `<skills>`, `<messaging>`, `<artifacts>`, `<guidelines>` and `<communication_style>`, with the user's plugin skills (first call 10,382 input tokens; the second read 10,307 from cache). The agent named `yu-writing-taste`, the `academic-*` skills and `baton`, and `view_file` on `~/.gemini/config/plugins/baton/skills/baton/SKILL.md` was allowed in read mode. The run took 13,082 input tokens, 735 output, 20 s.
- Do **not** rely on `--mode plan`. It only prepends `/plan`, and headless "proceeds through plan review automatically" [C], so a plan run can go on to implement.
- With the default agent, the user's global allowlist (`command(ls)`, `command(grep)`, `command(git status|log|diff|show)`, `command(echo)`, `command(agy)` …) lets the agent run those commands without asking. It also lets it explore **outside** the workspace: an accidental run did `ls ..` and `ls ../..` [M].

### 2.3 Edits in a worktree
```sh
git worktree add "$WT" -b baton/$ID && cd "$WT"
agy -p "$TASK" --agent baton-worker --mode accept-edits --model claude-opus-4-6-thinking \
    --output-format stream-json --print-timeout 30m </dev/null
```
- **Measured [M]:** `--mode accept-edits` with no dangerous flag created `hello.txt` through `write_to_file` and exited 0. `git status` showed `?? hello.txt`.
- **Shell commands are soft-denied** unless an allow rule matches. Under the default `toolPermission: request-review`, every command is classified `unsandboxed` and needs a `command(...)` allow rule.
- Options to let tests and builds run without the dangerous flag:
  1. The user adds scoped rules to `permissions.allow` in global `settings.json` (`command(cargo test)`, `command(regex:npm run (build|lint|test))`). This is the only option verified by the docs.
  2. **[I] Untested and the highest-value next experiment:** a workspace `.agents/hooks.json` `PreToolUse` hook returning `{"decision":"auto_approve"}` for vetted commands. The hook would be baton's own policy engine, committed nowhere. The docs say `auto_approve` "approves when the default policy would otherwise prompt, while honoring explicit user-configured rules".
  3. **[I]** `--sandbox --dangerously-skip-permissions` in a throwaway worktree: auto-approves everything, but commands stay confined (no network, writes limited to the workspace and temp). It is the safest "full auto" combination and is untested.
  4. **[I]** A dedicated `--project baton` whose project-scope grants hold baton's allowlist, set once by the user through `/permissions`. This keeps baton's rules out of the global settings.
- Agent frontmatter `commandExecutionPolicy: off|auto|eager|sandbox` (default `sandbox`) [D]. Its interaction with headless soft-deny is untested.

### 2.4 Structured output
```sh
agy -p "$Q" --agent baton-worker --json-schema schema.json --output-format json
jq -e '.structured_output' out.json   # MUST check presence
```
- Envelope fields: `conversation_id, status, response, duration_seconds, num_turns, usage{input_tokens,output_tokens,thinking_tokens,cache_read_tokens,total_tokens}`, plus `structured_output`, `json_schema`, `error` and `denied_actions` when relevant [D][M].
- **The schema is delivered through the `finish` tool.** A lean agent without `finish` failed [M]. The harness injected `"You did not call any tools … call the finish tool"` three times, giving `num_turns:4` and four copies of the answer in `response`. It still returned `status:"SUCCESS"`, exit 0 and **no `structured_output`**.
- So every custom agent used with `--json-schema` must list `finish` in `tools`. [I] I could not confirm that `finish` alone fixes it, because the follow-up run hit a soft-deny first.
- In stream-json, the schema applies only to the final `result` event.

### 2.5 Detecting soft-denied actions (headless never prompts) [M]
A denied run looks like success from the outside: **exit 0** and `status:"SUCCESS"`, but the turn ends at the denial, often with **`response:""`**. Baton should treat any one of these signals as "blocked":
1. `result.denied_actions: [{"action":"command","display_name":"RunCommand"}]`.
2. A stream `step_update` with `"state":"ERROR"`, `step_type:"tool"` and `tool_info.error = {type:"TOOL_ERROR", message:"permission check failed for unsandboxed \"uname -s\": user denied permission to run command: … Do not attempt to circumvent this denial …"}`.
3. A stderr line: `jetski: no output produced — a tool required the "command" permission that headless mode cannot prompt for, so it was auto-denied. Add an allow-rule under permissions.allow in settings.json (e.g. command(<target>)). …`
4. An empty `response` together with `status:"SUCCESS"`.

Actions that default to Ask, and so are soft-denied in headless: `command`, `mcp`, `read_url`, `execute_url`, and file access outside the workspace. Subagent permission requests bubble up and are denied the same way. Since 1.2.15 the agent "respects the denial and no longer tries to work around it" [C].

### 2.6 Multi-turn, resume, conversations
- **Preferred:** one long-lived process, `agy --input-format stream-json --output-format stream-json [--agent …]`. Send `{"event":"user","message":{"content":"…"}}` per turn and wait for each `result`.
  - Every turn emits its own `result`. `response` covers that turn only; `usage`, `num_turns` and `duration_seconds` are **cumulative**.
  - In the docs' example, turn 2 got `cache_read_tokens: 30214` out of 30,662 [D].
  - Close stdin to finish; the process exits 0 after the current turn.
  - An unknown `event` is skipped with a warning. `control_request`/`control_response` or a CLI slash command gives ERROR and exit 2. Bad JSON, a missing `event` or a non-text block gives ERROR and exit 1.
- `--conversation <id> -p …` starts a new process and reloads the whole trajectory. A caller observed 728 s and 27k uncached input on one resume. Use it only with `--print-timeout`.
- `-c` picks `cache/last_conversations.json[cwd]`, which **races under parallel runs**. Never use it in baton; always pass the id.
- **Storage [M]:**
  - `~/.gemini/antigravity-cli/conversations/<id>.db` is SQLite, about 0.4–0.46 MB **per one-line run**. Tables: `steps`, `gen_metadata`, `trajectory_meta`, …
  - `brain/<id>/.system_generated/logs/transcript.jsonl` and `transcript_full.jsonl` hold steps with per-model-call `input_tokens`, `cache_read_tokens` and `output_tokens`. `brain/<id>/scratch` is the agent's scratch dir.
  - `conversation_summaries.db` is the index.
  - There is no CLI prune; `/resume` deletes interactively with F4. Baton should track ids and offer cleanup.
- Hooks receive `transcriptPath`. Baton can tail the transcript to monitor a run, including subagents via `subagent_info.log_uri`.

### 2.7 Exit codes, statuses, errors
- **Exit codes:**
  - `0`: success. Also soft-deny, a partial result after `--print-timeout`, and benign tool errors [C].
  - `1`: bad `--model`, `--agent` or `--json-schema`, or invalid stream input.
  - `2`: usage errors (`-p` with `-i`, stream control events, CLI slash commands inside a stream).
  - `3`: model or agent API failure. stderr gets `AGY_ERROR: {canonical status, HTTP/gRPC code, retryable, error id}`, and JSON output includes the partial response [C].
- **Status values:** `SUCCESS`, `ERROR`, `CANCELED`, `INTERRUPTED`, `INVALID`, `WAITING`, `RUNNING` [D].
- **Retries:** rate limits with a server delay of 30 s or less are retried. A daily or billing cap, or "individual quota", fails immediately. With no credits you get `Your AI credits balance is too low to continue.` [C].
- **Effective settings [M]:** `toolPermission: request-review`, `agentMode: ""`, `enableTerminalSandbox: false`, `artifactReviewPolicy: asks-for-review`, `allowNonWorkspaceAccess: false`, `useG1Credits: false`, `verbosity: medium`, `trustedWorkspaces: [~]`.
- Headless honours the persisted settings policies (`permissions`, sandbox, artifact review) [C].

---

## 3. Token levers (measured)

Reference numbers only: since 2026-10-29 baton runs the strongest model with the user's customizations and agy's default prompt sections (reliability over cost, section 2.2), not these levers.
All runs used the prompt "Reply with exactly: ok" unless stated, in throwaway git repos under the scratchpad [M].

| Run | Agent | Model / effort | input tok | output (thinking) | cache_read | model s / wall s |
|---|---|---|---|---|---|---|
| e1 | default | Flash High (settings default) | **11,906** | 63 (62) | 0 | 2.28 / 8 |
| e2 | `lean` (no tools, `excludeDefaultComponents:true`, `inheritCustomizations:false`) | Flash High | **572** | 46 (45) | 0 | 2.23 / 5 |
| e3 | default | `gemini-3.8-flash-low` | 11,900 | **1 (0)** | 0 | 2.10 / 5 |
| e4 | default, `--mode accept-edits`, 2 tool calls | flash-low | 24,174 (11,930 + 12,244) | 258 (0) | **0** | 5.98 / 9 |
| e5 | `lean` + `--json-schema` (no `finish`) | `gemini-3.8-flash --effort low` | 3,016 over 4 forced turns (586→922) | 60 | 0 | 10.1 / 13 |
| e6 | `worker` (tools `view_file`, `run_command`, `finish`) | flash-low | **2,235** first call | 53 | 0 | 2.37 / 6 |
| accidental | default, prompt `x` (`--mode bogus`) | Flash High | 66,528 over 5 calls | 822 (321) | 0 | 16.3 |

Findings:
1. **The fixed prompt of the default agent is about 11.3k tokens.** The system prompt is about 14k chars (≈3.5k tokens). Its sections: identity, artifacts 3.7k chars, conversation_transcript 1.9k, slash_commands 1.5k, subagents 2.0k, skills 2.0k, messaging, user_information, guidelines. The remaining ≈8k tokens are tool declarations.
   - A **lean custom agent cuts this to about 0.57k (−95%)**. Each listed tool costs about 0.55k tokens (3 tools = +1.66k).
   - The default agent's prompt metadata is 71 KB vs 7 KB for the lean one (`gen_metadata` blob in the conversation DB).
2. **Every model call re-sends the whole context.** Within one turn, step 2 of e4 cost a full 12.2k again with zero cache.
   - Implicit caching only appeared after the context reached about 17–18k tokens. In a concurrent interactive session, cache hits started at the 7th call (prefix 17.8k → `cache_read 15,765`). After that, 85–95% of input came from cache.
   - Implication: short tool-heavy runs pay full price on every step. Long or streamed sessions get cheaper per step.
3. **Effort:** `low` removes thinking entirely for trivial prompts (high: 62 thinking tokens). `gemini-3.8-flash-low` ≡ `--model gemini-3.8-flash --effort low`.
4. **Exploration is the hidden cost.** An ambiguous one-character prompt made the default agent run 4 allowlisted shell commands and spend 66k input tokens. Lean agents with restricted tools and precise prompts avoid this.
5. **Other levers:**
   - `inheritCustomizations: false` drops the user's skills, rules, plugins, subagents and MCP from the agent.
   - `rules: [path]` names exact rules.
   - Skills are progressive: only name and description are listed (the default agent lists 2 built-ins, about 2k chars).
   - Rules have a 20k-token budget and a 24 KB per-file cap. Use `trigger: model_decision` instead of `always_on`.
   - MCP tools default to on-demand discovery through `call_mcp_tool` unless the server sets `alwaysLoad`.
   - `--disable-slash-commands`.
   - Keep `.agents/` lean in worktrees.
   - No global `GEMINI.md` or `AGENTS.md` exists today.

---

## 4. Quota reading

### 4.1 `/usage` (also `/quota`), free [M]
- **Does not consume quota.** Three back-to-back calls returned the identical `remaining_fraction 0.8386927843093872`. Each call takes about 1–2 s and creates no conversation.
- Text output is one TSV line per bucket, with UTC timestamps when piped (local time on a TTY [C]):
```
Gemini Models	Weekly Limit Remaining	84%	2026-10-26T12:50:34Z
Claude and GPT models	Weekly Limit Remaining	100%	2026-10-26T13:40:49Z
```
- JSON output (`agy -p /usage --output-format json`) has the data under `command.data`:
```json
{"description":"Within each group, models share a weekly limit. Quota is consumed proportionally to the cost of the tokens…",
 "groups":[{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro",
   "buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"…fully refresh in 6 days, 23 hours.","window":"weekly","remaining_fraction":0.8386927843093872,"reset_time":"2026-10-26T12:50:34Z"}]},
  {"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
   "buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-10-26T13:40:52Z"}]}]}
```
- **Buckets:** `gemini-weekly` covers the Gemini Models group; `3p-weekly` covers the Claude and GPT models group.
- The **weekly window starts at first use.** An unused bucket reports `reset_time = now + 7 d`, which slides on every call. A used bucket's reset is fixed.
- On the free tier (until 2026-11-12) this account had **only weekly buckets**, which matches the docs ("refreshed weekly").
- **Pro plan [M, 2026-11-13, agy 1.5.7]:** five buckets in three groups. Each group has a weekly and a five-hour bucket ("Within each group, models share a weekly limit and a 5-hour limit"), plus a separate Argon group:

  | Group | Bucket `id` | `window` | Meters |
  |---|---|---|---|
  | Gemini Models | `gemini-weekly`, `gemini-5h` | `weekly`, `5h` | Hydrogen, Flash |
  | Claude and GPT models | `3p-weekly`, `3p-5h` | `weekly`, `5h` | Claude Opus/Sonnet 5.5, GPT-OSS |
  | Gemini 4 Argon | `gemini-argon-wtu-weekly` | `weekly` | Argon (separate on Pro; part of the Gemini bucket on Ultra) |

  baton parses every bucket generically by its `window` (`weekly`, `daily`, `<n>m|h|d`) and skips one it cannot read; routing gates each model by the tightest bucket that meters it. `baton usage` prints all five (`argon wk`, `gemini 5h`, `gemini wk`, `claude+gpt 5h`, `claude+gpt wk`, as fractions left); the prompt-hook line prints `AG argon N% 5h N% wk N%` (the Gemini group's 5 h and weekly).
- **`remaining_fraction` is noisy at the 0.5-point level:** reads at most a few minutes apart on 2026-11-13 alternated between two values (Argon 0.9537 and 0.9484) with no run in between, and a bucket sometimes read higher after a run than before. Compare deltas only above about one point.
- `/credits` → `{"remaining_credits":0,"upgrade_uri":…}`. The `useG1Credits` setting (default false) decides whether credits are spent after the plan quota runs out.

### 4.2 Burn-rate observation [M]
- Between 13:40 and 13:54 UTC, `gemini-weekly` fell from 0.8387 to 0.5910 (−24.8 points).
- My runs (about 122k uncached input and 1.5k output in total) were **not** alone. A concurrent interactive `agy` session (pre-existing) burned 236k uncached input, 3.67M cached input and 21k output in the same window. The drop therefore can't be attributed to my runs, and baton can't derive a per-token rate from it.
- Order of magnitude: the free weekly bucket is a few million cost-weighted tokens. Baton should sample `/usage` before and after runs while the machine is otherwise idle to calibrate cost per run, and gate new dispatches when `remaining_fraction` drops below a threshold.

### 4.3 Status line quota JSON [D]
- Configure with `settings.json` → `"statusLine":{"type":"command","command":"…","padding":0,"enabled":true,"stack_with_default":false}`. The CLI pipes the payload to the script's stdin on every state change.
- Payload fields:
  - Identity and session: `cwd`, `conversation_id`, `session_id`, `transcript_path`, `model{id,display_name}`, `workspace{current_dir,project_dir}`, `version`, `product`, `email`, `plan_tier`.
  - Context: `context_window{total_input_tokens,total_output_tokens,context_window_size,used_percentage,remaining_percentage,current_usage{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}}`, `exceeds_200k_tokens`.
  - Quota and cost: `quota{<bucketId>:{remaining_fraction,reset_time,reset_in_seconds}}`, `cost` (unrounded estimated session cost [C]).
  - State: `agent_state` (idle, thinking, working, tool_use, initializing), `vcs`, `sandbox`, `artifact_count`, `pending_input_count`, `tool_confirmation_pending`, `task_count`, `terminal_width`, `execution_mode`, `conversation_title`.
- The status line only runs in the TUI. For headless runs, use `/usage`.

---

## 5. Packaging: plugins, skills, agents, hooks, MCP, rules

### 5.1 Discovery roots
- **Workspace:** `.agents/` (also `.agent/`, `_agents/`, `_agent/`) in every directory from cwd up to the repo root. It contains:
  - `agents/<n>.md` or `agents/<n>/agent.md`
  - `skills/<n>/SKILL.md`
  - `plugins/<n>/`
  - `rules/*.md` (flat scan)
  - `hooks.json`
  - `mcp_config.json`
  - Manifests `skills.json`, `plugins.json`, `agents.json`, `rules.json`, shaped `{"entries":[{"path":…,"include_only":[…],"exclude":[…]}],"inherits":[…]}`. Each entry is scanned one level deep [B][D].
- **Global:** `~/.gemini/config/` (`agents/`, `skills/`, `plugins/`, `rules/`, `hooks.json`, `mcp_config.json`, `config.json`, `AGENTS.md`/`GEMINI.md`). Also `~/.gemini/GEMINI.md|AGENTS.md`, `~/.gemini/antigravity-cli/{skills,rules}/`, and hooks inside `~/.gemini/antigravity-cli/settings.json`.
- **Precedence:** workspace > workspace-declared JSON > global discovery > built-ins > global-declared JSON.
- **Name collisions:** the first plugin found wins and the others are dropped **silently**. MCP server names are global, so prefix them.
- **Verified [M]:** a native plugin under the workspace's `.agents/plugins/` loads **without any install**. Its skill appears as `natplug:nat-hello`, its agent `nat-worker` is listed, and its `hooks.json` is active (`/skills`, `/agents`, `/hooks` in w3). **This is the zero-global-footprint packaging path for baton.** Materialise the plugin into each worktree, or point `.agents/plugins.json` at baton's shared directory.

### 5.2 Native plugin layout
```
<plugin>/
  plugin.json            # or .antigravity-plugin/plugin.json / .gemini-plugin/plugin.json (JSONC ok)
  skills/<skill>/SKILL.md
  agents/<agent>.md      # or root agents.json
  rules/AGENTS.md        # plain md, always-on while enabled (or rules/*.md with trigger frontmatter)
  hooks.json             # MUST be at plugin root
  mcp_config.json
  bin/                   # executables the agent can run by name
  assets/logo.svg
```
- `plugin.json` fields: `name` (required for CLI management, `^[a-zA-Z0-9-_]+$`), `description`, `version`, `displayName`, `logo` (relative path, png/svg/jpg/webp), `suggestedPrompts` (≤3), `disabled`, `skills` list, `$schema: https://antigravity.google/schemas/v1/plugin.json`.
- Unknown fields such as `author` and `homepage` are discarded, and validate warns about them.
- `PLUGIN_DATA` is a per-plugin data dir. The binary also contains `PLUGIN_ROOT=` [B]; an env var for hook commands is likely [I].
- Enable state lives in `~/.gemini/config/config.json` → `plugins.<dir>.enabled`, keyed by **install directory**, not by `name`.
- `agy plugin validate <dir> --json [--strict]` is read-only and CI-friendly, and baton should run it on generated plugins.
- **Install sources:** marketplace name, `https://github.com/<owner>/<repo>`, or a local dir. **Repo-subdirectory install is undocumented:** clone the repo and install the subdir path. Installs land in **`~/.gemini/config/plugins/<name>/`** [M] (the docs say `~/.gemini/antigravity-cli/plugins/`, which is stale).

### 5.3 Skills
- `SKILL.md` follows the open standard (agentskills.io). Frontmatter: `name` (it should match the folder), `description` (required), `disable-model-invocation`, `disable-slash-command` (hidden from the `/` menu but still model-invocable), `metadata.icon` [B][C].
- The body limit is 24 KB.
- **Progressive disclosure:** only name and description go in the prompt. The model reads the file when it is relevant (`view_file` with `IsSkillFile`).
- Every skill becomes `/skill-name` (`/<plugin>:<skill>` for plugin skills).
- [I] An agent built with `excludeDefaultComponents` needs `view_file` to read skills. `preloadSkills` (a frontmatter key in the binary) probably inlines specific skills; verify before relying on it.
- **What a skill can read [M, 2026-11-13, agy 1.5.7, headless read agent with `--add-dir <root>`]:** a probe plugin's skill read every file inside the installed plugin (`~/.gemini/config/plugins/<p>/`), also outside the skill's own folder; a file under `$TMPDIR` was readable; a file under `~/.claude/plugins/cache/` was soft-denied (`denied_actions: [read_file]`). So a skill's resources must be copied into the agy plugin; absolute paths into the Claude install do not work headless. The tool now reports as `read_file`; agent files listing `view_file` still get it.
- **`agy plugin install <dir>` copies the plugin** into `~/.gemini/config/plugins/<name>/`: a symlink that stays inside the plugin is dereferenced, one that points outside it fails the install ("symlink … points outside").
- **baton's converter (`baton sync --to agy`, converter 2, 2026-11-13):** stages the skills Claude Code loads: `skills/<s>/SKILL.md`, or the SKILL.md a Codex adapter shim there points at (`../../SKILL.md`, `../../<dir>/SKILL.md`); the folders a marketplace entry or plugin manifest lists under `skills`; else a root SKILL.md. A root SKILL.md's plugin files go beside it; a skill folder's `../../X` and `${CLAUDE_PLUGIN_ROOT}/X` go to the plugin root, and the variable becomes the install path. Referenced entries are followed through their Markdown; one over 2 MB stays behind (mo-academic-taste's 4.6 MB `corpus/`). `.baton-sync.json` carries `converter`, so a converter change re-stages every plugin. agy does not refuse a SKILL.md over the 24 KB limit (distributed-training's `deepspeed`, 145 KB, loads).

### 5.4 Custom agents (Markdown)
- **Frontmatter keys** [D][C][B]:
  - `name`, `description` (both required).
  - `tools: [..]`: a **YAML list** of agy tool names.
  - `model: inherit|flash|pro`: tier used when invoked as a subagent.
  - `mainAgent`, `subagent`, `hidden` (booleans).
  - `commandExecutionPolicy: off|auto|eager|sandbox`.
  - `mcpServers` (list), `inheritMcp`.
  - `skills`, `plugins`, `rules`, `agents`, `hooks`: paths relative to the agent file, the workspace, or absolute.
  - `inheritCustomizations` (default true).
  - **`excludeDefaultComponents: true`**: drops the default prompt sections and built-in tools; "post-invocation hooks preserved".
  - `preloadSkills`.
- The body after the frontmatter is the system prompt; H1 sections are allowed.
- An agent listing `invoke_subagent` automatically receives the roster of its subagents.
- **Pitfalls:**
  - A misspelt tool name "may cause the subagent process to hang" [D].
  - Reserved built-in names: `research`, `browser`, `self`, `image-generator`.
- Tool names for `tools:`, from stream `init` and the hooks docs: `view_file`, `write_to_file`, `replace_file_content`, `multi_replace_file_content`, `sed_file`, `list_dir`, `find_by_name`, `grep_search`, `run_command`, `command_status`, `send_command_input`, `manage_task`, `schedule`, `wait`, `read_url_content`, `search_web`, `call_mcp_tool`, `list_resources`, `read_resource`, `invoke_subagent`, `define_subagent`, `send_message`, `manage_subagents`, `manage_inbox`, `ask_question`, `ask_permission`, `ask_custom_permission`, `list_permissions`, `generate_image`, `notebook_edit`, `notebook_execution`, `run_workflow`, `finish`, the `browser_*` tools, `search_marketplace`, `list_plugin_accounts`, `delete_knowledge`.
- **Note:** stream `init.tools` always lists all 60 tools, even for a restricted agent. It is not the effective toolset [M].

### 5.5 Hooks (`hooks.json`)
- **Schema:** `{ "<hookName>": { "enabled"?: bool, "<Event>": [...] } }`. Locations: workspace `.agents/hooks.json`, global `~/.gemini/config/hooks.json` or settings.json, plugin-root `hooks.json`, and agent frontmatter `hooks:`.
- Tool events take matcher groups: `[{"matcher":"run_command|view_file","hooks":[{"type":"command","command":"…","timeout":30}]}]`. The matcher is a regex; `""` or `"*"` means all. Other events take a flat list of handlers: `[{"type":"command","command":"…","timeout":10}]`.
- The default timeout is 30 s.
- I/O is camelCase JSON on stdin and stdout. Common input fields: `conversationId`, `workspacePaths`, `transcriptPath`, `artifactDirectoryPath`, `modelName`.

| Event | Extra input | Output (decision values) |
|---|---|---|
| `SessionStart` | — | `injectSteps:[{systemMessage:{systemMessage:"…"}}\|{userMessage:"…"}\|{ephemeralMessage:"…"}\|{toolCall:{…}}]` |
| `PreToolUse` | `toolCall{name,args}`, `stepIdx` | **`decision` (required)**: `allow` (delegate to next/default), `auto_approve` (approve where the default would prompt, honouring explicit user rules), `deny` (hard block), `ask` (prompt; honours Always-Allow), `force_ask` (always prompt), `deny_unless_prior_grant`. Optional: `reason`, `overwrite` (shallow-merged args; the agent is told which keys changed), `permissionOverrides:["command(npm test)",…]` |
| `PostToolUse` | `toolCall`, `stepIdx`, `error` | `{}` |
| `PreInvocation` | `invocationNum`, `initialNumSteps` | `injectSteps` (inject baton messages before each model call) |
| `PostInvocation` | same | `injectSteps`, `terminationBehavior: "force_continue"\|"terminate"\|""`, so a **budget guard can hard-stop a run** |
| `Stop` | `executionNum`, `terminationReason` (`model_stop`/`max_steps_exceeded`/`error`), `error`, `fullyIdle` | `decision:"continue"` + `reason` (re-enter the loop; capped after N consecutive continues [C]) |

Built-in termination checks run after hooks.json hooks [C]. `/btw` side questions skip hooks.

### 5.6 MCP
- **Locations:** workspace `.agents/mcp_config.json` (no global footprint), plugin `mcp_config.json`, global `~/.gemini/config/mcp_config.json` (empty today).
- **Schema:** `{"mcpServers":{"name":{"command":"…","args":[…],"env":{…},"cwd":""}}}` for stdio. Remote servers take `"serverUrl":"https://…/mcp"` with `headers`, which support `${VAR}` substitution. The built-in docs say `url` is the key and `serverUrl` the legacy alias, while the web docs require `serverUrl`, so use `serverUrl`, which both accept.
- Streamable HTTP only, no legacy SSE. OAuth is supported (`mcp_oauth_tokens.json`).
- MCP calls default to **Ask, which means they are soft-denied in headless** unless `mcp(server/*)` is allowed.

### 5.7 Rules and context
- **Loaded:** `AGENTS.md`/`GEMINI.md` (no frontmatter, always on) from each directory between the touched file and the repo root, plus `.agents/AGENTS.md` and `.agents/rules/*.md`. `rules/*.md` need frontmatter with `trigger: always_on|model_decision|glob|manual`, plus `description` and `globs`. An invalid trigger means the rule is **silently dropped**.
- `@[label](path)` inlines another file; `@path` only references it.
- **Not loaded:** `CLAUDE.md`.
- **Budget:** 20k tokens for always-on and global rules; over-budget rules are demoted to path pointers. 24 KB per file.
- **Context size:**
  - An agent with `inheritCustomizations: false` and an explicit `rules:` loads only those rules; baton's workers inherit everything instead (since 2026-10-29).
  - `trigger: model_decision` rules cost a description until used.
  - A huge `AGENTS.md` committed into a repo is loaded into every agy run there.

### 5.8 Converting a Claude Code plugin (tested in a fake HOME and the workspace) [M]
- `agy plugin validate` on a Claude layout fails: *"This looks like a Claude Code plugin: `plugin install` converts it, but it doesn't load as is."*
- `agy plugin install <dir>` converted it and printed `skills 1 / agents 1 / commands 1 (converted to skills) / mcpServers 1 / hooks 1 processed`. Re-validating and loading the result exposed these gaps:

| Claude Code | Conversion result | What baton's converter must do |
|---|---|---|
| `.claude-plugin/plugin.json` | Root `plugin.json` written; `author` object became a string (then discarded) | Emit root `plugin.json` with `name`, `description`, `version` only |
| `skills/*/SKILL.md` | Copied, loads as `plugin:skill` | Usually as-is (24 KB limit, `name` = folder) |
| `commands/*.md` | Became `skills/<plugin>-cmd-<cmd>/SKILL.md` **without `name`**, so it loads as `plugin:SKILL`; `$ARGUMENTS` stays literal | Add `name:`; rewrite `$ARGUMENTS` usage (the args are in the prompt text) |
| `agents/*.md` | Copied verbatim → **skipped**: `tools: Read, Grep, Glob` is a string, not a list (YAML error); `model: sonnet` is not a tier | Map tools to a YAML list of agy names (Read→`view_file`, Write→`write_to_file`, Edit→`replace_file_content`/`multi_replace_file_content`, Grep→`grep_search`, Glob→`find_by_name`, LS→`list_dir`, Bash→`run_command`, WebFetch→`read_url_content`, WebSearch→`search_web`, Task→`invoke_subagent`); `model` → `inherit\|flash\|pro`; consider `excludeDefaultComponents: true` |
| `hooks/hooks.json` `{"hooks":{…}}` | Copied to root `hooks.json`, loaded as **one hook named `hooks`**. The `PreToolUse` matcher stays `Bash` (matches nothing), `${CLAUDE_PLUGIN_ROOT}` is left literal, and the `SessionStart` command is **empty** (Claude's nested `{"hooks":[…]}` wrapper isn't understood for non-tool events) | Rename the top key; map matchers to agy tool names; flatten non-tool events; use an absolute path or `$PLUGIN_ROOT` [I]; rewrite output to the stdout `{"decision":…}` contract (Claude's exit-2 and `permissionDecision` conventions don't apply); Stop `block` → `continue`; `UserPromptSubmit` → `PreInvocation` (invocationNum 0) or `SessionStart` with `injectSteps`; there is no equivalent for Notification, SubagentStop, PreCompact or SessionEnd |
| `.mcp.json` | Converted correctly to `mcp_config.json` | Prefix server names; remote → `serverUrl` |
| `CLAUDE.md`, `output-styles/`, `statusline` | Not handled | Rules → `rules/AGENTS.md` |
| `bin/` | Native `bin/` is on the agent's PATH | — |

- `agy plugin import claude` also exists ("import plugins from … claude"), presumably reading installed Claude Code plugins. It was not run, because it writes to the global config.
- Recommendation: baton should generate native workspace plugins itself rather than rely on `install`.

---

## 6. Subagents, Remote Control and stream-json capabilities for baton

### 6.1 stream-json output events [D][M]
```
{"event":"init","conversation_id":…,"init":{cwd,tools[],permission_mode:"request-review"|"always-proceed",model?,agent?,json_schema?}}
{"event":"step_update","step_update":{conversation_id,step_index,state:"ACTIVE"|"DONE"|"ERROR",step_type:"user_input"|"agent_response"|"tool"|"checkpoint",
   tool_name?,text_delta?,duration_seconds?,usage?,tool_info?{name,parameters,output?,error?{type,message}},subagent_info?{subagents:[{type_name,role,conversation_id,log_uri,workspace_uris}]}}}
{"event":"result","result":{conversation_id,status,response,duration_seconds,num_turns,usage,denied_actions?,structured_output?,json_schema?,error?}}
```
- `state:"ERROR"` is observed but undocumented.
- Per-step `usage` appears on `agent_response` and `checkpoint` steps. In my runs `write_to_file` reported no `output`.
- Concatenate `text_delta` with `jq -j`.
- For a baton UI: render tool steps live, sum per-step usage into a budget, kill the run on overrun. The `PostInvocation` `terminate` hook is a cleaner way to stop.

### 6.2 Subagents
- **Built-ins:** `research` (codebase exploration), `browser` (the `/browser` command), `self` (a clone of the caller), `image-generator`.
- **Invocation:** `invoke_subagent` with `Subagents:[{Prompt,Role,TypeName,Workspace}]`. `Workspace` is `inherit`, `branch` (an isolated git worktree under the app-data `worktrees/`, one per repo) or `share`.
- **Context:** subagents start with a clean context and don't inherit the parent conversation. Several can run concurrently.
- **Lifecycle:** Running → Idle (re-woken by `send_message`) → Killed. Maximum depth 10.
- **Inheritance:** subagents inherit the parent's allowed command prefixes, file scopes and sandbox setting.
- **Headless:** subagent questions are skipped; their permission requests bubble up and are **soft-denied**.
- **Other features:** `define_subagent` creates agents at runtime. `@<subagent> msg` messages a subagent. `/teamwork-preview` and `/boost` are multi-agent orchestrators on paid plans.
- **For baton:**
  - Prefer **baton-level fan-out** (one `agy` process per worktree). Baton keeps control of timeouts, accounting, permissions and merges.
  - Use agy-internal subagents only for cheap read-only research. Read their progress from `subagent_info.log_uri` and `brain/<child>/…/transcript.jsonl`.

### 6.3 Remote Control (phone or web watching)
- **Interactive only:**
  - `agy --remote-control` or `/remote-control on` prints `Open https://antigravity.google.com/r/<instance-id>?p=c%2F<conversation-id>`.
  - The phone and the TUI stay in live two-way sync: prompts, tool approvals and answers work from either side.
  - The tunnel dies with the process. Each CLI process is its own instance.
- **Daemon:** `agy remote-control start [--name X] [--session]` registers an always-on instance that starts sessions from the dashboard. Rename it via `cliRemoteControlHostname` in `~/.gemini/config/config.json`. Log at `~/Library/Logs/antigravity-cli-daemon.log`.
  - 1.4.2 fixed history display in daemon mode.
  - [I] Whether `-p` conversations show up there is unverified.
  - The hidden `serve` reported "not signed in" today.
- **Other limits:** not supported with ADC sign-in, and it can be disabled per account (`Remote control is not enabled for your account.`).
- **Options for baton:**
  - (a) Run selected agy workers as interactive sessions in a PTY with `--remote-control`, driving input through the PTY, and hand the user the printed link. This is fragile.
  - (b) Install the daemon and let the user browse conversations.
  - (c) Build baton's own mobile view from stream-json and transcripts. This one is the most robust.

### 6.4 Calling back to the lead (Claude)
- **Simplest and permission-free [I]:** `--input-format stream-json` plus a schema with a status field (`{"status":"done"|"need_input","question":…,"result":…}`). When the worker returns `need_input`, baton asks Claude and sends the answer as the next `user` event in the same process, so the context stays warm.
- **Through MCP:** a baton stdio MCP server in the worktree's `.agents/mcp_config.json` exposing `ask_lead`. It needs an `mcp(baton/*)` allow rule, or a `PreToolUse` `auto_approve` hook [I], or it is soft-denied.
- **Through a command:** `run_command baton ask …` needs a `command(baton)` allow rule.
- `ask_question` is useless in headless: the agent settles the choice itself.

---

## 7. Pitfalls (ranked by impact on baton)

1. **Soft-deny looks like success:** exit 0 and `SUCCESS`, often with an empty `response`. Always check `denied_actions`, stream `state:"ERROR"` and stderr `jetski: no output produced`.
2. **`--json-schema` with an agent that lacks `finish`:** three forced re-prompts (4× the cost), `SUCCESS` and no `structured_output`. Always `jq -e .structured_output`.
3. **Typos in `--mode` don't fail.** They only warn, then run a real, quota-spending turn in default mode. This happened in my accidental 7th run (66k tokens). Validate flags in baton. `--mode auto` is not available on this account.
4. **Global hooks from Orca run on every baton run.** `~/.gemini/config/hooks.json` defines `orca-status` on PreInvocation, PostInvocation, PreToolUse(`*`), PostToolUse(`*`) and Stop, each with a 10 s timeout.
   - Each call spawns sh → python3 (which waits up to 5 s for stdin) → curl to localhost. That is latency on every tool call and model call.
   - Its PreToolUse returned `{"decision":"ask"}` until about 13:40 UTC today, when the script on disk changed to `"allow"`.
   - [I] `ask` from a hook could make otherwise auto-allowed actions (edits) prompt, and therefore be soft-denied, in headless.
   - Baton should check `agy -p /hooks --output-format json` at start-up and warn. Whether `inheritCustomizations:false` skips global hooks is unknown.
5. **The user's global allowlist** (`command(ls)`, `command(grep)`, `command(echo)`, `command(git status|log|diff|show)`, **`command(agy)`**, plus several one-off `find`/`strings` lines) lets default-agent workers run those commands without asking. That includes listing outside the workspace and **spawning nested agy**. Restrict worker tools in agent files, and consider asking the user to prune that list.
6. **No prompt caching on short Gemini runs.** Every step re-sends the full context; on Gemini 3.8 Flash caching only started once context passed about 17–18k tokens, so tool-heavy short tasks cost N × 12k with the default agent. On `claude-opus-4-6-thinking` the second call of a 10k-token context already read 10,307 tokens from cache [M, 2026-10-29]. Streamed sessions keep fix rounds warm.
7. **Resume through `--conversation` is slow and uncached** (728 s and 27k tokens observed by the caller). `-c` races on `last_conversations.json`. Use one stream-json process per worker instead.
8. **Kill semantics:** `--print-timeout` expiry gives exit 0 with partial output, so check stderr and `status`. SIGALRM is ignored; use SIGTERM or SIGKILL. A stalled model stream is only cancelled after 5 minutes.
9. **Auth is HOME-bound.** With a fake `HOME`, agy waited 60 s for an OAuth code on the TTY. Never change HOME for workers, and always redirect stdin.
10. **Model churn:** `claude-sonnet-4-6`, `claude-opus-4-6-thinking` and `gpt-oss-120b-medium` disappear on **2026-11-02**. The 5.5 Claude models need Pro (non-trial) or Ultra, and Gemini 4 Argon needs Pro or Ultra. Pin slugs and handle exit 1 from an unknown model; baton checks `agy models` before each run and falls back along its ordered list (2.1).
11. **Claude Code plugins don't load as-is.** Install-time conversion silently drops agents (comma-string `tools`), leaves hooks inert (`Bash` matcher, literal `${CLAUDE_PLUGIN_ROOT}`, empty SessionStart command) and misnames converted commands (`plugin:SKILL`). Until converter 2 (2026-11-13) baton copied only `skills/`, so a Codex adapter shim arrived as a pointer to a root SKILL.md agy never got, and a root-only skill arrived as nothing (5.3).
12. **The docs are partly stale against 1.4.2:**
    - The print-timeout default is 0, not 5m.
    - `--json-schema` doesn't accept primitives.
    - The plugin install path is `~/.gemini/config/plugins`.
    - `agy agents --output-format` doesn't exist.
    - The `url` vs `serverUrl` guidance conflicts.
    - Trust the CLI's own `plugin validate` and the print-mode slash commands over the docs.
13. **Disk growth:** about 0.4–0.46 MB of SQLite plus a `brain/` dir per run, with no CLI prune.
14. **Typo in an agent `tools:` list:** the subagent may hang [D]. Validate with `agy plugin validate`; agent files outside plugins can be validated by wrapping them in a temp plugin.
15. **`agy plugin <sub> --help` is treated as a name:** `uninstall --help` reports success.
16. **`init.tools` is not the effective toolset** (still 60 tools in 1.4.10 whatever the agent; the declared tools are in the stored prompt, section 2.2), and the default agent's real toolset no longer includes `grep_search`, `find_by_name` or `list_dir`.

---

## 8. Experiment log and next experiments

| # | Purpose | Files (`baton-research/exp/`) | Result |
|---|---|---|---|
| e1 | Baseline default agent | `e1-default.json` | 11,906 in |
| e2 | Lean agent | `e2-lean.json`, `w1/.agents/agents/lean.md` | 572 in (−95%) |
| e3 | flash-low + stream-json | `e3-flashlow.ndjson` | 0 thinking; event shapes |
| e4 | accept-edits edit + command soft-deny | `e4-edit.ndjson/.err`, `w2/` | edit OK; `uname` soft-denied; `denied_actions`; empty response |
| e5 | Lean agent + json-schema + effort low | `e5-schema.json` | no `structured_output`; finish-tool nags ×3 |
| e6 | Worker with finish + `--mode auto` + schema | `e6-auto.ndjson/.err`, `w2/.agents/agents/worker.md` | auto unavailable; command soft-denied; 2,235 in |
| (7th, accidental) | `--mode bogus` flag check | transcript 136f70ab… | warning only → real run, 66,528 in |
| free | `/usage` ×4, `/model`, `/effort`, `/skills`, `/agents`, `/hooks`, `/permissions`, `/config`, `/credits` | `exp/slash/` | no quota change; payloads in 1.3 and 4.1 |
| free | Plugin validate, conversion, workspace load | `w3/.agents/plugins/{ccplug,ccconv,natplug}`, `fakehome/` | section 5.8 |

**Highest-value next experiments** (each is one tiny headless run):
1. Lean agent with `tools:[finish]` and `--json-schema`: confirm that `structured_output` appears.
2. Workspace `.agents/hooks.json` `PreToolUse` returning `auto_approve` for `run_command` in headless: does it bypass the soft-deny without the dangerous flag?
3. `--sandbox --dangerously-skip-permissions` in a worktree: confirm that network access is blocked and writes outside the workspace are denied.
4. Two-turn `--input-format stream-json` with a lean agent: measure turn-2 cache and the startup amortisation.
5. Whether `inheritCustomizations:false` or `excludeDefaultComponents` suppresses the global Orca hooks.
6. Whether a converted plugin hook gets `$PLUGIN_ROOT`.

## 9. Sources
- Local: `agy --help` and all subcommand help (`baton-research/agy-help/*.txt`), `agy changelog` (`agy-help/changelog.txt`), the built-in skill docs in `~/.gemini/antigravity-cli/builtin/skills/{agy-customizations,plugin,antigravity_guide}/`, and binary strings of `~/.local/bin/agy`.
- Docs (fetched as `.md` on 2026-10-19 into `baton-research/docs-md/`):
  - CLI: https://antigravity.google/docs/cli/headless, …/cli/reference, …/cli/modes, …/cli/conversations, …/cli/commands/{usage,effort,agents,context,resume,permissions,statusline,credits}, …/cli/statusline, …/cli/credits, …/cli/gcli-migration, …/cli/troubleshooting, …/cli/best-practices
  - Customisation: …/docs/hooks, …/docs/permissions, …/docs/sandbox, …/docs/settings, …/docs/agent-settings, …/docs/subagents, …/docs/plugins, …/docs/marketplace, …/docs/skills, …/docs/rules, …/docs/mcp
  - Account and platform: …/docs/models, …/docs/plans, …/docs/remote-control, …/docs/projects, …/docs/install
  - Index: https://antigravity.google/llms.txt
