#![forbid(unsafe_code)]

mod agy;
mod brief;
mod cli;
mod codex;
mod doctor;
mod git;
mod ledger;
mod paths;
mod process;
mod result;
mod status;
mod statusline;
mod usage;
mod verify;
mod worker;

use crate::{brief::Brief, cli::Cli, result::Meta, status::Progress};
use std::{
    fmt, fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Error {
    code: i32,
    message: String,
}
impl Error {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn usage(message: impl Into<String>) -> Self {
        Self::new(2, message)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(1, e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(1, e.to_string())
    }
}
type Res<T> = std::result::Result<T, Error>;

// Atomic create prevents two operations from editing the same task concurrently.
struct Lock(PathBuf);
impl Lock {
    fn acquire(dir: &Path) -> Res<Self> {
        let path = dir.join("active.lock");
        let _file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                Error::usage(format!(
                    "cannot lock task (another operation may be active): {e}"
                ))
            })?;
        Ok(Self(path))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn main() {
    // Parse first: help and usage errors never touch state or invoke other programs.
    let outcome = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| Error::usage("arguments must be valid UTF-8"))
        })
        .collect::<Res<Vec<_>>>()
        .and_then(|args| cli::parse(args.into_iter()))
        .and_then(dispatch);
    let code = match outcome {
        Ok(code) => code,
        Err(e) => {
            eprintln!("baton: {e}");
            e.code
        }
    };
    std::process::exit(code);
}

