#![forbid(unsafe_code)]

mod agy;
mod brief;
mod cli;
mod codex;
mod copilot;
mod doctor;
mod git;
mod hook_adapt;
mod ledger;
mod paths;
mod process;
mod quota;
mod result;
mod route;
mod scorecard;
mod setup;
mod status;
mod statusline;
mod sync;
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
    if let Cli::HookAdapt(options) = &cli {
        return hook_adapt::run(options);
    }
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
        println!("baton run <codex|agy|copilot|auto> --brief FILE [--type T] [--difficulty N] [--mode edit|read] [--effort low|medium|high|xhigh] [--model SLUG] [--task ID] [--base REF] [--timeout SECS] [--no-worktree] [--json] [--auto-fix N] [--detach]\nbaton route --brief FILE [--mode edit|read] [--type T] [--difficulty N] [--json]\nbaton scorecard [--since DAYS] [--json]\nbaton review <task> --by <codex|agy>\nbaton wait <task> [--max SECS]\nbaton fix <task> --notes FILE\nbaton ps [--json]\nbaton show <task> [--diff|--log|--brief]\nbaton accept <task> [--squash]\nbaton drop <task>\nbaton usage [--json] [--refresh]\nbaton statusline <line|subagent>\nbaton doctor [--quick | --scan --out DIR]\nbaton setup [--plan [--json] | --apply ID[,ID...] [--yes]]\nbaton sync [--dry-run] [--to codex,agy] [--only NAME[,NAME]] [--include-trading]\nbaton hook-adapt --from <agy|codex> --event <ClaudeEvent> -- CMD [ARGS...]");
        return Ok(0);
    }
    let home = paths::home()?;
    match cli {
        Cli::Sync(options) => sync::run(&home, &options),
        Cli::HookAdapt(_) => unreachable!(),
        Cli::Setup(options) => setup::run(&home, options),
        Cli::Run(run) => start(&home, run),
        Cli::Route(options) => {
            let brief = Brief::parse_for_route(fs::read_to_string(&options.brief)?)?;
            let class = route::classify(&brief, options.task_type.as_deref(), options.difficulty)?;
            let routing = routing(&home, &brief, &class, &options.mode)?;
            if options.json {
                println!("{}", serde_json::to_string(&routing)?);
            } else {
                route::print(&routing.choice);
                if let Some(second) = routing.runner_up {
                    print!("runner-up: ");
                    route::print(&second);
                }
            }
            Ok(0)
        }
        Cli::Scorecard { since, json } => {
            let card = scorecard::aggregate(&ledger::read(&home)?, since, paths::now());
            if json {
                println!("{}", serde_json::to_string(&card)?);
            } else {
                scorecard::print(&card);
            }
            Ok(0)
        }
        Cli::Review { task, by } => review(&home, &task, by),
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
        Cli::Fix { task, notes, json } => {
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
            classify_meta(&mut meta, &brief)?;
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
            execute(&home, &dir, &mut meta, &brief, &text, true, json)
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
                    if meta.base_commit.is_empty() {
                        return Err(Error::usage(
                            "cannot show diff: task was run outside a git repository",
                        ));
                    }
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

fn routing(
    home: &Path,
    brief: &Brief,
    class: &route::Classification,
    mode: &str,
) -> Res<route::Routing> {
    // Hard rules do not depend on availability or the ledger being readable.
    let history = if route::hard_rule(class, brief).is_none() {
        fresh_usage(home);
        ledger::read(home)?
    } else {
        Vec::new()
    };
    Ok(route::recommend(
        class,
        brief,
        &history,
        &quota::Usage::cached(home),
        paths::now(),
        mode,
    ))
}

/// Routing weighs remaining quota, so a `baton usage` cache older than 30 minutes, or one taken
/// before a window reset, is refreshed first (free calls, a few seconds). Caches in other layouts
/// are left alone, and `BATON_NO_USAGE_REFRESH` turns this off (tests).
fn fresh_usage(home: &Path) {
    if std::env::var_os("BATON_NO_USAGE_REFRESH").is_some() {
        return;
    }
    let Some(snapshot) = usage::cache(home) else {
        return;
    };
    let now = paths::now();
    let reset = snapshot
        .agents
        .iter()
        .flat_map(|a| a.windows.iter())
        .any(|w| w.resets_at > 0 && w.resets_at <= now);
    if reset || now.saturating_sub(snapshot.fetched_at) > 1800 {
        let _ = usage::read(home, false);
    }
}

fn classify_meta(meta: &mut Meta, brief: &Brief) -> Res<()> {
    if meta.task_type.is_empty() || meta.difficulty == 0 {
        let class = route::classify(brief, None, None)?;
        meta.task_type = class.task_type;
        meta.difficulty = class.difficulty;
    }
    Ok(())
}

fn start(home: &Path, mut run: cli::Run) -> Res<i32> {
    let text = fs::read_to_string(&run.brief)
        .map_err(|e| Error::usage(format!("{}: {e}", run.brief.display())))?;
    let brief = Brief::parse_for_route(text)?;
    let class = route::classify(&brief, run.task_type.as_deref(), run.difficulty)?;
    if run.agent == "auto" {
        let choice = routing(home, &brief, &class, &run.mode)?.choice;
        if choice.agent == "claude" {
            println!(
                "{}",
                serde_json::json!({"status":"needs_decision", "agent":"claude",
                "notes":choice.reason, "choice":choice, "questions":["The lead should handle this brief."]})
            );
            return Ok(6);
        }
        run.agent = choice.agent;
        run.model = choice.model.unwrap_or_default();
        run.effort = choice.effort.unwrap_or_default();
    }
    if run.agent == "copilot" && run.mode != "read" {
        return Err(Error::usage("copilot runs read briefs only"));
    }
    brief::lint(&brief.text)?;
    let read_folder = if run.agent == "copilot" {
        Some(
            fs::canonicalize(&run.brief)?
                .parent()
                .unwrap()
                .to_path_buf(),
        )
    } else {
        None
    };
    let detected_root = if let Some(folder) = &read_folder {
        git::run(folder, &["rev-parse", "--show-toplevel"]).map(|root| PathBuf::from(root.trim()))
    } else {
        git::root()
    };
    let (root, has_git) = match detected_root {
        Ok(root) => (root, true),
        Err(_) if run.mode == "read" => (
            read_folder.clone().unwrap_or(std::env::current_dir()?),
            false,
        ),
        Err(e) => return Err(e),
    };
    let (base_commit, base_branch) = if has_git {
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
        (base_commit, base_branch)
    } else {
        (String::new(), String::new())
    };
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
    } else if let Some(folder) = read_folder {
        folder
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
        task_type: class.task_type,
        difficulty: class.difficulty,
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
    classify_meta(meta, brief)?;
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
    } else if meta.agent == "copilot" {
        Box::new(copilot::Copilot)
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
                        exhausted: e.code == 4,
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
                worker.notes = if outcome.exhausted {
                    codex::one_line(&outcome.error)
                } else {
                    result::first(&outcome.error, 300)
                };
            }
        }
        let verification = match verify::run(
            dir,
            meta,
            brief,
            &mut progress,
            outcome.timed_out || outcome.exhausted,
        ) {
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
            || meta.agent == "copilot"
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
            outcome.exhausted |= e.code == 4;
            outcome.timed_out |= e.code == 5;
            if outcome.error.is_empty() || e.code == 4 {
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
    if outcome.exhausted {
        worker = worker::failed(codex::one_line(&outcome.error));
        // Copilot refuses at a reserve floor before making a request. Keep its numeric
        // balance usable for routing and a subsequent explicit floor override.
        if meta.agent != "copilot" {
            usage::mark_exhausted(home, &meta.agent, &worker.notes)?;
        }
    }
    let result = result::Result {
        task: meta.task.clone(),
        agent: meta.agent.clone(),
        model: meta.model.clone(),
        effort: meta.effort.clone(),
        status: worker.status,
        verified: verification.verified,
        files_changed: if meta.agent == "copilot" {
            Vec::new()
        } else {
            verification.files
        },
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
    let code = if outcome.exhausted {
        4
    } else if outcome.timed_out {
        5
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
    git::commit(cwd, &brief.subject(&meta.task))
}

fn review(home: &Path, task: &str, by: String) -> Res<i32> {
    let source = home.join("tasks").join(task);
    let _source_lock = Lock::acquire(&source)?;
    let mut meta: Meta = paths::read_json(&source.join("meta.json"))?;
    if meta.closed || meta.branch.is_empty() {
        return Err(Error::usage(
            "review requires an open task with a managed worktree",
        ));
    }
    let original = Brief::parse(fs::read_to_string(source.join("brief.md"))?)?;
    classify_meta(&mut meta, &original)?;
    let cwd = Path::new(&meta.worktree);
    if !git::run(cwd, &["status", "--porcelain"])?.trim().is_empty() {
        return Err(Error::usage("commit or fix task changes before reviewing"));
    }
    let diff = git::run(cwd, &["diff", &meta.base_commit, "--"])?;
    brief::lint(&diff)?;
    // Give the reviewer its own result, logs and thread. Compare against the
    // current commit with no allowed files so any reviewer edits fail verification.
    meta.base_commit = git::run(cwd, &["rev-parse", "HEAD"])?.trim().into();
    meta.task = format!("review-{}", paths::id());
    cli::task_id(&meta.task)?;
    meta.agent = by;
    meta.model = if meta.agent == "agy" {
        "gemini-3.8-flash"
    } else {
        "gpt-6.1-sol"
    }
    .into();
    meta.effort = ["low", "medium", "high", "xhigh"][(if meta.agent == "agy" {
        meta.difficulty.min(3)
    } else {
        meta.difficulty
    } - 1) as usize]
        .into();
    meta.mode = "read".into();
    meta.task_type = "review".into();
    meta.branch.clear();
    meta.thread_id = None;
    meta.rounds = 0;
    meta.auto_fix = 0;
    meta.closed = true;
    let brief = Brief::parse(format!("## Goal\nReview task {task} for bugs, regressions and missing tests. Report findings with file and line references. Do not edit any files.\n## Files\nnone\n## Forbidden\nAll edits.\n## Acceptance\n\n## Context\nOriginal brief and diff are supplied in the review prompt.\n"))?;
    let input = format!(
        "{}\nOriginal brief (reference only):\n{}\n\nDiff to review:\n{diff}",
        brief.text, original.text
    );
    let dir = home.join("tasks").join(&meta.task);
    fs::create_dir(&dir)?;
    let _lock = Lock::acquire(&dir)?;
    fs::write(dir.join("brief.md"), &brief.text)?;
    fs::write(dir.join("review-prompt.md"), &input)?;
    execute(home, &dir, &mut meta, &brief, &input, false, true)
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
        let brief = Brief::parse(fs::read_to_string(dir.join("brief.md"))?)?;
        let subject = brief.subject(task);
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
            git::commit(root, &subject)?;
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
                    "-m",
                    &subject,
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
    let root = setup::agy_root().unwrap_or_else(|_| PathBuf::from("/tmp/baton-wt"));
    let _ = std::fs::create_dir_all(&root);
    std::fs::canonicalize(&root).unwrap_or(root)
}
