# Codex 0.163 / gpt-6.1-sol: what one headless call sends (captured request bodies)

Captured by pointing `codex exec` at a local capture server, so no quota was spent.
Bytes are JSON bytes; tokens ≈ bytes / 4 for this mostly-English text.

| Component (input item) | default config | `--ignore-user-config` | lean worker (`model_instructions_file`) |
|---|---|---|---|
| 0 tools (`additional_tools`: code-mode `exec` 15.7 KB, multi-agent `collaboration.*` 5.4 KB, `request_user_input*` 3.0 KB, `wait` 1.4 KB, `clock.sleep` 0.4 KB) | 25.9 KB | 25.9 KB | 24.9 KB |
| 1 base instructions ("You are Codex, an agent based on GPT-6…") | 22.2 KB | 22.2 KB | 0.2 KB |
| 2 skills list (`<skills_instructions>`) | **43.9 KB** (37 plugins) | 4.4 KB (built-ins) | 3.5 KB |
| 3–4 multi-agent role and mode | 3.0 KB | 3.0 KB | 3.0 KB |
| 5 environment context | 0.7 KB | 0.7 KB | 0.7 KB |
| total request | 96.7 KB | 57.8 KB | 34.0 KB |

Measured usage for a one-line prompt (real runs):
- default config: input 26,580, cached 12,288 → uncached 14,292
- `--ignore-user-config` + effort low: input 14,747, cached 12,288 → uncached 2,459

Findings
- The single largest lever is not loading the user's plugins: the skills list alone is 44 KB (~11k tokens) with 37 plugins installed.
- `features.apps=false` trims 1 KB from the `exec` description; `features.multi_agent=false` and `features.code_mode` toggles did not change the tool payload in 0.163.
- Replacing the base instructions with a short worker prompt (`model_instructions_file`) removes another ~22 KB (~5.5k tokens), but those tokens sit in the cached prefix, and the default text carries tool-usage guidance. Use a tailored worker prompt (keep the apply_patch, sandbox and testing rules), not a one-liner.
- Model choice changes the tool set: `gpt-5.6-luna` gets no `collaboration.*` tools (17.2 KB tools total).
- The prefix (tools + instructions) is cached across calls (`cached_input_tokens` 12,288), so the real per-call cost is the uncached tail: skills list, environment, brief.
- Image generation: `features.image_generation` is on; auto size produced 1254x1254.
