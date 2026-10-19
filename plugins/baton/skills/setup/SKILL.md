---
name: setup
description: "Set up baton, Codex and Antigravity on this machine: install, log in, permissions, plugin sync, self-test"
---

Set up this machine from Claude Code. Use the real binary, not a missing-binary plugin launcher.

1. Locate `baton` on PATH or at `${BATON_HOME:-$HOME/.local/share/baton}/bin/baton`.
   If missing, explain the bootstrap commands and ask the user before installing it:

   - If `cargo` exists, run
     `git clone --depth 1 https://github.com/shiran-yu/baton "$HOME/.local/share/baton/src"`
     and `cargo build --release --manifest-path "$HOME/.local/share/baton/src/Cargo.toml"`.
     Use `$HOME/.local/share/baton/src/target/release/baton` for the next commands.
     If the clone directory already exists, inspect it and build the existing baton checkout;
     do not delete or overwrite the user's files.
   - Otherwise, select the release target using `uname -s` and `uname -m`:
     Darwin arm64/aarch64 → `aarch64-apple-darwin`, Darwin x86_64 → `x86_64-apple-darwin`,
     Linux x86_64 → `x86_64-unknown-linux-gnu`, Linux arm64/aarch64 → `aarch64-unknown-linux-gnu`.
     Stop and explain if the platform is unsupported. Download `baton-<target>.tar.gz` and
     `SHA256SUMS` from the **same release tag** at `https://github.com/shiran-yu/baton/releases`
     into a temporary directory. Resolve the latest release to a tag once, then use
     `https://github.com/shiran-yu/baton/releases/download/<tag>/<asset>` for both files.
     Select exactly the checksum line for this asset and run `sha256sum -c` on Linux or
     `shasum -a 256 -c` on macOS. Stop if verification fails or the checksum line is missing or
     duplicated. Extract only after verification; use the extracted `baton` for setup, keeping
     the temporary directory until setup has copied the executable.

2. Run `baton setup --plan --json` using the located/built/downloaded binary. The result is an
   ordered array of pending steps with `id`, `description`, `commands`, `needs_user` and
   `needs_consent`. `sync` and `selftest` are offered on each setup to refresh current state.

3. Ask the user **once** for the pending consent steps, listing their descriptions and exact
   commands. For `agy-permissions`, explain that only `read_file(<root>/)` and
   `write_file(<root>/)` are added to `~/.gemini/antigravity-cli/settings.json`, where `<root>`
   is the resolved `$BATON_AGY_WT_DIR`, or `${TMPDIR:-/tmp}/baton-wt`. The edit preserves other
   keys and their order and saves `settings.json.baton-bak`. Respect declined steps.

4. Run `baton setup --apply <approved-ids-comma-separated> --yes` for the approved steps,
   preserving plan order. Include `selftest` after the prerequisites are ready. `--yes` conveys
   the user's consent; it does not authorize doing login on their behalf. If installers put
   `codex` or `agy` outside the current PATH, locate the installed executables and set
   `BATON_CODEX` / `BATON_AGY` (or update this shell's PATH) before continuing.

5. For pending login steps, tell the user to type `! codex login --device-auth` or `! agy` in
   the Claude Code prompt and sign in themselves. Never launch login commands for the user.
   If login is needed before sync/self-test can succeed, do the approved install/configuration
   steps first, wait for the user to complete login, re-plan, and continue the already approved
   remaining steps. Ask again only if the new plan introduces additional consent steps.

6. Finish with `baton usage`. Report any failed or declined steps plainly.
