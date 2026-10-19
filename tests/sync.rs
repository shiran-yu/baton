use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "baton-sync-{}-{} with spaces",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        let f = Self {
            dir: fs::canonicalize(dir).unwrap(),
        };
        f.script(
            "codex",
            r#"
printf '%s\n' "codex $*" >> "$BATON_HOME/calls"
case "$*" in
  'plugin marketplace list') /bin/cat "$BATON_HOME/marketplaces.txt" ;;
  'plugin list --json') /bin/cat "$BATON_HOME/codex-list.json" ;;
  'plugin marketplace add '*|'plugin add '*) ;;
  *) exit 9 ;;
esac
"#,
        );
        f.script("agy", r#"
printf '%s\n' "agy $*" >> "$BATON_HOME/calls"
case "$*" in
  'plugin validate --json '*)
    [ -f "$4/plugin.json" ] || exit 7
    if [ "$FAIL_VALIDATE" = 1 ]; then echo '{"valid":false,"errors":["bad plugin"]}'; else echo '{"valid":true}'; fi ;;
  'plugin install '*) [ "$FAIL_INSTALL" != 1 ] || exit 8 ;;
  *) exit 9 ;;
esac
"#);
        f.write_json("state/codex-list.json", &json!([]));
        f.write("state/marketplaces.txt", "No marketplaces installed\n");
        f
    }
    fn write(&self, path: &str, content: &str) {
        let path = self.dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fn write_json(&self, path: &str, content: &Value) {
        self.write(path, &content.to_string());
    }
    fn script(&self, name: &str, text: &str) -> PathBuf {
        self.write(&format!("bin/{name}"), &format!("#!/bin/sh\n{text}"));
        let path = self.dir.join("bin").join(name);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_baton"));
        cmd.env_clear()
            .env("HOME", &self.dir)
            .env("PATH", self.dir.join("bin"))
            .env("BATON_HOME", self.dir.join("state"))
            .env("BATON_CLAUDE_PLUGINS", self.dir.join("claude"))
            .env("BATON_CODEX", self.dir.join("bin/codex"))
            .env("BATON_AGY", self.dir.join("bin/agy"));
        cmd
    }
    fn call(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn read_json(&self, path: &str) -> Value {
        serde_json::from_slice(&fs::read(self.dir.join(path)).unwrap()).unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.dir.join("state/calls")).unwrap_or_default()
    }
    fn inventory(&self) {
        self.write_json("claude/installed_plugins.json", &json!({"version":2,"plugins":{
            "alpha@my-develop-plugins":[{"installPath":self.dir.join("sources/alpha"),"version":"1.2.3"},{"installPath":"/must-not-read","version":"bad"}],
            "trade@trading-plugins":[{"installPath":self.dir.join("sources/trade"),"version":"2"}],
            "catalog@my-develop-plugins":[{"installPath":self.dir.join("sources/catalog"),"version":"3"}]
        }}));
        self.write_json("claude/known_marketplaces.json", &json!({
            "my-develop-plugins":{"installLocation":self.dir.join("market"),"source":{"source":"github","repo":"never/fetch"}},
            "trading-plugins":{"installLocation":self.dir.join("trading-market"),"source":{"source":"directory"}}
        }));
        self.write_json("market/.agents/plugins/marketplace.json", &json!({"name":"own-codex","plugins":[{"name":"catalog","source":{"source":"local","path":"./plugins/catalog"}}]}));
        for name in ["alpha", "trade", "catalog"] {
            self.write_json(&format!("sources/{name}/.claude-plugin/plugin.json"), &json!({"name":name,"description":"Fixture plugin","version":"manifest-version","author":{"name":"discard"}}));
        }
        self.write(
            "sources/alpha/skills/existing/SKILL.md",
            "---\nname: existing\ndescription: Existing skill\n---\nUnchanged.\n",
        );
        self.write(
            "sources/alpha/commands/check.md",
            "---\ndescription: Check things\n---\nRun the checks.\n",
        );
        self.write("sources/alpha/commands/plain.md", "Run plain checks.\n");
        self.write("sources/alpha/agents/reviewer.md", "---\nname: reviewer\ndescription: Review things\ntools: Read, Write, Edit, Grep, Glob, LS, Bash, WebFetch, WebSearch, Task, Unknown\nmodel: sonnet\nmaxTurns: 5\ncolor: red\neffort: high\nisolation: worktree\nbackground: true\ndisallowedTools:\n  - Task\n---\nReview carefully.\n");
        self.write_json("sources/alpha/.mcp.json", &json!({"mcpServers":{
            "remote":{"url":"https://example.invalid/mcp","headers":{"Authorization":"${TOKEN}"}},
            "local":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/tool","args":["${CLAUDE_PLUGIN_ROOT}/asset"],"env":{"ROOT":"${CLAUDE_PLUGIN_ROOT}"}}
        }}));
        self.write_json("sources/alpha/hooks/hooks.json", &json!({"modules":["ignored"],"hooks":{
            "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/bin/sh \"${CLAUDE_PLUGIN_ROOT}/hooks/guard.sh\"","timeout":9}]},
                          {"matcher":"Edit|Write","hooks":[{"type":"command","command":"/bin/sh \"$CLAUDE_PLUGIN_ROOT/hooks/guard.sh\""}]},
                          {"matcher":"Unknown","hooks":[{"type":"command","command":"true"}]},
                          {"matcher":"*","hooks":[{"type":"command","command":"true"}]}],
            "PostToolUse":[{"hooks":[{"type":"command","command":"true"}]}],
            "SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"echo context"}]}],
            "UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo prompt"}]}],
            "Stop":[{"hooks":[{"type":"command","command":"echo stopped"}]}],
            "Notification":[{"hooks":[{"type":"command","command":"false"}]}]
        }}));
        self.write(
            "sources/alpha/hooks/guard.sh",
            include_str!("fixtures/claude-guard.sh"),
        );
        self.write("sources/alpha/bin/tool", "#!/bin/sh\necho tool\n");
        fs::set_permissions(
            self.dir.join("sources/alpha/bin/tool"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fn hook(&self, host: &str, event: &str, command: &[&str], input: &Value) -> Output {
        let mut child = self
            .command()
            .args(["hook-adapt", "--from", host, "--event", event, "--"])
            .args(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn parsed(output: &Output) -> Value {
    success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}
fn agy_tool(command: &str) -> Value {
    json!({"conversationId":"session-1","workspacePaths":["/workspace", "/other"],"transcriptPath":"/transcript","toolCall":{"name":"run_command","args":{"CommandLine":command}}})
}

#[test]
fn sync_dry_run_generates_native_agy_and_plans_codex_without_installing() {
    let f = Fixture::new();
    f.inventory();
    let out = f.call(&["sync", "--dry-run"]);
    success(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("alpha codex: skipped (no codex manifest)"));
    assert!(text.contains("trade agy: skipped (trading guards"));
    assert!(text.contains("'plugin' 'marketplace' 'add'"));
    assert!(text.contains("'plugin' 'add' 'catalog@own-codex'"));
    let calls = f.calls();
    assert!(calls.contains("codex plugin marketplace list\n"));
    assert!(calls.contains("codex plugin list --json\n"));
    assert!(!calls.contains("codex plugin add"));
    assert!(!calls.contains("codex plugin marketplace add"));
    assert!(!calls.contains("agy plugin install"));
    assert_eq!(calls.matches("agy plugin validate").count(), 2);
    assert!(!f.dir.join("state/agy-plugins/trade").exists());
    assert_eq!(
        f.read_json("state/agy-plugins/alpha/plugin.json"),
        json!({"name":"alpha","description":"Fixture plugin","version":"1.2.3"})
    );
    let agent =
        fs::read_to_string(f.dir.join("state/agy-plugins/alpha/agents/reviewer.md")).unwrap();
    for tool in [
        "view_file",
        "write_to_file",
        "replace_file_content",
        "multi_replace_file_content",
        "grep_search",
        "find_by_name",
        "list_dir",
        "run_command",
        "read_url_content",
        "search_web",
        "invoke_subagent",
    ] {
        assert!(agent.contains(&format!("  - {tool}\n")), "{agent}");
    }
    for key in [
        "maxTurns",
        "color",
        "effort",
        "isolation",
        "background",
        "disallowedTools",
        "Unknown",
    ] {
        assert!(!agent.contains(key), "{agent}");
    }
    assert!(agent.contains("model: inherit\n"));
    assert!(agent.ends_with("Review carefully.\n"));
    assert_eq!(
        fs::read(f.dir.join("sources/alpha/skills/existing/SKILL.md")).unwrap(),
        fs::read(
            f.dir
                .join("state/agy-plugins/alpha/skills/existing/SKILL.md")
        )
        .unwrap()
    );
    for cmd in ["check", "plain"] {
        assert!(fs::read_to_string(f.dir.join(format!(
            "state/agy-plugins/alpha/skills/alpha-{cmd}/SKILL.md"
        )))
        .unwrap()
        .starts_with(&format!("---\nname: alpha-{cmd}\n")));
    }
    let mcp = f.read_json("state/agy-plugins/alpha/mcp_config.json");
    assert_eq!(
        mcp["mcpServers"]["remote"]["serverUrl"],
        "https://example.invalid/mcp"
    );
    assert!(mcp["mcpServers"]["remote"].get("url").is_none());
    assert_eq!(
        mcp["mcpServers"]["local"]["command"],
        f.dir
            .join("sources/alpha/bin/tool")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        mcp["mcpServers"]["remote"]["headers"]["Authorization"],
        "${TOKEN}"
    );
    let hooks = f.read_json("state/agy-plugins/alpha/hooks.json");
    assert_eq!(hooks.as_object().unwrap().len(), 1);
    let hooks = &hooks["alpha"];
    assert_eq!(hooks.as_object().unwrap().len(), 5);
    assert_eq!(hooks["PreToolUse"][0]["matcher"], "run_command");
    assert_eq!(
        hooks["PreToolUse"][1]["matcher"],
        "write_to_file|replace_file_content|multi_replace_file_content"
    );
    assert_eq!(hooks["PreToolUse"][2]["matcher"], "*");
    assert_eq!(hooks["PostToolUse"][0]["matcher"], "");
    assert_eq!(hooks["PreToolUse"][0]["hooks"][0]["timeout"], 9);
    for event in ["SessionStart", "PreInvocation", "Stop"] {
        assert_eq!(hooks[event][0]["type"], "command");
        assert!(hooks[event][0].get("hooks").is_none());
    }
    let command = hooks["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("hook-adapt --from agy --event PreToolUse --"));
    assert!(!command.contains("CLAUDE_PLUGIN_ROOT"));
    assert!(command.contains(f.dir.join("sources/alpha").to_str().unwrap()));
    assert_ne!(
        fs::metadata(f.dir.join("state/agy-plugins/alpha/bin/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    // Execute the generated wrapper against the Claude guard fixture.
    let child = f.command();
    // Use the fixture environment but run the hook shell, as agy would.
    let mut shell = Command::new("/bin/sh");
    shell.envs(child.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
    let mut child = shell
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(agy_tool("rm -rf /workspace").to_string().as_bytes())
        .unwrap();
    assert_eq!(
        parsed(&child.wait_with_output().unwrap())["decision"],
        "deny"
    );
    assert_eq!(
        f.read_json("state/agy-plugins/alpha/.baton-sync.json")["installed"],
        false
    );
    assert!(!f.dir.join("sources/alpha/plugin.json").exists());
}

#[test]
fn sync_installs_once_rebuilds_on_version_change_and_retries_failed_install() {
    let f = Fixture::new();
    f.inventory();
    success(&f.call(&["sync"]));
    let calls = f.calls();
    assert_eq!(calls.matches("codex plugin marketplace add").count(), 1);
    assert_eq!(
        calls.matches("codex plugin add catalog@own-codex").count(),
        1
    );
    assert_eq!(calls.matches("agy plugin install").count(), 2);
    f.write(
        "state/marketplaces.txt",
        &format!(
            "Name  Source\nown-codex  {}\n",
            f.dir.join("market").display()
        ),
    );
    f.write_json(
        "state/codex-list.json",
        &json!({"plugins":[{"name":"catalog", "marketplaceName":"own-codex", "installed":true}]}),
    );
    f.write("state/calls", "");
    let second = f.call(&["sync"]);
    success(&second);
    let text = String::from_utf8(second.stdout).unwrap();
    assert!(text.contains("catalog codex: up to date"));
    assert!(text.contains("alpha agy: up to date"));
    assert!(!f.calls().contains("plugin install"));
    assert!(!f.calls().contains("plugin add"));
    assert!(!f.calls().contains("marketplace add"));
    let mut inventory = f.read_json("claude/installed_plugins.json");
    inventory["plugins"]["alpha@my-develop-plugins"][0]["version"] = json!("next");
    f.write_json("claude/installed_plugins.json", &inventory);
    f.write("state/agy-plugins/alpha/stale", "delete on rebuild");
    let failed = f
        .command()
        .args(["sync", "--to", "agy", "--only", "alpha"])
        .env("FAIL_INSTALL", "1")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!f
        .dir
        .join("state/agy-plugins/alpha/.baton-sync.json")
        .exists());
    success(&f.call(&["sync", "--to", "agy", "--only", "alpha"]));
    assert_eq!(
        f.read_json("state/agy-plugins/alpha/.baton-sync.json")["version"],
        "next"
    );
    assert!(!f.dir.join("state/agy-plugins/alpha/stale").exists());
}

#[test]
fn sync_filters_config_trading_and_validation_errors() {
    let f = Fixture::new();
    f.inventory();
    success(&f.call(&[
        "sync",
        "--dry-run",
        "--to",
        "agy",
        "--only",
        "trade",
        "--include-trading",
    ]));
    assert!(f.dir.join("state/agy-plugins/trade/plugin.json").exists());
    assert!(!f.dir.join("state/agy-plugins/alpha").exists());
    assert!(!f.calls().contains("codex"));
    f.write_json(
        "state/sync.json",
        &json!({"own_marketplaces":["my-develop-plugins"],"exclude":["alpha"]}),
    );
    f.write("state/calls", "");
    let out = f.call(&["sync", "--to", "agy"]);
    success(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("alpha agy: skipped (excluded)"));
    assert!(text.contains("trade agy: skipped (not an own marketplace)"));
    f.write_json("state/sync.json", &json!({"exclude":[]}));
    let out = f
        .command()
        .args(["sync", "--to", "agy", "--only", "alpha"])
        .env("FAIL_VALIDATE", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("bad plugin"));
    assert!(!f
        .dir
        .join("state/agy-plugins/alpha/.baton-sync.json")
        .exists());
}

#[test]
fn codex_available_entries_are_installed_but_other_marketplace_does_not_count() {
    let f = Fixture::new();
    f.inventory();
    f.write(
        "state/marketplaces.txt",
        "[{\"name\":\"own-codex\",\"source\":\"local\"}]",
    );
    for plugins in [
        json!({"plugins":[{"name":"catalog", "installed":false}]}),
        json!([{"name":"catalog", "marketplace":"another"}]),
    ] {
        f.write_json("state/codex-list.json", &plugins);
        f.write("state/calls", "");
        success(&f.call(&["sync", "--to", "codex", "--only", "catalog"]));
        assert!(f.calls().contains("codex plugin add catalog@own-codex"));
        assert!(!f.calls().contains("marketplace add"));
    }
}

#[test]
fn codex_registers_own_marketplaces_even_without_installed_plugins() {
    let f = Fixture::new();
    f.inventory();
    let mut marketplaces = f.read_json("claude/known_marketplaces.json");
    marketplaces["baton"] =
        json!({"installLocation": f.dir.join("unused-market"), "source": {"source":"directory"}});
    f.write_json("claude/known_marketplaces.json", &marketplaces);
    f.write_json(
        "unused-market/.agents/plugins/marketplace.json",
        &json!({"name":"unused-codex","plugins":[]}),
    );
    success(&f.call(&["sync", "--to", "codex"]));
    assert!(f.calls().contains(&format!(
        "codex plugin marketplace add {}\n",
        f.dir.join("unused-market").display()
    )));
    assert_eq!(
        f.calls().matches("codex plugin marketplace list").count(),
        1
    );
}

#[test]
fn hook_agy_tool_decisions_fail_closed_and_translate_input() {
    let f = Fixture::new();
    for (name, body, expected) in [
        ("allow", "echo '{\"hookSpecificOutput\":{\"permissionDecision\":\"allow\"}}'", "allow"),
        ("deny", "echo '{\"hookSpecificOutput\":{\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"blocked\"}}'", "deny"),
        ("ask", "echo '{\"hookSpecificOutput\":{\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"approval needed\"}}'", "deny"),
        ("crash", "echo guard-crashed >&2; exit 1", "deny"),
        ("signal", "kill -TERM $$", "deny"),
        ("exit2", "echo explicit-block >&2; exit 2", "deny"),
        ("invalid", "echo invalid-json", "deny"),
        ("array", "echo '[]'", "deny"),
        ("invalid-utf8", "printf '{\"x\":\"\\377\"}'", "deny"),
        ("unknown", "echo '{\"hookSpecificOutput\":{\"permissionDecision\":\"surprise\"}}'", "deny"),
    ] {
        let script = f.script(name, &format!("/bin/cat > \"$BATON_HOME/input.json\"\n{body}"));
        let output = parsed(&f.hook("agy", "PreToolUse", &[script.to_str().unwrap()], &agy_tool("echo safe")));
        assert_eq!(output["decision"], expected, "{name}: {output}");
        if expected == "deny" { assert!(output["reason"].as_str().is_some_and(|s| !s.is_empty())); }
        let input = f.read_json("state/input.json");
        assert_eq!(input["session_id"], "session-1");
        assert_eq!(input["cwd"], "/workspace");
        assert_eq!(input["transcript_path"], "/transcript");
        assert_eq!(input["hook_event_name"], "PreToolUse");
        assert_eq!(input["tool_name"], "Bash");
        assert_eq!(input["tool_input"]["command"], "echo safe");
    }
    assert_eq!(
        parsed(&f.hook(
            "agy",
            "PreToolUse",
            &["/does/not/exist"],
            &agy_tool("echo safe")
        ))["decision"],
        "deny"
    );
    let guard = f.script("guard", include_str!("fixtures/claude-guard.sh"));
    for (command, decision) in [("rm -rf /workspace", "deny"), ("cargo test", "allow")] {
        assert_eq!(
            parsed(&f.hook(
                "agy",
                "PreToolUse",
                &[guard.to_str().unwrap()],
                &agy_tool(command)
            ))["decision"],
            decision
        );
    }
}

#[test]
fn hook_tool_mapping_command_aliases_and_plugin_environment() {
    let f = Fixture::new();
    f.inventory();
    let script = f.dir.join("sources/alpha/hooks/guard.sh");
    f.write("sources/alpha/hooks/guard.sh", "/bin/cat > \"$BATON_HOME/input.json\"\nprintf '%s' \"$CLAUDE_PLUGIN_ROOT\" > \"$BATON_HOME/root\"\necho '{}'\n");
    for (tool, expected) in [
        ("view_file", "Read"),
        ("write_to_file", "Write"),
        ("replace_file_content", "Edit"),
        ("multi_replace_file_content", "Edit"),
        ("grep_search", "Grep"),
        ("find_by_name", "Glob"),
        ("list_dir", "LS"),
    ] {
        let mut input = agy_tool("echo safe");
        input["toolCall"]["name"] = json!(tool);
        success(&f.hook(
            "agy",
            "PreToolUse",
            &["/bin/sh", script.to_str().unwrap()],
            &input,
        ));
        assert_eq!(f.read_json("state/input.json")["tool_name"], expected);
    }
    assert_eq!(
        fs::read_to_string(f.dir.join("state/root")).unwrap(),
        f.dir.join("sources/alpha").to_string_lossy()
    );
    for alias in ["CommandLine", "command", "Command", "cmd"] {
        let mut input = agy_tool("");
        input["toolCall"]["args"] = json!({alias:"echo alias"});
        success(&f.hook(
            "agy",
            "PreToolUse",
            &["/bin/sh", script.to_str().unwrap()],
            &input,
        ));
        assert_eq!(
            f.read_json("state/input.json")["tool_input"]["command"],
            "echo alias"
        );
    }
}

#[test]
fn hook_preinvocation_runs_only_initial_call_and_injects_context() {
    let f = Fixture::new();
    let script = f.script("prompt", "/bin/cat > \"$BATON_HOME/input.json\"\necho '{\"hookSpecificOutput\":{\"additionalContext\":\"context from guard\"}}'");
    for invocation in [1, 0, 1] {
        let output = parsed(&f.hook(
            "agy",
            "UserPromptSubmit",
            &[script.to_str().unwrap()],
            &json!({"invocationNum":invocation}),
        ));
        if invocation == 0 {
            assert_eq!(
                output,
                json!({"injectSteps":[{"ephemeralMessage":"context from guard"}]})
            );
            let input = f.read_json("state/input.json");
            assert_eq!(input["prompt"], "");
            assert_eq!(input["hook_event_name"], "UserPromptSubmit");
            fs::remove_file(f.dir.join("state/input.json")).unwrap();
        } else {
            assert_eq!(output, json!({}));
            assert!(!f.dir.join("state/input.json").exists());
        }
    }
    let plain = f.script("plain", "echo plain-context");
    assert_eq!(
        parsed(&f.hook(
            "agy",
            "SessionStart",
            &[plain.to_str().unwrap()],
            &json!({})
        )),
        json!({"injectSteps":[{"ephemeralMessage":"plain-context"}]})
    );
    let stop = f.script(
        "stop",
        "echo '{\"decision\":\"block\",\"reason\":\"work remains\"}'",
    );
    assert_eq!(
        parsed(&f.hook("agy", "Stop", &[stop.to_str().unwrap()], &json!({}))),
        json!({"decision":"continue","reason":"work remains"})
    );
    assert_eq!(
        parsed(&f.hook(
            "agy",
            "PostToolUse",
            &[plain.to_str().unwrap()],
            &agy_tool("echo safe")
        )),
        json!({})
    );
}

#[test]
fn hook_codex_passes_input_denies_ask_and_preserves_exit_two() {
    let f = Fixture::new();
    let script = f.script("ask", "/bin/cat > \"$BATON_HOME/input.json\"\necho '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"need approval\"}}'");
    let input = json!({"session_id":"codex-session","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf /workspace"},"custom":{"preserved":true}});
    let output = parsed(&f.hook("codex", "PreToolUse", &[script.to_str().unwrap()], &input));
    assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        output["hookSpecificOutput"]["permissionDecisionReason"],
        "need approval"
    );
    assert_eq!(f.read_json("state/input.json"), input);
    let script = f.script("block", "echo block-reason >&2; exit 2");
    let out = f.hook("codex", "PreToolUse", &[script.to_str().unwrap()], &input);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("block-reason"));
    for body in ["exit 1", "echo malformed"] {
        let script = f.script("failure", body);
        assert_eq!(
            parsed(&f.hook("codex", "PreToolUse", &[script.to_str().unwrap()], &input))
                ["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }
    let guard = f.script("guard", include_str!("fixtures/claude-guard.sh"));
    assert_eq!(
        parsed(&f.hook("codex", "PreToolUse", &[guard.to_str().unwrap()], &input))
            ["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
}

#[test]
fn cli_rejects_bad_options_before_creating_state() {
    let f = Fixture::new();
    for args in [
        vec!["sync", "--to", "other"],
        vec!["sync", "--only", "../escape"],
        vec!["sync", "--only", ""],
        vec![
            "hook-adapt",
            "--from",
            "other",
            "--event",
            "PreToolUse",
            "--",
            "true",
        ],
        vec!["hook-adapt", "--from", "agy", "--event", "PreToolUse"],
    ] {
        assert_eq!(f.call(&args).status.code(), Some(2));
    }
    assert!(!Path::new(&f.dir.join("state/calls")).exists());
}
