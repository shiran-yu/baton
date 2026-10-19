use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Machine {
    home: PathBuf,
}
impl Machine {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "baton-setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(home.join("bin")).unwrap();
        Self {
            home: fs::canonicalize(home).unwrap(),
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_baton"));
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("BATON_HOME", self.home.join("state"))
            .env("PATH", self.home.join("bin"))
            .env("TMPDIR", self.home.join("tmp"))
            .env("BATON_CODEX", self.home.join("bin/codex"))
            .env("BATON_AGY", self.home.join("bin/agy"))
            .stdin(Stdio::null());
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn plan(&self) -> Vec<Value> {
        serde_json::from_slice(&self.ok(&["setup", "--plan", "--json"]).stdout).unwrap()
    }
    fn script(&self, name: &str, body: &str) {
        let path = self.home.join("bin").join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' \"{name}:$*\" >> \"$HOME/calls\"\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn agents(&self) {
        self.script("codex", r#"case "$*" in
          'login status') exit 0 ;;
          'plugin marketplace list --json') echo '{"marketplaces":[{"name":"my-develop-plugins-codex"}]}' ;;
          *) exit 9 ;;
        esac"#);
        self.script(
            "agy",
            r#"[ "$*" = '-p /usage --output-format json' ] || exit 9
        if IFS= read -r input; then exit 10; fi
        echo '{"usage":[]}'"#,
        );
    }
    fn settings(&self) -> PathBuf {
        self.home.join(".gemini/antigravity-cli/settings.json")
    }
    fn write_settings(&self, text: &str) {
        fs::create_dir_all(self.settings().parent().unwrap()).unwrap();
        fs::write(self.settings(), text).unwrap();
    }
    fn backup(&self) -> PathBuf {
        self.settings().with_file_name("settings.json.baton-bak")
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.home.join("calls")).unwrap_or_default()
    }
}
impl Drop for Machine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}
fn ids(plan: &[Value]) -> Vec<&str> {
    plan.iter().map(|s| s["id"].as_str().unwrap()).collect()
}

#[test]
fn plan_missing_machine_is_ordered_complete_and_read_only() {
    let m = Machine::new();
    let plan = m.plan();
    assert_eq!(
        ids(&plan),
        [
            "binary",
            "codex-install",
            "agy-install",
            "codex-login",
            "agy-login",
            "agy-permissions",
            "marketplaces",
            "sync",
            "selftest"
        ]
    );
    assert_eq!(
        plan[1]["commands"][0],
        "curl -fsSL https://chatgpt.com/codex/install.sh | sh"
    );
    assert_eq!(
        plan[2]["commands"][0],
        "curl -fsSL https://antigravity.google/cli/install.sh | bash"
    );
    for step in &plan {
        assert!(!step["description"].as_str().unwrap().contains('\n'));
        assert!(!step["commands"].as_array().unwrap().is_empty());
        let login = step["id"].as_str().unwrap().ends_with("-login");
        assert_eq!(step["needs_user"], login);
        assert_eq!(step["needs_consent"], !login && step["id"] != "selftest");
    }
    assert!(!m.home.join("tmp").exists());
    assert!(!m.home.join("state").exists());
    assert!(!m.settings().exists());
    assert!(m.calls().is_empty());
    let plain = m.ok(&["setup"]);
    assert!(String::from_utf8_lossy(&plain.stdout).contains("codex-install"));
    assert!(!m.home.join("state").exists());
}

