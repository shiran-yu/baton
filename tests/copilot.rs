use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "baton-copilot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for sub in ["bin", "project", "state"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let f = Self {
            dir: fs::canonicalize(dir).unwrap(),
        };
        symlink("/bin/sh", f.dir.join("bin/sh")).unwrap();
        // No installed worker or gh is reachable, even for quota refresh and doctor.
        f.script("codex", "exit 1");
        f.script("agy", "exit 1");
        f.script("unshare", "echo namespace-denied >&2; exit 1");
        f.script(
            "gh",
            r#"[ "$*" = 'api /copilot_internal/user' ] || exit 9
/bin/cat "$HOME/quota.json""#,
        );
        f.script(
            "copilot",
            r#"if [ "$1" = --version ]; then echo copilot-fake-1.0.103; exit 0; fi
printf '%s\n' "$@" > "$HOME/argv"
pwd > "$HOME/cwd"
/usr/bin/env > "$HOME/worker-env"
/bin/cat > "$HOME/prompt"
/bin/cat "$HOME/events"
"#,
        );
        fs::write(
            f.dir.join("quota.json"),
            include_str!("fixtures/usage/copilot-user.json"),
        )
        .unwrap();
        f.events("Findings before the tail. {\"status\":\"done\",\"notes\":\"Found three records.\",\"questions\":[]}", 0);
        fs::write(f.dir.join("project/brief.md"), "## Goal\nFind records in local files.\n## Files\nnone\n## Forbidden\nAll edits.\n## Acceptance\n```sh\ntrue\n```\n").unwrap();
        f
    }
    fn script(&self, name: &str, body: &str) {
        let path = self.dir.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn events(&self, content: &str, exit: i32) {
        let events = [
            json!({"type":"assistant.turn_start"}),
            json!({"type":"tool.execution_start", "data":{"toolName":"view", "toolCallId":"t1"}}),
            json!({"type":"tool.execution_complete", "data":{"toolCallId":"t1"}}),
            json!({"type":"assistant.message", "data":{"content":"Earlier message must be ignored"}}),
            json!({"type":"assistant.message", "data":{"model":"gpt-5.6-luna","content":content,"toolRequests":[]}}),
            json!({"type":"session.usage_checkpoint", "data":{"totalPremiumRequests":2}}),
            json!({"type":"result","exitCode":exit,"usage":{"premiumRequests":1}}),
        ];
        fs::write(
            self.dir.join("events"),
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
        c.env_clear()
            .current_dir(&self.dir)
            .env("HOME", &self.dir)
            .env("PATH", self.dir.join("bin"))
            .env("BATON_HOME", self.dir.join("state"))
            // A relative override must resolve before the cwd becomes the brief folder.
            .env("BATON_COPILOT", "./bin/copilot")
            .env("BATON_COPILOT_NO_NS", "1")
            .env("BATON_CODEX", self.dir.join("bin/codex"))
            .env("BATON_AGY", self.dir.join("bin/agy"));
        c
    }
    fn run_command(&self, id: &str) -> Command {
        let mut c = self.command();
        c.args([
            "run",
            "copilot",
            "--mode",
            "read",
            "--task",
            id,
            "--brief",
            "project/brief.md",
            "--json",
        ]);
        c
    }
    fn result(&self, out: &Output, exit: i32) -> Value {
        assert_eq!(
            out.status.code(),
            Some(exit),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn read_json_tail_command_environment_progress_and_ledger() {
    let f = Fixture::new();
    let out = f
        .run_command("read")
        .args(["--effort", "xhigh"])
        .env("COPILOT_HOME", f.dir.join("copilot-home"))
        .env("GH_TOKEN", "test-gh")
        .env("GITHUB_TOKEN", "test-github")
        .env("COPILOT_GITHUB_TOKEN", "test-copilot")
        .env("COPILOT_AUTO_UPDATE", "true")
        .env("UNRELATED_VARIABLE", "remove-me")
        .env("OPENAI_API_KEY", "remove-me")
        .output()
        .unwrap();
    let r = f.result(&out, 0);
    assert_eq!(r["status"], "done");
    assert_eq!(r["verified"], true);
    assert_eq!(r["notes"], "Found three records.");
    assert_eq!(r["files_changed"], json!([]));
    assert_eq!(r["model"], "auto");
    assert_eq!(r["tokens"]["premium_requests"], 1.0);
    assert_eq!(r["rounds"], 0);
    assert_eq!(r["acceptance"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(f.dir.join("cwd")).unwrap().trim(),
        fs::canonicalize(&f.dir).unwrap().to_str().unwrap()
    );
    let args = fs::read_to_string(f.dir.join("argv")).unwrap();
    let usage = f.dir.join("state/tasks/read/usage.json");
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "--model",
            "auto",
            "--allow-all-tools",
            "--no-ask-user",
            "--no-custom-instructions",
            "--disable-builtin-mcps",
            "--output-format",
            "json",
            "--usage-output-file",
            usage.to_str().unwrap()
        ]
    );
    let prompt = fs::read_to_string(f.dir.join("prompt")).unwrap();
    assert!(prompt.starts_with("## Goal\nFind records"));
    assert!(prompt.contains("You are read-only."));
    assert!(prompt.contains("ONE JSON object"));
    let env = fs::read_to_string(f.dir.join("worker-env")).unwrap();
    for expected in [
        "GH_TOKEN=test-gh",
        "GITHUB_TOKEN=test-github",
        "COPILOT_GITHUB_TOKEN=test-copilot",
        "COPILOT_AUTO_UPDATE=false",
        "BATON_TASK=read",
        "COPILOT_HOME=",
    ] {
        assert!(
            env.lines().any(|l| l.starts_with(expected)),
            "{expected}: {env}"
        );
    }
    for removed in [
        "UNRELATED_VARIABLE",
        "OPENAI_API_KEY",
        "BATON_COPILOT_NO_NS",
        "BATON_HOME",
    ] {
        assert!(!env.contains(removed));
    }
    let ledger: Value =
        serde_json::from_str(&fs::read_to_string(f.dir.join("state/ledger.jsonl")).unwrap())
            .unwrap();
    assert_eq!(ledger["tokens"]["premium_requests"], 1.0);
    let status: Value =
        serde_json::from_slice(&fs::read(f.dir.join("state/tasks/read/status.json")).unwrap())
            .unwrap();
    assert_eq!(status["commands"], 1);
    assert!(f.dir.join("state/tasks/read/events.jsonl").exists());
}

#[test]
fn unstructured_message_falls_back_to_result_exit_code() {
    let f = Fixture::new();
    let message = "Plain answer with {invalid JSON} and no result tail.\nAll records found.";
    for (id, exit, status) in [("ok", 0, "done"), ("bad", 1, "failed")] {
        f.events(message, exit);
        let out = f.run_command(id).output().unwrap();
        let r = f.result(&out, exit);
        assert_eq!(r["status"], status);
        assert_eq!(r["notes"], message);
    }
}

#[test]
fn quota_floor_is_enforced_before_worker_and_can_be_overridden() {
    let f = Fixture::new();
    let mut quota: Value =
        serde_json::from_str(include_str!("fixtures/usage/copilot-user.json")).unwrap();
    quota["quota_snapshots"]["premium_interactions"]["remaining"] = json!(19);
    fs::write(f.dir.join("quota.json"), quota.to_string()).unwrap();
    let out = f.run_command("floor").output().unwrap();
    let r = f.result(&out, 4);
    assert!(r["notes"].as_str().unwrap().contains("below floor 20"));
    assert!(!f.dir.join("argv").exists());
    // A reserve-floor refusal does not exhaust the account or require a refresh.
    let out = f
        .run_command("override")
        .env("BATON_COPILOT_FLOOR", "10")
        .output()
        .unwrap();
    assert_eq!(f.result(&out, 0)["status"], "done");
}

#[test]
fn edit_default_and_review_are_usage_errors_without_processes() {
    let f = Fixture::new();
    for mode in [None, Some("edit")] {
        let mut c = f.command();
        c.args(["run", "copilot", "--brief", "project/brief.md"]);
        if let Some(mode) = mode {
            c.args(["--mode", mode]);
        }
        let out = c.output().unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("copilot runs read briefs only"));
    }
    assert_eq!(
        f.command()
            .args(["review", "task", "--by", "copilot"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(!f.dir.join("argv").exists());
    assert!(!f.dir.join("state/usage.json").exists());
}

#[test]
fn namespace_failure_never_launches_copilot_and_model_overrides_work() {
    let f = Fixture::new();
    let out = f
        .run_command("namespace")
        .env_remove("BATON_COPILOT_NO_NS")
        .output()
        .unwrap();
    assert!(f.result(&out, 1)["notes"]
        .as_str()
        .unwrap()
        .contains("refusing to run writable"));
    assert!(!f.dir.join("argv").exists());
    for (id, flags, model) in [
        ("env-model", vec![], "env-model"),
        ("cli-model", vec!["--model", "cli-model"], "cli-model"),
    ] {
        let out = f
            .run_command(id)
            .env("BATON_COPILOT_MODEL", "env-model")
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(f.result(&out, 0)["model"], model);
        assert!(fs::read_to_string(f.dir.join("argv"))
            .unwrap()
            .starts_with(&format!("--model\n{model}\n")));
    }
}

#[test]
fn route_only_offers_copilot_for_easy_read_briefs_above_floor() {
    let f = Fixture::new();
    // Make Copilot the sole available worker.
    assert!(f
        .command()
        .args(["usage", "--refresh"])
        .output()
        .unwrap()
        .status
        .success());
    for (mode, difficulty, expected) in [
        ("read", "1", "copilot"),
        ("read", "2", "copilot"),
        ("read", "3", "claude"),
        ("edit", "1", "claude"),
    ] {
        let out = f
            .command()
            .args([
                "route",
                "--brief",
                "project/brief.md",
                "--mode",
                mode,
                "--difficulty",
                difficulty,
                "--json",
            ])
            .output()
            .unwrap();
        assert_eq!(f.result(&out, 0)["choice"]["agent"], expected);
    }
    let out = f
        .command()
        .env("BATON_COPILOT_FLOOR", "124")
        .args([
            "route",
            "--brief",
            "project/brief.md",
            "--mode",
            "read",
            "--difficulty",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(f.result(&out, 0)["choice"]["agent"], "claude");
    let out = f
        .command()
        .args([
            "run",
            "auto",
            "--brief",
            "project/brief.md",
            "--mode",
            "read",
            "--difficulty",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(f.result(&out, 0)["agent"], "copilot");
}

#[test]
fn namespace_mount_failure_does_not_fall_back_to_writable_worker() {
    let f = Fixture::new();
    f.script(
        "unshare",
        r#"case "$*" in
'-rm sh -c mount -t tmpfs tmpfs /tmp') exit 0 ;;
*) echo 'read-only bind mount failed' >&2; exit 1 ;;
esac"#,
    );
    let out = f
        .run_command("mount-failed")
        .env_remove("BATON_COPILOT_NO_NS")
        .output()
        .unwrap();
    assert!(f.result(&out, 1)["notes"]
        .as_str()
        .unwrap()
        .contains("bind mount failed"));
    assert!(!f.dir.join("argv").exists());
}

