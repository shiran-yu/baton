use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "baton-usage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::create_dir_all(dir.join("store")).unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        let f = Self { dir };
        let init = json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"baton","version":env!("CARGO_PKG_VERSION")}}});
        let read = json!({"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}});
        f.script(
            "codex",
            &format!(
                r#"[ "$1" = app-server ] || exit 1
echo codex >> "$BATON_HOME/calls"
IFS= read -r line
[ "$line" = '{init}' ] || exit 2
echo '{{"method":"notification"}}'
echo '{{"id":1,"result":{{}}}}'
IFS= read -r line
[ "$line" = '{{"method":"initialized"}}' ] || exit 3
IFS= read -r line
[ "$line" = '{read}' ] || exit 4
echo '{{"method":"notification"}}'
/bin/cat "$HOME/codex.json"
echo
IFS= read -r line
"#
            ),
        );
        f.script(
            "agy",
            r#"[ "$*" = '-p /usage --output-format json' ] || exit 1
if IFS= read -r line; then exit 2; fi
echo agy >> "$BATON_HOME/calls"
/bin/cat "$HOME/agy.json"
"#,
        );
        fs::write(
            f.dir.join("codex.json"),
            serde_json::from_str::<Value>(include_str!("fixtures/usage/codex-ratelimits.json"))
                .unwrap()
                .to_string(),
        )
        .unwrap();
        fs::write(
            f.dir.join("agy.json"),
            include_str!("fixtures/usage/agy-usage.json"),
        )
        .unwrap();
        fs::write(
            f.dir.join("store/account.json"),
            include_str!("fixtures/usage/claude-usage-store.json"),
        )
        .unwrap();
        f
    }
    fn script(&self, name: &str, text: &str) {
        let path = self.dir.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{text}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
        c.env_clear()
            .env("HOME", &self.dir)
            .env("PATH", self.dir.join("bin"))
            .env("BATON_HOME", self.dir.join("state"))
            .env("BATON_CODEX", self.dir.join("bin/codex"))
            .env("BATON_AGY", self.dir.join("bin/agy"))
            .env("BATON_CLAUDE_STORE", self.dir.join("store"))
            .env("CODEX_HOME", self.dir.join("codex-home"));
        c
    }
    fn call(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn usage(&self, refresh: bool) -> Value {
        let args = if refresh {
            vec!["usage", "--json", "--refresh"]
        } else {
            vec!["usage", "--json"]
        };
        serde_json::from_slice(&self.call(&args).stdout).unwrap()
    }
    fn input(&self, text: &str) -> Output {
        let mut child = self
            .command()
            .args(["statusline", "subagent"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        output
    }
    fn task(&self, id: &str, agent: &str, started: u64) {
        let path = self.dir.join("state/tasks").join(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("status.json"), json!({"task":id,"agent":agent,"started":started,"effort":"medium","files":3,"commands":9,"tests":"12/14","tokens":{"input":78000,"output":2000}}).to_string()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn source<'a>(snapshot: &'a Value, name: &str) -> &'a Value {
    snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent"] == name)
        .unwrap()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn fixtures_exchange_normalization_cache_and_refresh() {
    let f = Fixture::new();
    let first = f.usage(false);
    assert_eq!(first["agents"].as_array().unwrap().len(), 3);
    let cx = source(&first, "codex");
    assert_eq!(cx["plan_type"], "plus");
    assert_eq!(cx["ordinary_usage_allowed"], true);
    assert_eq!(cx["windows"][0]["used"], 0.07);
    assert_eq!(cx["windows"][0]["window"], "5h");
    assert_eq!(cx["windows"][1]["window"], "weekly");
    assert_eq!(cx["windows"][1]["window_secs"], 604800);
    let ag = source(&first, "agy");
    assert!((ag["windows"][0]["used"].as_f64().unwrap() - 0.4697504043579102).abs() < 1e-12);
    assert_eq!(ag["windows"][0]["window"], "gemini-weekly");
    assert_eq!(ag["windows"][0]["resets_at"], 1793019034_u64);
    assert_eq!(ag["windows"][1]["used"], 0.0);
    let cc = source(&first, "claude");
    assert_eq!(cc["fetched_at"], 1792418477_u64);
    assert_eq!(cc["windows"][0]["used"], 0.36);
    assert_eq!(cc["windows"][1]["window"], "7d");
    for s in first["agents"].as_array().unwrap() {
        assert!(s["error"].is_null(), "{s}");
        for w in s["windows"].as_array().unwrap() {
            let elapsed = (1.0
                - (w["resets_at"].as_f64().unwrap() - first["fetched_at"].as_f64().unwrap())
                    / w["window_secs"].as_f64().unwrap())
            .clamp(0.01, 1.0);
            assert!(
                (w["pace"].as_f64().unwrap() - w["used"].as_f64().unwrap() / elapsed).abs() < 0.01
            );
            assert_eq!(w["agent"], s["agent"]);
        }
    }
    let calls = fs::read_to_string(f.dir.join("state/calls")).unwrap();
    assert_eq!(calls.lines().count(), 2);
    f.script("codex", "exit 1");
    f.script("agy", "exit 1");
    assert_eq!(f.usage(false), first);
    assert_eq!(
        fs::read_to_string(f.dir.join("state/calls")).unwrap(),
        calls
    );
    let refreshed = f.usage(true);
    assert!(source(&refreshed, "codex")["error"].is_string());
    assert!(source(&refreshed, "agy")["error"].is_string());
    assert_eq!(
        source(&refreshed, "claude")["windows"][0]["used"],
        cc["windows"][0]["used"]
    );
    // An expired cache must also refetch without --refresh.
    let mut expired = first.clone();
    expired["fetched_at"] = json!(now() - 300);
    fs::write(f.dir.join("state/usage.json"), expired.to_string()).unwrap();
    assert!(source(&f.usage(false), "agy")["error"].is_string());
}

#[test]
fn newest_rollout_last_rate_limits_and_failure_isolation() {
    let f = Fixture::new();
    f.script("codex", "exit 1");
    f.script("agy", "exit 1");
    let dir = f.dir.join("codex-home/sessions/2026/10/19");
    fs::create_dir_all(&dir).unwrap();
    let older = dir.join("rollout-old.jsonl");
    fs::write(&older, "{\"rate_limits\":{}}\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&older)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    fs::write(dir.join("rollout-new.jsonl"), concat!(
        "{\"payload\":{\"rate_limits\":{\"primary\":{\"used_percent\":1,\"window_minutes\":300,\"resets_at\":1792432873}}}}\n",
        "{\"payload\":{\"rate_limits\":{\"plan_type\":\"plus\",\"primary\":{\"used_percent\":14,\"window_minutes\":300,\"resets_at\":1792432873},\"secondary\":{\"used_percent\":3,\"window_minutes\":10080,\"resets_at\":1792957281}}}}\n",
        "{\"payload\":{\"type\":\"other\"}}\n")).unwrap();
    let v = f.usage(true);
    assert_eq!(source(&v, "codex")["stale"], true);
    assert_eq!(source(&v, "codex")["windows"][0]["used"], 0.14);
    assert!(source(&v, "agy")["error"].is_string());
    assert_eq!(source(&v, "claude")["windows"][0]["used"], 0.36);
}

#[test]
fn claude_selects_seen_at_without_disclosing_secrets() {
    let f = Fixture::new();
    let mut account: Value =
        serde_json::from_str(include_str!("fixtures/usage/claude-usage-store.json")).unwrap();
    account["seenAt"] = json!(9999999999_u64);
    account["usage"]["fh"] = json!(42);
    account["accessToken"] = json!("NEVER-PRINT-THIS");
    account["organizationName"] = json!("PRIVATE-ORG");
    fs::write(f.dir.join("store/new.json"), account.to_string()).unwrap();
    fs::write(f.dir.join("store/broken.json"), "NEVER-PRINT-THIS").unwrap();
    let v = f.usage(true);
    assert_eq!(source(&v, "claude")["windows"][0]["used"], 0.42);
    let output = f.call(&["usage"]);
    for bytes in [
        output.stdout,
        fs::read(f.dir.join("state/usage.json")).unwrap(),
    ] {
        let text = String::from_utf8(bytes).unwrap();
        for secret in [
            "NEVER-PRINT-THIS",
            "PRIVATE-ORG",
            "accessToken",
            "organizationName",
        ] {
            assert!(!text.contains(secret));
        }
    }
    account["usage"]["fh"] = json!("NEVER-PRINT-THIS");
    fs::write(f.dir.join("store/new.json"), account.to_string()).unwrap();
    let v = f.usage(true);
    assert_eq!(source(&v, "claude")["error"], "invalid quota data");
    assert!(!v.to_string().contains("NEVER-PRINT-THIS"));
}

#[test]
fn statusline_cache_only_and_malformed_cache() {
    let f = Fixture::new();
    assert!(f.call(&["statusline", "line"]).stdout.is_empty());
    assert!(!f.dir.join("state/calls").exists());
    f.usage(true);
    f.script("codex", "exit 99");
    f.script("agy", "exit 99");
    let out = f.call(&["statusline", "line"]);
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "CX 5h 7% wk 3% · AG gem 53% 3p 100% · CC 5h 36% 7d 7%\n"
    );
    assert_eq!(
        fs::read_to_string(f.dir.join("state/calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    fs::write(f.dir.join("state/usage.json"), "malformed").unwrap();
    assert!(f.call(&["statusline", "line"]).stdout.is_empty());
}

#[test]
fn subagent_explicit_time_matching_claims_and_ignored_rows() {
    let f = Fixture::new();
    f.usage(true);
    let id = "t-20261019-120000-abcd";
    let ag = "t-20261019-120001-0001";
    let cx = "t-20261019-120002-0002";
    f.task(id, "codex", 1000);
    f.task(ag, "agy", 1001);
    f.task(cx, "codex", 1002);
    let input = json!({"tasks":[
        {"id":"ignored","agentType":"other","description":id},
        {"id":"time","agentType":"codex","startTime":1000000},
        {"id":"label","agentType":"baton:codex","label":format!("Run {id}")},
        {"id":"agy","agentType":"agy","startTime":1000000},
        {"id":"duplicate","agentType":"baton:agy","startTime":1000000},
        {"id":"old","agentType":"codex","startTime":2000000}
    ]});
    let out = f.input(&input.to_string());
    let rows: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], "time");
    assert_eq!(rows[1]["id"], "label");
    assert_eq!(rows[2]["id"], "agy");
    assert_eq!(
        rows[0]["content"],
        "codex · medium · 3 files · 9 cmds · tests 12/14 · 80k tok · 5h 7% · wk 3%"
    );
    let description = json!({"tasks":[{"id":"desc","agentType":"baton:codex","description":format!("baton {id} now")} ]});
    assert!(!f.input(&description.to_string()).stdout.is_empty());
    for bad in [
        "{",
        "{}",
        "[]",
        "{\"tasks\":{}}",
        "{\"tasks\":[{\"agentType\":\"codex\"}]}",
    ] {
        assert!(f.input(bad).stdout.is_empty());
    }
    // Even a late malformed row must not produce a partial response.
    assert!(f.input(&json!({"tasks":[{"id":"ok","agentType":"codex","description":id},{"agentType":"codex"}]}).to_string()).stdout.is_empty());
}

#[test]
fn subagent_start_time_boundaries_recent_limit_and_unknown_parts() {
    let f = Fixture::new();
    f.task("t-20261019-120000-abcd", "codex", 1000);
    for (start, matched) in [
        (1005000, true),
        (1005001, false),
        (400000, true),
        (399999, false),
    ] {
        let input = json!({"tasks":[{"id":"row","agentType":"codex","startTime":start}]});
        assert_eq!(
            !f.input(&input.to_string()).stdout.is_empty(),
            matched,
            "start {start}"
        );
    }
    // The handler inspects at most the newest 20 candidates.
    for n in 0..20 {
        f.task(&format!("t-20261019-130000-{n:04x}"), "agy", 1000);
    }
    assert!(f
        .input(&json!({"tasks":[{"id":"row","agentType":"codex","startTime":1000000}]}).to_string())
        .stdout
        .is_empty());
    let id = "t-20261019-140000-abcd";
    f.task(id, "codex", 1000);
    fs::write(
        f.dir.join("state/tasks").join(id).join("status.json"),
        json!({"task":id,"agent":"codex"}).to_string(),
    )
    .unwrap();
    let out =
        f.input(&json!({"tasks":[{"id":"row","agentType":"codex","description":id}]}).to_string());
    let row: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(row["content"], "codex");
}

#[test]
fn quick_doctor_is_silent_or_names_missing_without_spawning() {
    let f = Fixture::new();
    f.script("codex", "echo spawned > \"$HOME/spawned\"");
    f.script("agy", "echo spawned > \"$HOME/spawned\"");
    assert!(f.call(&["doctor", "--quick"]).stdout.is_empty());
    assert!(!f.dir.join("spawned").exists());
    fs::remove_file(f.dir.join("bin/agy")).unwrap();
    let out = f
        .command()
        .env_remove("BATON_AGY")
        .args(["doctor", "--quick"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "baton doctor: missing agy on PATH\n"
    );
    fs::write(f.dir.join("not-a-dir"), "").unwrap();
    let out = f
        .command()
        .env("BATON_HOME", f.dir.join("not-a-dir/child"))
        .args(["doctor", "--quick"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("writable BATON_HOME"));
}

#[test]
fn doctor_scan_nested_help_skips_help_and_stops_at_depth_three() {
    let f = Fixture::new();
    for agent in ["codex", "agy", "claude"] {
        f.script(agent, r#"case "$*" in
--version) echo fake-version ;;
--help) echo 'Commands:'; echo '  first  First command'; echo '    Wrapped description'; echo '  help   Show help'; echo '  Options:'; echo '  --flag x' ;;
'first --help') echo 'Available subcommands:'; echo '  nested  Nested command' ;;
'first nested --help') echo 'Commands:'; echo '  third Third level' ;;
'first nested third --help') echo 'Commands:'; echo '  fourth Too deep' ;;
*) exit 1 ;;
esac
"#);
    }
    let dir = f.dir.join("scan");
    let out = f
        .command()
        .args(["doctor", "--scan", "--out"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let versions = fs::read_to_string(dir.join("versions.txt")).unwrap();
    for agent in ["codex", "agy", "claude"] {
        assert!(versions.contains(&format!("{agent}: fake-version")));
        for tail in ["", "_first", "_first_nested", "_first_nested_third"] {
            assert!(dir.join(agent).join(format!("{agent}{tail}.txt")).exists());
        }
        assert_eq!(fs::read_dir(dir.join(agent)).unwrap().count(), 4);
    }
}

#[test]
fn full_doctor_auth_presence_hooks_versions_and_usage() {
    let f = Fixture::new();
    // Versions and usage share fake executables; no installed user CLIs are reachable.
    for (agent, fixture) in [("codex", "codex.json"), ("agy", "agy.json")] {
        let old = fs::read_to_string(f.dir.join("bin").join(agent)).unwrap();
        f.script(
            agent,
            &format!(
                "if [ \"$1\" = --version ]; then echo fake-{agent}; exit 0; fi\n{}",
                old.strip_prefix("#!/bin/sh\n").unwrap()
            ),
        );
        assert!(f.dir.join(fixture).exists());
    }
    fs::create_dir_all(f.dir.join(".codex")).unwrap();
    fs::write(
        f.dir.join(".codex/auth.json"),
        "do-not-read-auth-invalid-json",
    )
    .unwrap();
    fs::create_dir_all(f.dir.join(".gemini/config")).unwrap();
    fs::write(
        f.dir.join(".gemini/config/hooks.json"),
        json!({"test-hook":{"enabled":true,"PreInvocation":[{"command":"secret-command"}]}})
            .to_string(),
    )
    .unwrap();
    let text = String::from_utf8(f.call(&["doctor"]).stdout).unwrap();
    for expected in [
        concat!("baton: ", env!("CARGO_PKG_VERSION")),
        "fake-codex",
        "fake-agy",
        "claude: not installed",
        "auth.json: present",
        "test-hook",
        "run on every worker call",
        "plus",
        "gemini",
        "36% used",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains("secret-command"));
    assert!(!text.contains("do-not-read-auth"));
}

#[test]
fn exhausted_source_survives_cache_expiry_until_explicit_refresh() {
    let f = Fixture::new();
    let mut snapshot = f.usage(true);
    snapshot["fetched_at"] = json!(0);
    let codex = snapshot["agents"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["agent"] == "codex")
        .unwrap();
    codex["exhausted"] = json!(true);
    codex["error"] = json!("usage limit reached");
    fs::write(f.dir.join("state/usage.json"), snapshot.to_string()).unwrap();
    let cached = f.usage(false);
    assert_eq!(source(&cached, "codex")["error"], "usage limit reached");
    assert_eq!(source(&cached, "codex")["exhausted"], true);
    let refreshed = f.usage(true);
    assert!(source(&refreshed, "codex")["error"].is_null());
    assert!(source(&refreshed, "codex")["exhausted"].is_null());
}
