#![forbid(unsafe_code)]

mod brief;
mod cli;
mod codex;
mod git;
mod ledger;
mod paths;
mod result;
mod status;
mod verify;

use crate::{
    brief::Brief,
    cli::Cli,
    result::{Meta, Worker},
    status::Progress,
};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::Instant,
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
    if let Cli::Help = cli {
        println!("baton run codex --brief FILE [--mode edit|read] [--effort low|medium|high|xhigh] [--model SLUG] [--task ID] [--base REF] [--timeout SECS] [--no-worktree] [--json]\nbaton fix <task> --notes FILE\nbaton ps [--json]\nbaton show <task> [--diff|--log|--brief]\nbaton accept <task> [--squash]\nbaton drop <task>");
        return Ok(0);
    }
    let home = paths::home()?;
    match cli {
        Cli::Run(run) => start(&home, run),
        Cli::Fix { task, notes } => {
            let text = fs::read_to_string(&notes)
                .map_err(|e| Error::usage(format!("{}: {e}", notes.display())))?;
            brief::lint(&text)?;
            let dir = home.join("tasks").join(&task);
            let _lock = Lock::acquire(&dir)?;
            let mut meta: Meta = paths::read_json(&dir.join("meta.json"))?;
            if meta.closed || meta.agent != "codex" || meta.thread_id.is_none() {
                return Err(Error::usage(
                    "fix requires an open Codex task with a stored thread_id",
                ));
            }
            if meta.rounds >= 2 {
                return Err(Error::usage(
                    "two fix rounds already used; take over this task",
                ));
            }
            let brief = Brief::parse(fs::read_to_string(dir.join("brief.md"))?)?;
            meta.rounds += 1;
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
        fs::create_dir_all(home.join("wt"))?;
        let wt = home.join("wt").join(&task);
        git::run(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &wt.to_string_lossy(),
                &base_commit,
            ],
        )
        .map_err(|e| Error::usage(format!("cannot create task worktree: {e}")))?;
        wt
    } else {
        root.clone()
    };
    let mut meta = Meta {
        task,
        agent: "codex".into(),
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
        closed: false,
    };
    execute(home, &dir, &mut meta, &brief, &brief.text, false, run.json)
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
    paths::write_json(&dir.join("meta.json"), meta)?;
    let mut progress = Progress::new(dir, meta, json)?;
    let start = Instant::now();
    let outcome = match codex::run(dir, meta, input, resume, &mut progress) {
        Ok(outcome) => outcome,
        Err(e) => codex::Outcome {
            timed_out: false,
            success: false,
            error: e.to_string(),
        },
    };
    let raw = fs::read_to_string(dir.join("last.json")).unwrap_or_default();
    let mut worker = serde_json::from_str::<Worker>(&raw)
        .ok()
        .filter(|w| ["done", "partial", "blocked", "needs_decision"].contains(&w.status.as_str()))
        .unwrap_or_else(|| Worker {
            status: "failed".into(),
            notes: result::first(&raw, 300),
            questions: Vec::new(),
        });
    if !outcome.success {
        worker.status = "failed".into();
        if !outcome.error.is_empty() {
            worker.notes = result::first(&outcome.error, 300);
        }
    }
    let mut verification = match verify::run(dir, meta, brief, &mut progress, outcome.timed_out) {
        Ok(verification) => verification,
        Err(e) => {
            worker.status = "failed".into();
            worker.notes = format!("verification failed: {e}");
            verify::Verification {
                verified: false,
                files: Vec::new(),
                out_of_scope: Vec::new(),
                acceptance: Vec::new(),
                diffstat: "+0 -0 in 0 files".into(),
            }
        }
    };
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
        worktree: meta.worktree.clone(),
        branch: meta.branch.clone(),
    };
    paths::write_json(&dir.join("meta.json"), meta)?;
    paths::write_json(&dir.join("result.json"), &result)?;
    ledger::append(home, &result, meta)?;
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
    progress.finish()?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(code)
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
