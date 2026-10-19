use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    codex: PathBuf,
    agy: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "baton-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.join("repo");
        fs::create_dir(&repo).unwrap();
        let codex = dir.join("fake-codex");
        fs::write(&codex, include_str!("fixtures/codex.sh")).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        let agy = dir.join("fake-agy");
        fs::write(&agy, include_str!("fixtures/agy.sh")).unwrap();
        fs::set_permissions(&agy, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self {
            agy,
            home: dir.join("home"),
            dir,
            repo,
            codex,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Test"]);
        fixture.git(&["config", "user.email", "test@localhost"]);
        fs::write(fixture.repo.join("README"), "initial\n").unwrap();
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "initial"]);
        fixture
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_baton"));
        cmd.current_dir(&self.repo)
            .env("BATON_HOME", &self.home)
            .env("BATON_CODEX", &self.codex)
            .env("BATON_AGY", &self.agy)
            .env("CODEX_HOME", self.dir.join("codex-home"))
            .env("TMPDIR", &self.dir)
            .env("USER", "baton-test-user")
            .env("SHELL", "/bin/sh")
            .env("LC_ALL", "C")
            .env("XDG_CONFIG_HOME", self.dir.join("xdg"))
            .env("UNRELATED_VARIABLE", "should-be-removed")
            .env("OPENAI_API_KEY", "should-be-removed")
            .env("KEYFOB_TEST", "should-be-removed")
            .env("CUSTOM_TOKEN", "should-be-removed");
        cmd
    }
    fn call(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn brief(&self, name: &str, goal: &str, acceptance: &str) -> PathBuf {
        let path = self.dir.join(format!("{name}.md"));
        fs::write(&path, format!("## Goal\n{goal}\n\n## Files\n- `src/**`\n\n## Forbidden\nNo unrelated changes.\n\n## Acceptance\n```sh\n# one command per line\n\n{acceptance}\n```\n```\nfalse\n```\n")).unwrap();
        path
    }
    fn run(&self, id: &str, goal: &str, acceptance: &str) -> Output {
        let brief = self.brief(id, goal, acceptance);
        self.command()
            .args(["run", "codex", "--task", id, "--brief"])
            .arg(brief)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn task(&self, task: &str) -> PathBuf {
        self.home.join("tasks").join(task)
    }
    fn read(&self, task: &str, file: &str) -> Value {
        serde_json::from_slice(&fs::read(self.task(task).join(file)).unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn code(out: &Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn run_lean_profile_result_resume_and_accept() {
    let f = Fixture::new();
    let out = f.run("good", "Write one file", "test -f src/output.txt");
    code(&out, 0);
    let result = json(&out);
    assert_eq!(result, f.read("good", "result.json"));
    assert_eq!(result["verified"], true);
    assert_eq!(result["status"], "done");
    assert_eq!(
        result["files_changed"],
        serde_json::json!(["src/output.txt"])
    );
    assert_eq!(result["diffstat"], "+1 -0 in 1 files");
    assert_eq!(
        result["tokens"],
        serde_json::json!({"input":190369,"cached":156288,"output":5927})
    );
    assert_eq!(result["acceptance"][0]["passed"], true);
    let wt = f.home.join("wt/good");
    assert!(wt.join(".git").is_file());
    assert_eq!(f.git(&["branch", "--list", "baton/good"]), "+ baton/good");
    let task = f.task("good");
    let args = fs::read_to_string(task.join("argv-no.txt")).unwrap();
    let expected = vec![
        "exec".to_string(),
        "--ignore-user-config".into(),
        "-m".into(),
        "gpt-6.1-sol".into(),
        "-c".into(),
        "model_reasoning_effort=\"medium\"".into(),
        "-c".into(),
        "model_reasoning_summary=\"none\"".into(),
        "-c".into(),
        "model_verbosity=\"low\"".into(),
        "-c".into(),
        "web_search=\"disabled\"".into(),
        "-c".into(),
        "features.apps=false".into(),
        "-s".into(),
        "workspace-write".into(),
        "-C".into(),
        wt.display().to_string(),
        "--output-schema".into(),
        task.join("schema.json").display().to_string(),
        "-o".into(),
        task.join("last.json").display().to_string(),
        "--json".into(),
        "-".into(),
    ];
    assert_eq!(args.lines().collect::<Vec<_>>(), expected);
    let env = fs::read_to_string(task.join("environment.txt")).unwrap();
    assert!(env.contains("BATON_TASK=good"));
    assert!(!env.contains("should-be-removed"));
    assert_eq!(f.read("good", "meta.json")["thread_id"], "fake-thread-123");
    let status = f.read("good", "status.json");
    assert_eq!(status["commands"], 1);
    assert_eq!(status["last"], "cargo test");
    assert_eq!(status["phase"], "done");
    assert!(fs::read_to_string(task.join("log.txt"))
        .unwrap()
        .contains("$ cargo test  (exit 0)"));
    assert_eq!(
        fs::read_to_string(task.join("events.jsonl"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    let ps = f.call(&["ps", "--json"]);
    code(&ps, 0);
    assert_eq!(json(&ps)[0]["task"], "good");
    for flag in ["--brief", "--log", "--diff"] {
        code(&f.call(&["show", "good", flag]), 0);
    }
    code(&f.call(&["show", "good"]), 0);
    let notes = f.dir.join("notes.md");
    fs::write(&notes, "Append the correction.").unwrap();
    let fixed = f
        .command()
        .args(["fix", "good", "--notes"])
        .arg(&notes)
        .output()
        .unwrap();
    code(&fixed, 0);
    let resume_args = fs::read_to_string(task.join("argv-yes.txt")).unwrap();
    assert_eq!(
        resume_args,
        format!(
            "exec\nresume\nfake-thread-123\n{}",
            args.strip_prefix("exec\n").unwrap()
        )
    );
    assert_eq!(
        fs::read_to_string(task.join("input-yes.txt")).unwrap(),
        "Append the correction."
    );
    assert_eq!(
        f.read("good", "result.json")["diffstat"],
        "+2 -0 in 1 files"
    );
    assert_eq!(
        fs::read_to_string(f.home.join("ledger.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    code(&f.call(&["accept", "good"]), 0);
    assert!(!wt.exists());
    assert_eq!(
        fs::read_to_string(f.repo.join("src/output.txt")).unwrap(),
        "hello\nfixed\n"
    );
    assert_eq!(f.read("good", "meta.json")["closed"], true);
    code(
        &f.command()
            .args(["fix", "good", "--notes"])
            .arg(notes)
            .output()
            .unwrap(),
        2,
    );
}

#[test]
fn independent_verification_catches_scope_acceptance_and_empty_diff() {
    let f = Fixture::new();
    let outside = f.run("outside", "FAKE_OUT_OF_SCOPE", "true");
    code(&outside, 3);
    assert_eq!(json(&outside)["verified"], false);
    assert_eq!(
        json(&outside)["out_of_scope"],
        serde_json::json!(["outside.txt"])
    );
    let scope_prompt = fs::read_to_string(f.task("outside").join("fix-1.md")).unwrap();
    assert!(scope_prompt.contains("out_of_scope paths") && scope_prompt.contains("outside.txt"));
    let failed = f.run(
        "failed-test",
        "Change the file",
        "printf failed >&2; exit 7",
    );
    code(&failed, 3);
    assert_eq!(json(&failed)["acceptance"][0]["passed"], false);
    assert_eq!(json(&failed)["acceptance"][0]["tail"], "failed");
    let fix_prompt = fs::read_to_string(f.task("failed-test").join("fix-1.md")).unwrap();
    assert!(fix_prompt.contains("printf failed >&2; exit 7"));
    assert!(fix_prompt.contains("Output tail:\nfailed"));
    let empty = f.run("empty", "FAKE_NO_EDIT", "true");
    code(&empty, 3);
    assert_eq!(json(&empty)["files_changed"], serde_json::json!([]));
    code(&f.call(&["drop", "outside"]), 0);
    assert!(!f.home.join("wt/outside").exists());
    assert_eq!(f.git(&["branch", "--list", "baton/outside"]), "");
}

#[test]
fn lint_validation_and_task_paths() {
    let f = Fixture::new();
    let out = f.run("risky", "Read KEYFOB", "true");
    code(&out, 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("risk lint"));
    assert!(!f.home.exists());
    let missing = f.dir.join("missing.md");
    fs::write(&missing, "## Goal\nNo other headings\n").unwrap();
    code(
        &f.command()
            .args(["run", "codex", "--brief"])
            .arg(missing)
            .output()
            .unwrap(),
        2,
    );
    for args in [
        vec!["run", "agy"],
        vec!["show", "../escape"],
        vec!["run", "codex", "--brief", "a", "--timeout", "0"],
        vec!["ps", "--bogus"],
    ] {
        code(&f.call(&args), 2);
    }
}

#[test]
fn read_mode_custom_options_and_generated_id() {
    let f = Fixture::new();
    let brief = f.brief("read", "FAKE_NO_EDIT", "true");
    let out = f
        .command()
        .args(["run", "codex", "--brief"])
        .arg(brief)
        .args([
            "--mode",
            "read",
            "--model",
            "custom-model",
            "--effort",
            "xhigh",
            "--json",
        ])
        .output()
        .unwrap();
    code(&out, 0);
    let result = json(&out);
    assert_eq!(result["verified"], true);
    assert_eq!(result["branch"], "");
    let task = result["task"].as_str().unwrap();
    assert_eq!(task.len(), 22);
    assert!(task.starts_with("t-"));
    let args = fs::read_to_string(f.task(task).join("argv-no.txt")).unwrap();
    assert!(args.contains("-s\nread-only\n"));
    assert!(args.contains("model_reasoning_effort=\"xhigh\""));
    assert!(args.contains("-m\ncustom-model\n"));
    assert_eq!(
        Path::new(result["worktree"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        f.repo.canonicalize().unwrap()
    );
}

#[test]
fn timeouts_and_malformed_worker_results() {
    let f = Fixture::new();
    let brief = f.brief("timeout", "FAKE_SLEEP", "true");
    let out = f
        .command()
        .args(["run", "codex", "--task", "timeout", "--brief"])
        .arg(brief)
        .args(["--timeout", "1", "--json"])
        .output()
        .unwrap();
    code(&out, 5);
    assert_eq!(json(&out)["status"], "failed");
    assert_eq!(json(&out)["verified"], false);
    assert_eq!(f.read("timeout", "status.json")["phase"], "failed");
    let invalid = f.run("invalid", "FAKE_INVALID", "true");
    code(&invalid, 1);
    assert_eq!(json(&invalid)["notes"], "this is not JSON");
    code(&f.run("auth", "FAKE_AUTH", "true"), 4);
    let failed_event = f.run("failed-event", "FAKE_FAIL_EVENT", "true");
    code(&failed_event, 1);
    assert_eq!(json(&failed_event)["notes"], "worker turn failed");
    code(&f.run("decision", "FAKE_DECISION", "true"), 6);
    let brief = f.brief("accept-timeout", "Make a file", "sleep 30");
    let out = f
        .command()
        .args(["run", "codex", "--task", "accept-timeout", "--brief"])
        .arg(brief)
        .args(["--timeout", "1", "--json"])
        .output()
        .unwrap();
    code(&out, 3);
    assert!(json(&out)["acceptance"][0]["tail"]
        .as_str()
        .unwrap()
        .contains("timed out"));
    // The other scenarios take us beyond the grandchild's two-second delay.
    assert!(
        !f.task("timeout").join("child-survived").exists(),
        "timeout failed to kill the worker process group"
    );
}

#[test]
fn squash_accept_and_fix_round_limit() {
    let f = Fixture::new();
    code(&f.run("squash", "Write the file", "true"), 0);
    let notes = f.dir.join("notes");
    fs::write(&notes, "Fix it").unwrap();
    for _ in 0..2 {
        code(
            &f.command()
                .args(["fix", "squash", "--notes"])
                .arg(&notes)
                .output()
                .unwrap(),
            0,
        );
    }
    code(
        &f.command()
            .args(["fix", "squash", "--notes"])
            .arg(notes)
            .output()
            .unwrap(),
        2,
    );
    code(&f.call(&["accept", "squash", "--squash"]), 0);
    assert!(!f.home.join("wt/squash").exists());
    assert_eq!(
        f.git(&["log", "-1", "--format=%an <%ae>"]),
        "baton <baton@localhost>"
    );
}

#[test]
fn no_worktree_leaves_the_checkout_uncommitted() {
    let f = Fixture::new();
    let original = f.git(&["rev-parse", "HEAD"]);
    let brief = f.brief("direct", "Write a file", "true");
    let out = f
        .command()
        .args(["run", "codex", "--brief"])
        .arg(brief)
        .args(["--no-worktree", "--json"])
        .output()
        .unwrap();
    code(&out, 0);
    assert_eq!(json(&out)["verified"], true);
    assert!(f.repo.join("src/output.txt").exists());
    assert_eq!(f.git(&["rev-parse", "HEAD"]), original);
}

fn run_agy(f: &Fixture, id: &str, goal: &str, acceptance: &str, extra: &[&str]) -> Output {
    f.command()
        .args(["run", "agy", "--task", id, "--brief"])
        .arg(f.brief(id, goal, acceptance))
        .arg("--json")
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn agy_verified_lean_agent_and_exact_exclusion() {
    let f = Fixture::new();
    fs::create_dir_all(f.repo.join(".agents/plugins")).unwrap();
    fs::write(f.repo.join(".agents/plugins/tracked.md"), "tracked").unwrap();
    f.git(&["add", "."]);
    f.git(&["commit", "-m", "tracked agents plugin"]);
    let out = run_agy(&f, "agy", "Write a file", "test -f src/output.txt", &[]);
    code(&out, 0);
    let result = json(&out);
    assert_eq!(result["status"], "done");
    assert_eq!(result["verified"], true);
    assert_eq!(
        result["files_changed"],
        serde_json::json!(["src/output.txt"])
    );
    assert_eq!(
        result["tokens"],
        serde_json::json!({"input":100,"cached":40,"output":13})
    );
    assert_eq!(f.read("agy", "status.json")["commands"], 1);
    assert_eq!(f.read("agy", "status.json")["last"], "write_to_file");
    let wt = f.home.join("wt/agy");
    let agent = fs::read_to_string(wt.join(".agents/agents/baton-worker.md")).unwrap();
    assert_eq!(agent, "---\nname: baton-worker\ndescription: Bounded baton worker\nexcludeDefaultComponents: true\ninheritCustomizations: false\nsubagent: false\ntools:\n  - view_file\n  - list_dir\n  - find_by_name\n  - grep_search\n  - write_to_file\n  - replace_file_content\n  - multi_replace_file_content\n  - finish\n---\nStay inside the files listed in the brief. Never run commands; baton runs the acceptance commands. Call finish with the result object.\n");
    let exclude = fs::read_to_string(f.repo.join(".git/info/exclude")).unwrap();
    assert!(exclude
        .lines()
        .any(|s| s == "/.agents/agents/baton-worker.md"));
    assert!(!exclude.lines().any(|s| s == ".agents/" || s == "/.agents/"));
    fs::write(wt.join(".agents/plugins/tracked.md"), "changed").unwrap();
    let changed = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&wt)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&changed.stdout).contains(".agents/plugins/tracked.md"));
    let task = f.task("agy");
    let args = fs::read_to_string(task.join("agy-argv.txt")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        vec![
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--agent",
            "baton-worker",
            "--model",
            "gemini-3.8-flash",
            "--effort",
            "medium",
            "--mode",
            "accept-edits",
            "--json-schema",
            task.join("schema.json").to_str().unwrap(),
            "--print-timeout",
            "1800s"
        ]
    );
    assert!(task.join("agy-closed").exists());
    assert_eq!(f.read("agy", "meta.json")["thread_id"], "fake-agy");
    let turns = fs::read_to_string(task.join("agy-turns.jsonl")).unwrap();
    let turn: Value = serde_json::from_str(turns.trim()).unwrap();
    assert_eq!(turn["event"], "user");
    assert_eq!(
        turn["message"]["content"],
        fs::read_to_string(task.join("brief.md")).unwrap()
    );
    // Restore the tracked file so accept can confirm the generated agent is ignored.
    fs::write(wt.join(".agents/plugins/tracked.md"), "tracked").unwrap();
    code(&f.call(&["accept", "agy"]), 0);
}

#[test]
fn agy_denials_and_missing_structured_output() {
    let f = Fixture::new();
    for (id, goal, status, expected_code) in [
        ("denied", "FAKE_DENY", "blocked", 0),
        ("step-denied", "FAKE_STEP_DENY", "blocked", 0),
        ("stderr-denied", "FAKE_STDERR_DENY", "blocked", 0),
        ("missing", "FAKE_NO_STRUCTURED", "failed", 1),
        (
            "missing-error",
            "FAKE_NO_STRUCTURED FAKE_RESULT_ERROR",
            "failed",
            1,
        ),
        ("late-denied", "FAKE_LATE_STDERR_DENY", "blocked", 0),
    ] {
        let out = run_agy(&f, id, goal, "true", &[]);
        code(&out, expected_code);
        assert_eq!(json(&out)["status"], status);
        let notes = json(&out)["notes"].as_str().unwrap().to_string();
        assert!(
            notes.contains("permission check failed")
                || notes.contains("jetski: no output produced")
                || notes == "no structured_output"
        );
        assert_eq!(f.read(id, "meta.json")["rounds"], 0);
    }
}

#[test]
fn codex_and_agy_auto_fix_and_explicit_agy_fix() {
    let f = Fixture::new();
    for agent in ["codex", "agy"] {
        let brief = f.brief(
            agent,
            "Write then correct a file",
            "test -f src/output.txt && test \"$(wc -l < src/output.txt | tr -d ' ')\" = 2",
        );
        let out = f
            .command()
            .args(["run", agent, "--task", agent, "--brief"])
            .arg(brief)
            .arg("--json")
            .output()
            .unwrap();
        code(&out, 0);
        assert_eq!(json(&out)["verified"], true);
        assert_eq!(json(&out)["rounds"], 1);
        let prompt = fs::read_to_string(f.task(agent).join("fix-1.md")).unwrap();
        assert!(prompt.contains("Failing acceptance command:"));
        assert!(prompt.contains("Output tail:"));
        let events = fs::read_to_string(f.task(agent).join("events.jsonl")).unwrap();
        if agent == "agy" {
            assert_eq!(
                events
                    .lines()
                    .filter(|s| s.contains("\"event\":\"init\""))
                    .count(),
                1
            );
            assert_eq!(
                json(&out)["tokens"],
                serde_json::json!({"input":200,"cached":80,"output":26})
            );
            let turns = fs::read_to_string(f.task(agent).join("agy-turns.jsonl")).unwrap();
            assert_eq!(turns.lines().count(), 2);
            assert!(turns.contains("Failing acceptance command"));
        } else {
            assert_eq!(json(&out)["tokens"]["input"], 380738);
            let args = fs::read_to_string(f.task(agent).join("argv-yes.txt")).unwrap();
            assert!(args.starts_with("exec\nresume\nfake-thread-123\n--ignore-user-config\n"));
        }
    }
    let ledger = fs::read_to_string(f.home.join("ledger.jsonl")).unwrap();
    for line in ledger.lines() {
        assert_eq!(serde_json::from_str::<Value>(line).unwrap()["rounds"], 1);
    }
    let notes = f.dir.join("fix-notes.md");
    fs::write(&notes, "Make the same correction.").unwrap();
    let fixed = f
        .command()
        .args(["fix", "agy", "--notes"])
        .arg(notes)
        .output()
        .unwrap();
    code(&fixed, 0);
    let turns = fs::read_to_string(f.task("agy").join("agy-turns.jsonl")).unwrap();
    assert!(turns.lines().any(|line| line.contains("Current diff:")));
    assert!(turns
        .lines()
        .any(|line| line.contains("Make the same correction.")));
}

#[test]
fn auto_fix_zero_and_multiple_rounds() {
    let f = Fixture::new();
    for agent in ["codex", "agy"] {
        for (fixes, lines, expected) in [("0", "2", 3), ("2", "3", 0)] {
            let id = format!("{agent}-{fixes}");
            let acceptance = format!("test \"$(wc -l < src/output.txt | tr -d ' ')\" = {lines}");
            let brief = f.brief(&id, "Write a file", &acceptance);
            let out = f
                .command()
                .args(["run", agent, "--task", &id, "--brief"])
                .arg(brief)
                .args(["--auto-fix", fixes, "--json"])
                .output()
                .unwrap();
            code(&out, expected);
            assert_eq!(json(&out)["rounds"], fixes.parse::<u32>().unwrap());
        }
    }
}

#[test]
fn detached_wait_and_foreground_results_match() {
    let f = Fixture::new();
    let foreground = f.run("foreground", "Write a file", "test -f src/output.txt");
    code(&foreground, 0);
    let waited = f.call(&["wait", "foreground"]);
    code(&waited, 0);
    assert_eq!(foreground.stdout, waited.stdout);
    let brief = f.brief("detached", "Write a file", "test -f src/output.txt");
    let detached = f
        .command()
        .args(["run", "codex", "--task", "detached", "--brief"])
        .arg(brief)
        .arg("--detach")
        .output()
        .unwrap();
    code(&detached, 0);
    assert_eq!(detached.stdout, b"detached\n");
    assert!(f.task("detached").join("meta.json").exists());
    assert!(f.task("detached").join("status.json").exists());
    let waited = f.call(&["wait", "detached", "--max", "20"]);
    code(&waited, 0);
    assert_eq!(
        String::from_utf8_lossy(&waited.stdout).trim(),
        fs::read_to_string(f.task("detached").join("result.json")).unwrap()
    );
    // Compare the actual result contracts, allowing only per-task identity and duration to differ.
    let mut a = json(&foreground);
    let mut b = json(&waited);
    for key in ["task", "worktree", "branch", "seconds"] {
        a.as_object_mut().unwrap().remove(key);
        b.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(a, b);
}

#[test]
fn wait_max_progress_then_worker_exit_codes() {
    let f = Fixture::new();
    let brief = f.brief("slow", "FAKE_WAIT_SLEEP", "true");
    let detached = f
        .command()
        .args(["run", "codex", "--task", "slow", "--brief"])
        .arg(brief)
        .arg("--detach")
        .output()
        .unwrap();
    code(&detached, 0);
    let waiting = f.call(&["wait", "slow", "--max", "1"]);
    code(&waiting, 7);
    let line = String::from_utf8(waiting.stdout).unwrap();
    assert_eq!(line.lines().count(), 1);
    assert!(line.starts_with("codex · medium · "));
    assert!(line.contains(" files · ") && line.contains(" cmds · ") && line.contains(" tok · "));
    code(&f.call(&["wait", "slow", "--max", "20"]), 0);
    for (id, goal, acceptance, expected) in [
        ("bad-verification", "Write a file", "false", 3),
        ("bad-worker", "FAKE_INVALID", "true", 1),
        ("quota", "FAKE_AUTH", "true", 4),
        ("decision-wait", "FAKE_DECISION", "true", 6),
    ] {
        let out = f.run(id, goal, acceptance);
        code(&out, expected);
        let waited = f.call(&["wait", id, "--max", "1"]);
        code(&waited, expected);
        assert_eq!(out.stdout, waited.stdout);
    }
}

#[test]
fn worker_environment_allowlist_and_agy_read_tools() {
    let f = Fixture::new();
    code(&f.run("env-codex", "Write a file", "true"), 0);
    let out = run_agy(&f, "env-agy", "FAKE_NO_EDIT", "true", &["--mode", "read"]);
    code(&out, 0);
    for (task, file) in [
        ("env-codex", "environment.txt"),
        ("env-agy", "agy-environment.txt"),
    ] {
        let env = fs::read_to_string(f.task(task).join(file)).unwrap();
        for name in [
            "HOME=",
            "PATH=",
            "CODEX_HOME=",
            "TMPDIR=",
            "USER=baton-test-user",
            "SHELL=/bin/sh",
            "LC_ALL=C",
            "BATON_TASK=",
        ] {
            assert!(env.contains(name), "missing {name}: {env}");
        }
        assert!(!env.contains("should-be-removed"));
        assert_eq!(env.contains("XDG_CONFIG_HOME="), task == "env-agy");
    }
    let agent = fs::read_to_string(f.repo.join(".agents/agents/baton-worker.md")).unwrap();
    assert!(agent.contains("  - finish\n"));
    for tool in [
        "write_to_file",
        "replace_file_content",
        "multi_replace_file_content",
        "run_command",
    ] {
        assert!(!agent.contains(tool));
    }
}

#[test]
fn agy_timeout_terminates_then_kills_the_group() {
    let f = Fixture::new();
    let out = run_agy(
        &f,
        "agy-timeout",
        "FAKE_TIMEOUT",
        "true",
        &["--timeout", "1"],
    );
    code(&out, 5);
    assert_eq!(json(&out)["status"], "failed");
    assert_eq!(json(&out)["verified"], false);
    assert_eq!(f.read("agy-timeout", "status.json")["phase"], "failed");
    let waited = f.call(&["wait", "agy-timeout"]);
    code(&waited, 5);
    assert_eq!(waited.stdout, out.stdout);
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(!f.task("agy-timeout").join("child-survived").exists());
}

#[test]
fn detached_setup_failure_is_a_waitable_result() {
    let f = Fixture::new();
    f.git(&["branch", "baton/conflict"]);
    let brief = f.brief("conflict", "Write a file", "true");
    let out = f
        .command()
        .args(["run", "codex", "--task", "conflict", "--brief"])
        .arg(brief)
        .arg("--detach")
        .output()
        .unwrap();
    code(&out, 0);
    assert_eq!(out.stdout, b"conflict\n");
    let waited = f.call(&["wait", "conflict", "--max", "20"]);
    code(&waited, 1);
    assert_eq!(json(&waited)["status"], "failed");
    assert!(json(&waited)["notes"]
        .as_str()
        .unwrap()
        .contains("already exists"));
}
