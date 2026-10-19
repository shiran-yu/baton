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
        let fixture = Self {
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
    let failed = f.run(
        "failed-test",
        "Change the file",
        "printf failed >&2; exit 7",
    );
    code(&failed, 3);
    assert_eq!(json(&failed)["acceptance"][0]["passed"], false);
    assert_eq!(json(&failed)["acceptance"][0]["tail"], "failed");
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
