## Goal

Make a fresh machine work with one command, and ship binaries (spec: `DESIGN.md` §2 `setup`).

1. `baton setup --plan [--json]`: inspect the machine and list the steps still needed, each with an
   id, a one-line description, the exact commands it would run, and whether it needs the user
   (login) or consent (install, config change). Steps, in order:
   - `binary`: baton itself at `$BATON_HOME/bin/baton` and a symlink `~/.local/bin/baton` (so Codex
     and agy workers can call `baton`); copies the running executable.
   - `codex-install`: `codex` missing → `curl -fsSL https://chatgpt.com/codex/install.sh | sh`.
   - `agy-install`: `agy` missing → `curl -fsSL https://antigravity.google/cli/install.sh | bash`.
   - `codex-login`: `codex login status` fails → the user runs `codex login --device-auth`.
   - `agy-login`: `agy -p /usage --output-format json </dev/null` fails or reports
     `authentication required` → the user runs `agy` once and signs in.
   - `agy-permissions`: agy's settings (`~/.gemini/antigravity-cli/settings.json`) lack
     `permissions.allow` entries `read_file(<root>/)` and `write_file(<root>/)` for the agy worktree
     root (`$BATON_AGY_WT_DIR`, default `$TMPDIR/baton-wt`, else `/tmp/baton-wt`; resolve symlinks,
     e.g. /tmp → /private/tmp on macOS) → add exactly those two entries.
   - `marketplaces`: Codex lacks the `my-develop-plugins-codex` marketplace → `codex plugin
     marketplace add shiran-yu/my-develop-plugins` and `codex plugin add baton@my-develop-plugins-codex`.
   - `sync`: run `baton sync`.
   - `selftest`: `baton usage --refresh` and `baton doctor`.
2. `baton setup --apply <id>[,<id>...] [--yes]`: run those steps. Without `--yes` and with a TTY,
   ask `y/N` per step that needs consent; without a TTY and without `--yes`, refuse such steps with
   a message (Claude Code relays the question to the user, then calls `--apply ... --yes`). Login
   steps never run anything: they print the command for the user. The settings edit must keep every
   other key and the file's key order, write a backup `settings.json.baton-bak` first, and be a no-op
   when the entries exist.
3. `baton setup` without flags: `--plan` then, on a TTY, offer to apply each pending step.
4. Release builds: add `.github/workflows/release.yml` that, on a pushed tag `v*`, builds
   `baton` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
   `aarch64-unknown-linux-gnu` (use `cross` or the `ubuntu-24.04-arm` runner for linux arm), uploads
   `baton-<target>.tar.gz` and a `SHA256SUMS` file to the GitHub release.
5. The plugin launcher `plugins/baton/bin/baton`: when no binary exists, print one line telling
   Claude to run the setup skill, keep `statusline`/`doctor` silent as now.
6. Add the skill `plugins/baton/skills/setup/SKILL.md` (frontmatter `name: setup`, description
   "Set up baton, Codex and Antigravity on this machine: install, log in, permissions, plugin sync,
   self-test"): it tells Claude to (a) if `baton` is missing, build it — `git clone --depth 1
   https://github.com/shiran-yu/baton "$HOME/.local/share/baton/src"` and `cargo build --release`
   when `cargo` exists, else download the release asset for this platform and check it against
   `SHA256SUMS` — after asking the user; (b) run `baton setup --plan --json`; (c) ask the user once
   for the consent steps, listing them; (d) run `baton setup --apply ... --yes`; (e) for login steps
   tell the user to type `! codex login --device-auth` or `! agy` in the prompt; (f) finish with
   `baton usage`.

## Files

- `src/**`
- `tests/**`
- `.github/workflows/release.yml`
- `plugins/baton/bin/baton`
- `plugins/baton/skills/setup/SKILL.md`

## Forbidden

- New dependencies; `unsafe`; network in tests; running real installers or logins in tests (fake
  `codex`/`agy` on `BATON_CODEX`/`BATON_AGY`, a temp `HOME`); editing outside Files.

## Acceptance

```
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test
sh -n plugins/baton/bin/baton
```

Tests: plan on a fake machine where everything is missing and where everything is present; the
settings edit (adds the two entries, keeps other keys and order, writes the backup, idempotent);
`--apply` without TTY or `--yes` refusing consent steps; login steps printing the command only.

## Context

- Return the final message in the given JSON schema.