fn dispatch(cli: Cli) -> Res<i32> {
    // Hooks are best effort, including when HOME or the state directory is unavailable.
    if let Cli::Statusline(mode) = &cli {
        if let Ok(home) = paths::home() {
            statusline::run(&home, mode);
        }
        return Ok(0);
    }
    if let Cli::Doctor { quick: true, .. } = cli {
        doctor::quick();
        return Ok(0);
    }
    if let Cli::Help = cli {
        println!("baton run <codex|agy> --brief FILE [--mode edit|read] [--effort low|medium|high|xhigh] [--model SLUG] [--task ID] [--base REF] [--timeout SECS] [--no-worktree] [--json] [--auto-fix N] [--detach]\nbaton wait <task> [--max SECS]\nbaton fix <task> --notes FILE\nbaton ps [--json]\nbaton show <task> [--diff|--log|--brief]\nbaton accept <task> [--squash]\nbaton drop <task>\nbaton usage [--json] [--refresh]\nbaton statusline <line|subagent>\nbaton doctor [--quick | --scan --out DIR]");
        return Ok(0);
    }
    let home = paths::home()?;
    match cli {
        Cli::Run(run) => start(&home, run),
        Cli::Supervise(task) => {
            let dir = home.join("tasks").join(task);
            // The parent transfers its existing lock after spawning this supervisor.
            let _lock = Lock(dir.join("active.lock"));
            if !_lock.0.exists() {
                return Err(Error::usage("supervisor requires a prepared task lock"));
            }
            let mut meta: Meta = paths::read_json(&dir.join("meta.json"))?;
            let brief = Brief::parse(fs::read_to_string(dir.join("brief.md"))?)?;
            supervise(&home, &dir, &mut meta, &brief, true)
        }
        Cli::Wait { task, max } => wait(&home.join("tasks").join(task), max),
        Cli::Fix { task, notes } => {
            let text = fs::read_to_string(&notes)
                .map_err(|e| Error::usage(format!("{}: {e}", notes.display())))?;
            brief::lint(&text)?;
            let dir = home.join("tasks").join(&task);
            let _lock = Lock::acquire(&dir)?;
            let mut meta: Meta = paths::read_json(&dir.join("meta.json"))?;
            if meta.closed || (meta.agent == "codex" && meta.thread_id.is_none()) {
                return Err(Error::usage(
                    "fix requires an open task (Codex needs a stored thread_id)",
                ));
            }
            if meta.rounds >= 2 {
                return Err(Error::usage(
                    "two fix rounds already used; take over this task",
                ));
            }
            let brief = Brief::parse(fs::read_to_string(dir.join("brief.md"))?)?;
            meta.rounds += 1;
            // An explicit agy fix starts a fresh session with the brief and current diff.
            let text = if meta.agent == "agy" {
                let diff = git::run(
                    Path::new(&meta.worktree),
                    &["diff", &meta.base_commit, "--"],
                )?;
                format!(
                    "{}\n\nFix notes:\n{text}\n\nCurrent diff:\n{diff}",
                    brief.text
                )
            } else {
                text
            };
            execute(&home, &dir, &mut meta, &brief, &text, true, false)
        }
        Cli::Ps { json } => {
            let mut rows = Vec::new();
            let tasks = home.join("tasks");
            if tasks.exists() {
                let mut entries = fs::read_dir(tasks)?.collect::<std::io::Result<Vec<_>>>()?;
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    let file = entry.path().join("status.json");
                    if file.exists() {
                        rows.push(paths::read_json::<serde_json::Value>(&file)?);
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                for row in rows {
                    println!(
                        "{}  {}  {}  {}",
                        row["task"].as_str().unwrap_or("?"),
                        row["phase"].as_str().unwrap_or("?"),
                        row["last"].as_str().unwrap_or(""),
                        row["tests"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(0)
        }
        Cli::Show { task, view } => {
            let dir = home.join("tasks").join(task);
            match view.as_str() {
                "--diff" => {
                    let meta: Meta = paths::read_json(&dir.join("meta.json"))?;
                    let (cwd, target) = if !meta.closed {
                        (meta.worktree.as_str(), "")
                    } else {
                        (meta.repo_root.as_str(), meta.branch.as_str())
                    };
                    if target.is_empty() {
                        print!(
                            "{}",
                            git::run(Path::new(cwd), &["diff", &meta.base_commit, "--"])?
                        );
                    } else {
                        print!(
                            "{}",
                            git::run(Path::new(cwd), &["diff", &meta.base_commit, target, "--"])?
                        );
                    }
                }
                _ => {
                    let file = match view.as_str() {
                        "--log" => "log.txt",
                        "--brief" => "brief.md",
                        _ => "result.json",
                    };
                    println!(
                        "{}",
                        fs::read_to_string(dir.join(file))
                            .map_err(|e| Error::usage(format!("task data unavailable: {e}")))?
                            .trim_end()
                    );
                }
            }
            Ok(0)
        }
        Cli::Accept { task, squash } => manage(&home, &task, Some(squash)),
        Cli::Drop(task) => manage(&home, &task, None),
        Cli::Usage { json, refresh } => {
            let snapshot = usage::read(&home, refresh)?;
            if json {
                println!("{}", serde_json::to_string(&snapshot)?);
            } else {
                print!("{}", usage::table(&snapshot));
            }
            Ok(0)
        }
        Cli::Doctor { out, .. } => doctor::run(&home, out.as_deref()),
        Cli::Statusline(_) => Ok(0),
        Cli::Help => Ok(0),
    }
}

fn start(home: &Path, run: cli::Run) -> Res<i32> {
    let text = fs::read_to_string(&run.brief)
        .map_err(|e| Error::usage(format!("{}: {e}", run.brief.display())))?;
    let brief = Brief::parse(text)?;
    let root = git::root()?;
    let base_commit = git::run(
        &root,
        &["rev-parse", "--verify", &format!("{}^{{commit}}", run.base)],
    )
    .map_err(|e| Error::usage(format!("invalid base ref '{}': {e}", run.base)))?
    .trim()
    .to_string();
    let mut base_branch = git::run(&root, &["rev-parse", "--symbolic-full-name", &run.base])?
        .trim()
        .strip_prefix("refs/heads/")
        .map(String::from)
        .unwrap_or_default();
    if base_branch.is_empty() {
        base_branch = git::run(&root, &["branch", "--show-current"])?
            .trim()
            .to_string();
    }
    let task = run.task.unwrap_or_else(paths::id);
    cli::task_id(&task)?;
    let dir = home.join("tasks").join(&task);
    fs::create_dir_all(home.join("tasks"))?;
    fs::create_dir(&dir).map_err(|e| {
        Error::usage(format!(
            "cannot create task '{task}' (IDs must be unique): {e}"
        ))
    })?;
    let _lock = Lock::acquire(&dir)?;
    fs::write(dir.join("brief.md"), &brief.text)?;
    let managed = run.mode == "edit" && !run.no_worktree;
    let branch = if managed {
        format!("baton/{task}")
    } else {
        String::new()
    };
    let worktree = if managed {
        if run.agent == "agy" {
            agy_wt_root().join(&task)
        } else {
            home.join("wt").join(&task)
        }
    } else {
        root.clone()
    };
    let worktree = if worktree.is_absolute() {
        worktree
    } else {
        root.join(worktree)
    };
    let mut meta = Meta {
        task,
        agent: run.agent,
        model: run.model,
        effort: run.effort,
        mode: run.mode,
        base: run.base,
        base_commit,
        base_branch,
        repo_root: root.to_string_lossy().into_owned(),
        worktree: worktree.to_string_lossy().into_owned(),
        branch,
        thread_id: None,
        timeout: run.timeout,
        rounds: 0,
        auto_fix: run.auto_fix,
        closed: false,
    };
    paths::write_json(&dir.join("meta.json"), &meta)?;
    Progress::new(&dir, &meta, true)?;
    if run.detach {
        // Keep the task locked across the re-exec handoff.
        let stdout = fs::File::create(dir.join("supervisor.stdout"))?;
        let stderr = fs::File::create(dir.join("supervisor.stderr"))?;
        let stdin = fs::File::create(dir.join("supervisor.stdin"))?;
        let stdin = {
            drop(stdin);
            fs::File::open(dir.join("supervisor.stdin"))?
        };
        Command::new(std::env::current_exe()?)
            .args(["__supervise", &meta.task])
            .env("BATON_HOME", home)
            .stdin(Stdio::from(stdin))
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .spawn()?;
        std::mem::forget(_lock); // The supervisor now owns cleanup of active.lock.
        println!("{}", meta.task);
        Ok(0)
    } else {
        supervise(home, &dir, &mut meta, &brief, run.json)
    }
}

fn supervise(home: &Path, dir: &Path, meta: &mut Meta, brief: &Brief, json: bool) -> Res<i32> {
    let outcome = (|| {
        if !meta.branch.is_empty() {
            if let Some(parent) = Path::new(&meta.worktree).parent() {
                fs::create_dir_all(parent)?;
            }
            git::run(
                Path::new(&meta.repo_root),
                &[
                    "worktree",
                    "add",
                    "-b",
                    &meta.branch,
                    &meta.worktree,
                    &meta.base_commit,
                ],
            )?;
        }
        execute(home, dir, meta, brief, &brief.text, false, json)
    })();
    if let Err(e) = &outcome {
        // wait must also terminate when setup fails before a worker is started.
        let mut progress = Progress::new(dir, meta, true)?;
        progress.status.phase = "failed".into();
        progress.log(&e.to_string())?;
        let result = result::Result {
            task: meta.task.clone(),
            agent: meta.agent.clone(),
            model: meta.model.clone(),
            effort: meta.effort.clone(),
            status: "failed".into(),
            verified: false,
            files_changed: Vec::new(),
            diffstat: "+0 -0 in 0 files".into(),
            out_of_scope: Vec::new(),
            acceptance: Vec::new(),
            notes: e.to_string(),
            questions: Vec::new(),
            tokens: result::Tokens::default(),
            seconds: 0,
            rounds: meta.rounds,
            worktree: meta.worktree.clone(),
            branch: meta.branch.clone(),
        };
        publish(home, dir, meta, &result, e.code, &mut progress)?;
    }
    outcome
}

fn wait(dir: &Path, max: u64) -> Res<i32> {
    let start = Instant::now();
    loop {
        let status: status::Status = paths::read_json(&dir.join("status.json"))?;
        if ["done", "failed"].contains(&status.phase.as_str()) && dir.join("exit.json").exists() {
            let code = paths::read_json(&dir.join("exit.json"))?;
            if dir.join("result.json").exists() {
                println!(
                    "{}",
                    fs::read_to_string(dir.join("result.json"))?.trim_end()
                );
            } else {
                return Err(Error::new(code, "task setup failed; see task log"));
            }
            return Ok(code);
        }
        if start.elapsed() >= Duration::from_secs(max) {
            println!("{}", status.line());
            return Ok(7);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn execute(
    home: &Path,
    dir: &Path,
    meta: &mut Meta,
    brief: &Brief,
    input: &str,
    resume: bool,
    json: bool,
) -> Res<i32> {
    let _ = fs::remove_file(dir.join("exit.json"));
    let _ = fs::remove_file(dir.join("result.json"));
    paths::write_json(&dir.join("meta.json"), meta)?;
    let mut progress = Progress::new(dir, meta, json)?;
    let start = Instant::now();
    let mut driver: Box<dyn worker::Worker> = if meta.agent == "agy" {
        match agy::Agy::new(dir, meta) {
            Ok(driver) => Box::new(driver),
            Err(e) => Box::new(FailedWorker(Some(e))),
        }
    } else {
        Box::new(worker::Codex)
    };
    let mut prompt = input.to_string();
    let mut resume = resume;
    let mut fixes = 0;
    let (mut outcome, mut worker, mut verification) = loop {
        let (outcome, mut worker) = driver
            .turn(dir, meta, &prompt, resume, &mut progress)
            .unwrap_or_else(|e| {
                (
                    codex::Outcome {
                        timed_out: e.code == 5,
                        success: false,
                        error: e.to_string(),
                    },
                    worker::failed(e.to_string()),
                )
            });
        if !outcome.success {
            worker.status = "failed".into();
            if !outcome.error.is_empty()
                && !(meta.agent == "agy" && worker.notes == "no structured_output")
            {
                worker.notes = result::first(&outcome.error, 300);
            }
        }
        let verification = match verify::run(dir, meta, brief, &mut progress, outcome.timed_out) {
            Ok(verification) => verification,
            Err(e) => {
                worker = worker::failed(format!("verification failed: {e}"));
                verify::Verification {
                    verified: false,
                    files: Vec::new(),
                    out_of_scope: Vec::new(),
                    acceptance: Vec::new(),
                    diffstat: "+0 -0 in 0 files".into(),
                }
            }
        };
        if verification.verified
            || !outcome.success
            || !["done", "partial"].contains(&worker.status.as_str())
            || fixes >= meta.auto_fix
        {
            break (outcome, worker, verification);
        }
        prompt = fix_prompt(&verification);
        fixes += 1;
        meta.rounds += 1;
        // Persist the human-readable prompt as well as the round before resuming.
        fs::write(dir.join(format!("fix-{}.md", meta.rounds)), &prompt)?;
        paths::write_json(&dir.join("meta.json"), meta)?;
        resume = true;
    };
    match driver.finish(&mut progress) {
        Ok(Some(message)) => {
            worker.status = "blocked".into();
            worker.notes = message;
        }
        Ok(None) => {}
        Err(e) => {
            outcome.success = false;
            outcome.timed_out |= e.code == 5;
            if outcome.error.is_empty() {
                outcome.error = e.to_string();
            }
            if worker.notes != "no structured_output" || outcome.timed_out {
                worker = worker::failed(outcome.error.clone());
            }
        }
    }
    if !meta.branch.is_empty() {
        if let Err(e) = commit_task(meta, brief) {
            worker.status = "failed".into();
            worker.notes = format!("cannot commit task changes: {e}");
            verification.verified = false;
        }
    }
    let result = result::Result {
        task: meta.task.clone(),
        agent: meta.agent.clone(),
        model: meta.model.clone(),
        effort: meta.effort.clone(),
        status: worker.status,
        verified: verification.verified,
        files_changed: verification.files,
        diffstat: verification.diffstat,
        out_of_scope: verification.out_of_scope,
        acceptance: verification.acceptance,
        notes: worker.notes,
        questions: worker.questions,
        tokens: progress.status.tokens.clone(),
        seconds: start.elapsed().as_secs(),
        rounds: meta.rounds,
        worktree: meta.worktree.clone(),
        branch: meta.branch.clone(),
    };
    let error_lower = outcome.error.to_lowercase();
    let code = if outcome.timed_out {
        5
    } else if !outcome.success
        && [
            "auth",
            "unauthorized",
            "quota",
            "rate limit",
            "usage limit",
            "not logged in",
        ]
        .iter()
        .any(|s| error_lower.contains(s))
    {
        4
    } else if result.status == "needs_decision" {
        6
    } else if result.status == "done" && !result.verified {
        3
    } else if result.status == "failed" {
        1
    } else {
        0
    };
    progress.status.phase = if code == 0 || code == 6 {
        "done"
    } else {
        "failed"
    }
    .into();
    publish(home, dir, meta, &result, code, &mut progress)?;
    Ok(code)
}

fn publish(
    home: &Path,
    dir: &Path,
    meta: &Meta,
    result: &result::Result,
    code: i32,
    progress: &mut Progress,
) -> Res<()> {
    paths::write_json(&dir.join("meta.json"), meta)?;
    paths::write_json(&dir.join("result.json"), result)?;
    ledger::append(home, result, meta)?;
    progress.finish()?;
    // Publish the exit code last so wait never observes an unfinished result.
    paths::write_json(&dir.join("exit.json"), &code)?;
    println!("{}", serde_json::to_string(result)?);
    Ok(())
}

struct FailedWorker(Option<Error>);
impl worker::Worker for FailedWorker {
    fn turn(
        &mut self,
        _: &Path,
        _: &mut Meta,
        _: &str,
        _: bool,
        _: &mut Progress,
    ) -> Res<(codex::Outcome, result::Worker)> {
        Err(self.0.take().unwrap())
    }
}

fn fix_prompt(verification: &verify::Verification) -> String {
    let mut prompt = String::from(
        "Verification failed. Fix the following issues inside the brief's allowed files.\n",
    );
    if verification.files.is_empty() {
        prompt.push_str("The edit task has an empty diff.\n");
    }
    for acceptance in &verification.acceptance {
        if !acceptance.passed {
            prompt.push_str(&format!(
                "\nFailing acceptance command: {}\nOutput tail:\n{}\n",
                acceptance.command, acceptance.tail
            ));
        }
    }
    if !verification.out_of_scope.is_empty() {
        prompt.push_str(&format!(
            "\nout_of_scope paths (revert these changes):\n{}\n",
            verification.out_of_scope.join("\n")
        ));
    }
    prompt
}

fn commit_task(meta: &Meta, brief: &Brief) -> Res<()> {
    let cwd = Path::new(&meta.worktree);
    let current = git::run(cwd, &["branch", "--show-current"])?;
    if current.trim() != meta.branch {
        return Err(Error::new(
            1,
            "worker switched branches; refusing to commit",
        ));
    }
    git::commit(cwd, &format!("baton {}: {}", meta.task, brief.goal))
}

fn manage(home: &Path, task: &str, accept: Option<bool>) -> Res<i32> {
    let dir = home.join("tasks").join(task);
    let _lock = Lock::acquire(&dir)?;
    let mut meta: Meta = paths::read_json(&dir.join("meta.json"))?;
    if meta.closed {
        return Err(Error::usage("task is already closed"));
    }
    if meta.branch.is_empty() {
        return Err(Error::usage("task has no managed worktree or branch"));
    }
    let root = Path::new(&meta.repo_root);
    if let Some(squash) = accept {
        if !git::run(Path::new(&meta.worktree), &["status", "--porcelain"])?
            .trim()
            .is_empty()
        {
            return Err(Error::usage(
                "task worktree has uncommitted changes; commit or fix them before accepting",
            ));
        }
        let current = git::run(root, &["branch", "--show-current"])?;
        if meta.base_branch.is_empty() || current.trim() != meta.base_branch {
            return Err(Error::usage(format!(
                "check out the base branch '{}' in {} before accepting",
                meta.base_branch,
                root.display()
            )));
        }
        if !git::run(root, &["status", "--porcelain"])?
            .trim()
            .is_empty()
        {
            return Err(Error::usage("base checkout must be clean before accepting"));
        }
        if squash {
            git::run(root, &["merge", "--squash", &meta.branch])?;
            git::commit(root, &format!("baton {task}: accept"))?;
        } else {
            git::run(
                root,
                &[
                    "-c",
                    "user.name=baton",
                    "-c",
                    "user.email=baton@localhost",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "merge",
                    "--no-edit",
                    &meta.branch,
                ],
            )?;
        }
    }
    git::run(root, &["worktree", "remove", "--force", &meta.worktree])?;
    if accept.is_none() {
        git::run(root, &["branch", "-D", &meta.branch])?;
    }
    meta.closed = true;
    paths::write_json(&dir.join("meta.json"), &meta)?;
    println!(
        "{} {task}",
        if accept.is_some() {
            "accepted"
        } else {
            "dropped"
        }
    );
    Ok(0)
}

/// Root for agy worktrees: outside the home directory (agy resolves its workspace to a trusted
/// ancestor such as `~`) and fixed, so `baton setup` can grant agy read/write on exactly this path.
/// `/tmp` resolves to `/private/tmp` on macOS.
pub fn agy_wt_root() -> PathBuf {
    let root = std::env::var_os("BATON_AGY_WT_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/baton-wt"));
    let _ = std::fs::create_dir_all(&root);
    std::fs::canonicalize(&root).unwrap_or(root)
}