#[test]
fn plan_present_machine_only_offers_repeatable_sync_and_selftest() {
    let m = Machine::new();
    m.agents();
    m.ok(&["setup", "--apply", "binary,agy-permissions", "--yes"]);
    assert_eq!(ids(&m.plan()), ["sync", "selftest"]);
    let binary = m.home.join("state/bin/baton");
    assert_eq!(
        fs::read(&binary).unwrap(),
        fs::read(env!("CARGO_BIN_EXE_baton")).unwrap()
    );
    assert_eq!(
        fs::read_link(m.home.join(".local/bin/baton")).unwrap(),
        binary
    );
    // Reinstalling from the installed executable must not truncate it or break the symlink.
    let mut installed = m.command();
    let output = installed
        .args(["setup", "--apply", "binary", "--yes"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = Command::new(m.home.join(".local/bin/baton"))
        .env_clear()
        .env("HOME", &m.home)
        .env("BATON_HOME", m.home.join("state"))
        .args(["setup", "--apply", "binary", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn permissions_preserve_keys_order_bytes_backup_and_are_idempotent() {
    let m = Machine::new();
    let original = " \n{\n  \"z-last\": 1e2,\n  \"permissions\": {\"z\":true, \"allow\":[\"read_file(/existing/)\"], \"a\":{\"escape\\\"\": [1,2]}},\n  \"a-first\": false\n}\n";
    m.write_settings(original);
    fs::set_permissions(m.settings(), fs::Permissions::from_mode(0o600)).unwrap();
    m.ok(&["setup", "--apply", "agy-permissions", "--yes"]);
    assert_eq!(fs::read_to_string(m.backup()).unwrap(), original);
    let edited = fs::read_to_string(m.settings()).unwrap();
    let v: Value = serde_json::from_str(&edited).unwrap();
    let root = m.home.join("tmp/baton-wt");
    let root = root.to_string_lossy();
    assert_eq!(
        v["permissions"]["allow"],
        json!([
            "read_file(/existing/)",
            format!("read_file({root}/)"),
            format!("write_file({root}/)")
        ])
    );
    assert!(edited.contains("\"z-last\": 1e2"));
    assert!(edited.find("z-last").unwrap() < edited.find("permissions").unwrap());
    assert!(edited.find("permissions").unwrap() < edited.find("a-first").unwrap());
    assert!(edited.find("\"z\"").unwrap() < edited.find("\"allow\"").unwrap());
    assert!(edited.find("\"allow\"").unwrap() < edited.find("\"a\"").unwrap());
    assert_eq!(v["permissions"]["a"], json!({"escape\"":[1,2]}));
    assert_eq!(
        fs::metadata(m.settings()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let modified = fs::metadata(m.settings()).unwrap().modified().unwrap();
    m.ok(&["setup", "--apply", "agy-permissions", "--yes"]);
    assert_eq!(fs::read_to_string(m.settings()).unwrap(), edited);
    assert_eq!(fs::read_to_string(m.backup()).unwrap(), original);
    assert_eq!(
        fs::metadata(m.settings()).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn permissions_handle_missing_objects_arrays_and_partial_grants() {
    for original in [
        "{}",
        " \n{ }\n",
        r#"{"z":1,"permissions":{},"a":2}"#,
        r#"{"permissions":{"allow":[]}}"#,
    ] {
        let m = Machine::new();
        m.write_settings(original);
        m.ok(&["setup", "--apply", "agy-permissions", "--yes"]);
        let v: Value = serde_json::from_str(&fs::read_to_string(m.settings()).unwrap()).unwrap();
        assert_eq!(v["permissions"]["allow"].as_array().unwrap().len(), 2);
        assert_eq!(fs::read_to_string(m.backup()).unwrap(), original);
    }
    let m = Machine::new();
    let root = m.home.join("tmp/baton-wt");
    m.write_settings(
        &json!({"permissions":{"allow":[format!("read_file({}/)", root.display())]}}).to_string(),
    );
    m.ok(&["setup", "--apply", "agy-permissions", "--yes"]);
    let v: Value = serde_json::from_str(&fs::read_to_string(m.settings()).unwrap()).unwrap();
    assert_eq!(v["permissions"]["allow"].as_array().unwrap().len(), 2);
}

#[test]
fn invalid_settings_fail_without_overwriting_or_backup() {
    for text in [
        "not json",
        "[]",
        r#"{"permissions":null}"#,
        r#"{"permissions":{"allow":"*"}}"#,
    ] {
        let m = Machine::new();
        m.write_settings(text);
        assert!(!m
            .run(&["setup", "--apply", "agy-permissions", "--yes"])
            .status
            .success());
        assert_eq!(fs::read_to_string(m.settings()).unwrap(), text);
        assert!(!m.backup().exists());
    }
}

#[test]
fn permissions_resolve_symlinks_without_creating_the_worktree_root() {
    let m = Machine::new();
    fs::create_dir(m.home.join("real")).unwrap();
    symlink(m.home.join("real"), m.home.join("alias")).unwrap();
    let output = m
        .command()
        .env("BATON_AGY_WT_DIR", m.home.join("alias/nonexistent/../wt"))
        .args(["setup", "--apply", "agy-permissions", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_str(&fs::read_to_string(m.settings()).unwrap()).unwrap();
    let expected = fs::canonicalize(m.home.join("real")).unwrap().join("wt");
    assert_eq!(
        v["permissions"]["allow"][0],
        format!("read_file({}/)", expected.display())
    );
    assert!(!expected.exists());
}

#[test]
fn consent_is_required_for_each_mutating_step_without_tty() {
    for id in [
        "binary",
        "codex-install",
        "agy-install",
        "agy-permissions",
        "marketplaces",
        "sync",
        "selftest,binary",
    ] {
        let m = Machine::new();
        let output = m.run(&["setup", "--apply", id]);
        assert!(!output.status.success(), "{id}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
        assert!(!m.home.join("state").exists());
        assert!(!m.settings().exists());
        assert!(m.calls().is_empty());
    }
}

#[test]
fn login_apply_only_prints_commands_and_never_probes_or_launches() {
    let m = Machine::new();
    m.agents();
    for extra in [vec![], vec!["--yes"]] {
        let mut args = vec!["setup", "--apply", "codex-login,agy-login"];
        args.extend(extra);
        let text = String::from_utf8(m.ok(&args).stdout).unwrap();
        assert!(text.contains("codex login --device-auth"));
        assert!(text.contains("bin/agy"));
        assert_eq!(text.lines().count(), 2);
        assert!(m.calls().is_empty());
    }
}

#[test]
fn failed_or_auth_required_probes_offer_login() {
    let m = Machine::new();
    m.script("codex", "exit 1");
    for body in [
        "exit 1",
        "echo 'Authentication Required'",
        "echo 'authentication required' >&2",
    ] {
        m.script("agy", body);
        let plan = m.plan();
        let ids = ids(&plan);
        assert!(ids.contains(&"codex-login"));
        assert!(ids.contains(&"agy-login"));
        assert!(!ids.contains(&"codex-install"));
        assert!(!ids.contains(&"agy-install"));
    }
    m.script(
        "codex",
        "echo '{\"name\":\"my-develop-plugins-codex-unrelated\"}'",
    );
    assert!(ids(&m.plan()).contains(&"marketplaces"));
}

#[test]
fn explicit_apply_uses_plan_order_and_yes_allows_marketplace_install() {
    let m = Machine::new();
    m.script("codex", r#"case "$*" in
      'plugin marketplace list --json') echo '[]' ;;
      'plugin marketplace add shiran-yu/my-develop-plugins'|'plugin add baton@my-develop-plugins-codex') exit 0 ;;
      *) exit 9 ;;
    esac"#);
    m.ok(&["setup", "--apply", "marketplaces,codex-login", "--yes"]);
    let calls = m.calls();
    assert!(calls.contains("codex:plugin marketplace add shiran-yu/my-develop-plugins\ncodex:plugin add baton@my-develop-plugins-codex\n"));
    assert!(!calls.contains("login"));
    assert!(!m
        .run(&["setup", "--apply", "binary,typo", "--yes"])
        .status
        .success());
    assert!(!m.home.join("state/bin/baton").exists());
}

#[test]
fn setup_rejects_incompatible_or_incomplete_flags() {
    let m = Machine::new();
    for args in [
        vec!["--json"],
        vec!["--yes"],
        vec!["--apply"],
        vec!["--apply", ""],
        vec!["--plan", "--apply", "binary"],
        vec!["--apply", "binary", "--json"],
        vec!["--plan", "--yes"],
    ] {
        let mut command = vec!["setup"];
        command.extend(args);
        assert!(!m.run(&command).status.success());
    }
    assert!(!m.home.join("state").exists());
}

#[test]
fn sync_installs_only_owned_plugins_with_native_manifests() {
    let m = Machine::new();
    m.script("codex", "exit 0");
    m.script("agy", "exit 0");
    let plugin = m.home.join("plugin with 'quotes'");
    fs::create_dir_all(plugin.join(".codex-plugin")).unwrap();
    fs::write(plugin.join(".codex-plugin/plugin.json"), "{}").unwrap();
    fs::write(plugin.join("plugin.json"), "{}").unwrap();
    let registry = m.home.join(".claude/plugins");
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("installed_plugins.json"),
        json!({"plugins": {
            "test@my-develop-plugins":[{"installPath":plugin},{"installPath":plugin}],
            "third-party@other":[{"installPath":plugin}]
        }})
        .to_string(),
    )
    .unwrap();
    fs::write(registry.join("known_marketplaces.json"), json!({
        "my-develop-plugins":{"source":{"source":"github","repo":"shiran-yu/my-develop-plugins"}},
        "other":{"source":{"source":"github","repo":"someone/other"}}
    }).to_string()).unwrap();
    m.ok(&["setup", "--apply", "sync", "--yes"]);
    let calls = m.calls();
    assert!(calls.contains("codex:plugin marketplace add shiran-yu/my-develop-plugins"));
    assert!(calls.contains("codex:plugin add test@my-develop-plugins-codex"));
    assert!(calls.contains(&format!("agy:plugin install {}", plugin.display())));
    assert_eq!(calls.lines().count(), 3);
}

#[test]
fn launcher_reports_setup_skill_and_keeps_hooks_silent() {
    let m = Machine::new();
    let launcher = m.home.join("launcher");
    fs::write(&launcher, include_str!("../plugins/baton/bin/baton")).unwrap();
    for arg in ["doctor", "statusline", "usage"] {
        let output = Command::new("/bin/sh")
            .arg(&launcher)
            .arg(arg)
            .env_clear()
            .env("HOME", &m.home)
            .output()
            .unwrap();
        if arg == "usage" {
            assert!(!output.status.success());
            let text = String::from_utf8(output.stderr).unwrap();
            assert_eq!(text.lines().count(), 1);
            assert!(text.contains("baton:setup"));
        } else {
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn installer_checks_download_failure_and_existing_tools_are_noops() {
    let m = Machine::new();
    m.script("curl", "exit 23");
    m.script("sh", "exit 0");
    let output = m.run(&["setup", "--apply", "codex-install", "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("installer failed"));
    assert!(m
        .calls()
        .contains("curl:-fsSL https://chatgpt.com/codex/install.sh"));
    m.agents();
    fs::write(m.home.join("calls"), "").unwrap();
    m.ok(&["setup", "--apply", "codex-install,agy-install", "--yes"]);
    assert!(m.calls().is_empty());
}

#[test]
fn quoted_executable_and_home_paths_remain_single_arguments() {
    let m = Machine::new();
    let nested = m.home.join("machine with 'quotes' and $() characters");
    fs::create_dir_all(nested.join("bin")).unwrap();
    let m = Machine { home: nested };
    m.script("codex", "echo '[]'");
    m.ok(&[
        "setup",
        "--apply",
        "binary,agy-permissions,marketplaces",
        "--yes",
    ]);
    assert!(m.home.join("state/bin/baton").exists());
    assert!(m
        .calls()
        .contains("codex:plugin add baton@my-develop-plugins-codex"));
    let plan = m.plan();
    assert!(!ids(&plan).contains(&"binary"));
    assert!(!ids(&plan).contains(&"agy-permissions"));
}
