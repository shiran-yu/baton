use crate::{
    brief,
    codex::{self, Outcome, WorkerProcess},
    git, paths, process,
    result::{Meta, Tokens, Worker as Report},
    status::Progress,
    usage,
    worker::{self, Worker},
    Error, Res,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

enum Event {
    Out(String),
    Err(String),
    ReadError(String),
}

pub struct Agy {
    child: Option<WorkerProcess>,
    input: Option<ChildStdin>,
    events: Receiver<Event>,
    dir: PathBuf,
    timeout: u64,
    denied: Option<String>,
    counted: BTreeSet<(String, u64)>,
    usage: BTreeSet<(String, u64)>,
    tokens: Tokens,
    preamble: Option<String>,
}

// Strongest first. The Claude 4.6 models draw on the 3p-weekly bucket and leave agy on
// 2026-11-02; Gemini 3.8 Flash stays.
// pub const MODELS: [&str; 3] = ["claude-opus-4-6-thinking", "claude-sonnet-4-6", GEMINI];
// pub const GEMINI: &str = "gemini-3.8-flash";

// Model policy (user 2026-11-13, Pro plan); route.rs and main.rs read it from here.
// - Argon is the top tier (Claude's strongest): judgement work runs Argon while its own weekly
//   bucket allows, else Hydrogen.
// - Hydrogen is Sonnet's counterpart: tool work (mechanical, docs, research sweeps, D1) runs on it.
// - Every model runs at the highest effort `agy models` lists for it (Argon high, Hydrogen xhigh
//   today); Flash, at its highest, is the last fallback. The caller's --model and --effort win.
pub const ARGON: &str = "gemini-4-argon";
pub const HYDROGEN: &str = "gemini-4-hydrogen";
pub const FLASH: &str = "gemini-3.8-flash";
// Strongest first: a model agy does not list falls back to the next one it does.
pub const MODELS: [&str; 3] = [ARGON, HYDROGEN, FLASH];
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Tool-like work goes to Hydrogen; a review is always judgement.
pub fn tool_work(task_type: &str, difficulty: u8) -> bool {
    task_type != "review"
        && (["mechanical", "docs", "research"].contains(&task_type) || difficulty <= 1)
}

/// The policy's models for a task, preferred first.
pub fn tiers(task_type: &str, difficulty: u8) -> Vec<&'static str> {
    if tool_work(task_type, difficulty) {
        vec![HYDROGEN]
    } else {
        vec![ARGON, HYDROGEN]
    }
}

/// Whether a `/usage` bucket meters a model. On Pro, Argon has its own weekly bucket; the other
/// Gemini models share the Gemini group's 5 h and weekly buckets; Claude and GPT the 3p group's.
pub fn meters(model: &str, bucket: &str) -> bool {
    let argon = bucket.starts_with("gemini-argon");
    if model.starts_with(ARGON) {
        argon
    } else if model.starts_with("claude-") || model.starts_with("gpt-") {
        bucket.starts_with("3p-")
    } else {
        bucket.starts_with("gemini-") && !argon
    }
}

// `agy models`, one slug per line.
fn offered() -> Vec<String> {
    process::capture(
        Command::new(process::executable("agy")).arg("models"),
        Duration::from_secs(30),
    )
    .map(|out| {
        out.lines()
            .filter_map(|line| line.split('\t').next())
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}

/// The model and effort of a run, checked against `agy models` (free, about 3 s).
pub fn choose(model: &str, effort: Option<String>) -> (String, Option<String>) {
    // let listed: Vec<String> = process::capture(
    //     Command::new(process::executable("agy")).arg("models"),
    //     Duration::from_secs(30),
    // )
    // .map(|out| {
    //     out.lines()
    //         .filter_map(|line| line.split('\t').next())
    //         .map(String::from)
    //         .collect()
    // })
    // .unwrap_or_default();
    pick(model, effort, &offered())
}

/// The policy's first model that agy lists and `allowed` (the quota) passes, else Flash, each at
/// its highest listed effort.
pub fn policy(tiers: &[&str], allowed: impl Fn(&str) -> bool) -> (String, Option<String>) {
    first(tiers, allowed, &offered())
}

// An empty list (agy models failed) leaves only the quota to decide.
fn first(
    tiers: &[&str],
    allowed: impl Fn(&str) -> bool,
    listed: &[String],
) -> (String, Option<String>) {
    let offered = |model: &str| {
        listed.is_empty() || listed.iter().any(|s| s == model) || !efforts(listed, model).is_empty()
    };
    let model = tiers
        .iter()
        .find(|model| offered(model) && allowed(model))
        .unwrap_or(&FLASH);
    pick(model, None, listed)
}

// A model from MODELS falls back to the next one agy lists; any other model is kept. The effort
// is the given one, else the highest listed as `<model>-<effort>`; a model listed only bare takes
// none (agy refuses --effort for it). An empty list changes nothing.
fn pick(model: &str, effort: Option<String>, listed: &[String]) -> (String, Option<String>) {
    let bare = |m: &str| listed.iter().any(|s| s == m);
    let model = MODELS
        .iter()
        .position(|m| *m == model)
        .and_then(|i| {
            MODELS[i..]
                .iter()
                .find(|m| bare(m) || !efforts(listed, m).is_empty())
        })
        .copied()
        .unwrap_or(model);
    let levels = efforts(listed, model);
    let effort = if levels.is_empty() && bare(model) {
        None
    } else {
        effort.or_else(|| {
            EFFORTS
                .iter()
                .rev()
                .find(|e| levels.contains(e))
                .map(|e| e.to_string())
        })
    };
    (model.into(), effort)
}

fn efforts<'a>(listed: &'a [String], model: &str) -> Vec<&'a str> {
    listed
        .iter()
        .filter_map(|s| s.strip_prefix(model)?.strip_prefix('-'))
        .filter(|e| EFFORTS.contains(e))
        .collect()
}

const LISTED: usize = 200;
// Directory entries the ## Files walk may visit outside git.
#[cfg(not(test))]
const WALK: usize = 50_000;
#[cfg(test)]
const WALK: usize = 300;

// agy is never told its workspace otherwise and resolves relative brief paths by searching from /.
fn preamble(meta: &Meta, files: &[String]) -> String {
    let root = Path::new(&meta.worktree);
    // let mut text = format!(
    //     "Workspace root: {root}\nEvery path in this brief is relative to {root}. Never list, search or read anything outside it.\n",
    //     root = meta.worktree
    // );
    // Skills live in the installed plugins; without the exception a literal model (Hydrogen
    // medium, 2026-11-13) refused to open them.
    let mut text = format!(
        "Workspace root: {root}\nEvery path in this brief is relative to {root}. Never list, search or read anything outside it, except a skill's SKILL.md and the files it links to under {plugins}.\n",
        root = meta.worktree,
        plugins = usage::user_home().join(".gemini/config/plugins").display()
    );
    // Read agents have no listing or file search tools, so name the files explicitly.
    if meta.mode == "read" {
        let (paths, more) = listed(root, files);
        if !paths.is_empty() {
            text.push_str("Files in scope:\n");
            for path in paths {
                text.push_str(&format!("- {}\n", root.join(path).display()));
            }
        }
        if more {
            text.push_str(&format!(
                "This file list is incomplete (at most {LISTED} paths); use grep_search inside the root for the rest.\n"
            ));
        }
    }
    text.push('\n');
    text
}

// Expand the brief's ## Files globs to root-relative paths: git's file list, else a bounded walk.
fn listed(root: &Path, files: &[String]) -> (Vec<String>, bool) {
    let patterns: Vec<String> = files
        .iter()
        .filter_map(|line| {
            // A Files line may carry a note after the path.
            let pattern = line.split(|c: char| c == '`' || c.is_whitespace()).next()?;
            let pattern = match Path::new(pattern).strip_prefix(root) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) if pattern.starts_with('/') => return None,
                Err(_) => pattern.into(),
            };
            let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
            (!pattern.is_empty()).then(|| pattern.to_string())
        })
        .collect();
    if patterns.is_empty() {
        return (Vec::new(), false);
    }
    let (candidates, mut more): (Vec<String>, bool) =
        match git::run(root, &["ls-files", "-co", "--exclude-standard", "-z"]) {
            Ok(out) => (
                out.split('\0')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
                false,
            ),
            Err(_) => {
                // Start at each pattern's leading literal directory, not at the root.
                let mut starts: Vec<String> = patterns
                    .iter()
                    .map(|p| {
                        p.split('/')
                            .take_while(|part| !part.contains(['*', '?']))
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .collect();
                starts.sort();
                starts.dedup();
                let mut kept: Vec<String> = Vec::new();
                for start in starts {
                    if !kept
                        .iter()
                        .any(|k| k.is_empty() || start.starts_with(&format!("{k}/")))
                    {
                        kept.push(start);
                    }
                }
                let (mut found, mut budget, mut complete) = (Vec::new(), WALK, true);
                let base = fs::canonicalize(root).unwrap_or_else(|_| root.into());
                for start in kept {
                    // A start through a symlink or .. would leave the root.
                    if fs::canonicalize(root.join(&start)).ok() != Some(base.join(&start)) {
                        continue;
                    }
                    match fs::metadata(root.join(&start)) {
                        Ok(kind) if kind.is_dir() => {
                            let prefix = if start.is_empty() {
                                start
                            } else {
                                format!("{start}/")
                            };
                            complete &= walk(
                                &root.join(&prefix),
                                &prefix,
                                &patterns,
                                &mut found,
                                &mut budget,
                            );
                        }
                        Ok(_) => found.push(start),
                        Err(_) => {}
                    }
                }
                (found, !complete)
            }
        };
    let mut matched = candidates.into_iter().filter(|path| {
        patterns
            .iter()
            .any(|p| brief::glob(p, path) || brief::glob(&format!("{p}/**"), path))
    });
    let paths = matched.by_ref().take(LISTED).collect();
    more |= matched.next().is_some();
    (paths, more)
}

// Whether a file under dir can match pattern; a matched directory covers everything under it.
fn reaches(pattern: &str, dir: &str) -> bool {
    let mut parts = pattern.split('/');
    for name in dir.split('/') {
        match parts.next() {
            None => return true,
            Some(part) if part.contains("**") => return true,
            Some(part) if !brief::glob(part, name) => return false,
            _ => {}
        }
    }
    true
}

// Returns false when the entry budget ran out.
fn walk(
    dir: &Path,
    prefix: &str,
    patterns: &[String],
    found: &mut Vec<String>,
    budget: &mut usize,
) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return true;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        // file_type does not follow symlinks, so the walk never leaves the root.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if ![".git", "target", "node_modules"].contains(&name.as_str())
                && patterns.iter().any(|p| reaches(p, &path))
                && !walk(&entry.path(), &format!("{path}/"), patterns, found, budget)
            {
                return false;
            }
        } else {
            found.push(path);
        }
    }
    true
}

// Returns the agent name for --agent.
fn agent(meta: &Meta) -> Res<&'static str> {
    let mut tools = vec!["view_file"];
    if meta.mode == "edit" {
        tools.extend([
            "list_dir",
            "find_by_name",
            "grep_search",
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ]);
    } else {
        tools.push("grep_search");
    }
    tools.push("finish");
    let tools = tools
        .iter()
        .map(|tool| format!("  - {tool}\n"))
        .collect::<String>();
    if meta.mode == "read" {
        // Read runs write nothing into the user's folder or repo; agy also loads its global agents dir.
        let home = usage::user_home();
        if home.as_os_str().is_empty() {
            return Err(Error::usage("HOME must be set for agy read mode"));
        }
        let dir = home.join(".gemini/config/agents");
        let path = dir.join("baton-read-worker.md");
        // The user's skills, rules, plugins and hooks load, and agy's default prompt sections list
        // the skills; agy still declares only these tools plus its manage_task (measured 2026-10-29).
        // let content = format!("---\nname: baton-read-worker\ndescription: Read-only baton worker\nexcludeDefaultComponents: false\ninheritCustomizations: true\nsubagent: false\ntools:\n{tools}---\nRead only inside the workspace root named in the first message. Never run commands. Call finish with the result object.\n");
        let content = format!("---\nname: baton-read-worker\ndescription: Read-only baton worker\nexcludeDefaultComponents: false\ninheritCustomizations: true\nsubagent: false\ntools:\n{tools}---\nRead only inside the workspace root named in the first message, and the skill files it allows. Never run commands. Call finish with the result object.\n");
        if fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            fs::create_dir_all(&dir)?;
            // Rename so a concurrent agy never reads a half-written agent.
            let temp = dir.join(format!(".baton-read-worker.{}.tmp", std::process::id()));
            fs::write(&temp, &content)?;
            fs::rename(&temp, &path)?;
        }
        return Ok("baton-read-worker");
    }
    let cwd = Path::new(&meta.worktree);
    let relative = ".agents/agents/baton-worker.md";
    let common = git::run(cwd, &["rev-parse", "--git-common-dir"])?;
    let common = cwd.join(common.trim());
    fs::create_dir_all(common.join("info"))?;
    let exclude = common.join("info/exclude");
    let content = fs::read_to_string(&exclude).unwrap_or_default();
    let pattern = format!("/{relative}");
    if !content.lines().any(|line| line == pattern) {
        // A leading newline also handles an existing file without a final newline.
        writeln!(
            OpenOptions::new().create(true).append(true).open(exclude)?,
            "\n{pattern}"
        )?;
    }
    fs::create_dir_all(cwd.join(".agents/agents"))?;
    fs::write(cwd.join(relative), format!("---\nname: baton-worker\ndescription: Bounded baton worker\nexcludeDefaultComponents: false\ninheritCustomizations: true\nsubagent: false\ntools:\n{tools}---\nStay inside the files listed in the brief. Never run commands; baton runs the acceptance commands. Call finish with the result object.\n"))?;
    Ok("baton-worker")
}

impl Agy {
    pub fn new(dir: &Path, meta: &Meta, files: &[String]) -> Res<Self> {
        let name = agent(meta)?;
        fs::write(dir.join("schema.json"), codex::SCHEMA)?;
        let binary = std::env::var_os("BATON_AGY").unwrap_or_else(|| "agy".into());
        let binary =
            if Path::new(&binary).is_relative() && Path::new(&binary).components().count() > 1 {
                std::env::current_dir()?.join(binary).into_os_string()
            } else {
                binary
            };
        let mut command = Command::new(binary);
        command.args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--agent",
            name,
            "--model",
            &meta.model,
        ]);
        if !meta.effort.is_empty() {
            command.args(["--effort", &meta.effort]);
        }
        command
            .args(["--mode", "accept-edits", "--json-schema"])
            .arg(dir.join("schema.json"))
            .arg("--print-timeout")
            .arg(format!("{}s", meta.timeout));
        // Reads under the root are then auto-allowed even when it is not a trusted workspace.
        if meta.mode == "read" {
            command.arg("--add-dir").arg(&meta.worktree);
        }
        codex::scrub(&mut command, &meta.task);
        if let Some(value) = std::env::var_os("XDG_CONFIG_HOME") {
            command.env("XDG_CONFIG_HOME", value);
        }
        let mut child = WorkerProcess(
            command
                .current_dir(&meta.worktree)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .map_err(|e| Error::new(1, format!("cannot start agy: {e}")))?,
        );
        let input = child.stdin.take();
        let (tx, events) = mpsc::channel();
        fn reader(stream: impl Read + Send + 'static, tx: mpsc::Sender<Event>, stderr: bool) {
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let event = match line {
                        Ok(s) if stderr => Event::Err(s),
                        Ok(s) => Event::Out(s),
                        Err(e) => Event::ReadError(e.to_string()),
                    };
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(out) = child.stdout.take() {
            reader(out, tx.clone(), false);
        }
        if let Some(err) = child.stderr.take() {
            reader(err, tx.clone(), true);
        }
        drop(tx);
        Ok(Self {
            child: Some(child),
            input,
            events,
            dir: dir.into(),
            timeout: meta.timeout,
            denied: None,
            counted: BTreeSet::new(),
            usage: BTreeSet::new(),
            tokens: Tokens::default(),
            preamble: Some(preamble(meta, files)),
        })
    }

    fn event(
        &mut self,
        event: Event,
        meta: Option<&mut Meta>,
        progress: &mut Progress,
    ) -> Res<Option<(Outcome, Report)>> {
        let line = match event {
            Event::Err(line) => {
                progress.log(&line)?;
                if codex::quota_error("agy", &line) {
                    return Ok(Some(exhausted(&line)));
                }
                if line.contains("jetski: no output produced") {
                    self.denied = Some(line);
                }
                return Ok(None);
            }
            Event::ReadError(e) => return Err(Error::new(1, e)),
            Event::Out(line) => line,
        };
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join("events.jsonl"))?,
            "{line}"
        )?;
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            if codex::quota_error("agy", &line) {
                return Ok(Some(exhausted(&line)));
            }
            return Ok(None);
        };
        let kind = event["event"]
            .as_str()
            .or(event["type"].as_str())
            .unwrap_or("");
        if ["error", "AGY_ERROR", "result"].contains(&kind) || event.get("error").is_some() {
            if let Some(message) = quota_message(&event) {
                return Ok(Some(exhausted(message)));
            }
        }
        match event["event"].as_str().unwrap_or("") {
            "init" => {
                if let (Some(meta), Some(id)) = (meta, event["conversation_id"].as_str()) {
                    meta.thread_id = Some(id.into());
                    paths::write_json(&self.dir.join("meta.json"), meta)?;
                }
            }
            "step_update" => {
                let step = &event["step_update"];
                let key = (
                    step["conversation_id"].as_str().unwrap_or("").to_string(),
                    step["step_index"].as_u64().unwrap_or(0),
                );
                if step["step_type"] == "tool" {
                    if self.counted.insert(key.clone()) {
                        progress.status.commands += 1;
                    }
                    progress.status.last = step["tool_name"]
                        .as_str()
                        .or(step["tool_info"]["name"].as_str())
                        .unwrap_or("tool")
                        .into();
                }
                if step["state"] == "ERROR" {
                    if let Some(message) = step["tool_info"]["error"]["message"].as_str() {
                        if message.to_lowercase().contains("permission check failed") {
                            self.denied = Some(message.into());
                        }
                    }
                }
                if step["usage"].is_object() && self.usage.insert(key) {
                    let tokens = tokens(&step["usage"]);
                    self.tokens.input += tokens.input;
                    self.tokens.cached += tokens.cached;
                    self.tokens.output += tokens.output;
                    progress.status.tokens = self.tokens.clone();
                }
            }
            "result" => {
                let result = &event["result"];
                // Result usage is cumulative for the session; never add it to step usage.
                if result["usage"].is_object() {
                    self.tokens = tokens(&result["usage"]);
                }
                progress.status.tokens = self.tokens.clone();
                if result["denied_actions"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                {
                    self.denied = Some(format!("denied_actions: {}", result["denied_actions"]));
                }
                let timed_out = result["status"].as_str().is_some_and(|s| {
                    s.to_uppercase().contains("TIMEOUT") || s.to_uppercase().contains("TIMED_OUT")
                });
                let success = result["status"] == "SUCCESS";
                let error = if timed_out {
                    "worker timed out".into()
                } else if !success {
                    result["error"]["message"]
                        .as_str()
                        .unwrap_or("agy turn failed")
                        .to_string()
                } else {
                    String::new()
                };
                let report = if let Some(message) = &self.denied {
                    Report {
                        status: "blocked".into(),
                        notes: message.clone(),
                        questions: Vec::new(),
                    }
                } else if !result["structured_output"].is_object() {
                    worker::failed("no structured_output")
                } else {
                    worker::parse_report(&result["structured_output"].to_string())
                };
                fs::write(
                    self.dir.join("last.json"),
                    result["structured_output"].to_string(),
                )?;
                return Ok(Some((
                    Outcome {
                        success,
                        timed_out,
                        exhausted: false,
                        error,
                    },
                    report,
                )));
            }
            _ => {}
        }
        Ok(None)
    }

    fn terminate(&mut self, progress: &mut Progress) -> Res<()> {
        self.input.take();
        if let Some(child) = self.child.as_mut() {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", "--", &format!("-{}", child.id())])
                .status();
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => {
                    self.event(event, None, progress)?;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            progress.update()?;
            // Wait the grace period even if the group leader exits: descendants may ignore TERM.
        }
        if let Some(child) = self.child.as_mut() {
            codex::kill_group(child);
        }
        self.child.take();
        Ok(())
    }
}

impl Worker for Agy {
    fn turn(
        &mut self,
        _dir: &Path,
        meta: &mut Meta,
        input: &str,
        _resume: bool,
        progress: &mut Progress,
    ) -> Res<(Outcome, Report)> {
        progress.status.phase = "working".into();
        progress.log("worker turn started")?;
        let mut stdin = self
            .input
            .take()
            .ok_or_else(|| Error::new(1, "agy session closed"))?;
        let input = format!("{}{input}", self.preamble.take().unwrap_or_default());
        let line = json!({"event":"user", "message":{"content":input}}).to_string();
        // A worker that stops reading must not block the supervisor's timeout loop.
        let (written, writing) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = writeln!(stdin, "{line}")
                .and_then(|_| stdin.flush())
                .map(|()| stdin);
            let _ = written.send(outcome);
        });
        let mut result = None;
        let start = Instant::now();
        loop {
            if self.input.is_none() {
                match writing.try_recv() {
                    Ok(stdin) => self.input = Some(stdin?),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(Error::new(1, "agy input writer stopped"))
                    }
                }
            }
            if self.input.is_some() {
                if let Some(result) = result.take() {
                    return Ok(result);
                }
            }
            if start.elapsed() >= Duration::from_secs(meta.timeout) {
                self.terminate(progress)?;
                return Ok((
                    Outcome {
                        timed_out: true,
                        exhausted: false,
                        success: false,
                        error: "worker timed out".into(),
                    },
                    worker::failed("worker timed out"),
                ));
            }
            match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => {
                    if let Some(turn) = self.event(event, Some(meta), progress)? {
                        if turn.0.exhausted {
                            self.input.take();
                            self.child.take();
                            return Ok(turn);
                        }
                        if turn.0.timed_out {
                            self.terminate(progress)?;
                            return Ok(turn);
                        }
                        result = Some(turn);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if result.is_none() {
                        return Err(Error::new(1, "agy exited without a result"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            progress.update()?;
        }
    }

    fn finish(&mut self, progress: &mut Progress) -> Res<Option<String>> {
        self.input.take();
        let start = Instant::now();
        let mut disconnected = false;
        while self.child.is_some() {
            match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => {
                    if let Some((outcome, _)) = self.event(event, None, progress)? {
                        if outcome.exhausted {
                            self.child.take();
                            return Err(Error::new(4, outcome.error));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            progress.update()?;
            if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                if disconnected {
                    self.child.take();
                    if !status.success() && self.denied.is_none() {
                        return Err(Error::new(1, "agy process failed"));
                    }
                }
            }
            if start.elapsed() >= Duration::from_secs(self.timeout) {
                self.terminate(progress)?;
                return Err(Error::new(5, "worker timed out closing agy session"));
            }
        }
        Ok(self.denied.clone())
    }
}

fn tokens(usage: &Value) -> Tokens {
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    // agy 1.5.7 counts thinking inside output_tokens (total = input + output, 2026-11-13), so
    // output + thinking counted it twice; total - input holds under either convention.
    let output = match usage["total_tokens"].as_u64() {
        Some(total) if total >= input => total - input,
        _ => {
            usage["output_tokens"].as_u64().unwrap_or(0)
                + usage["thinking_tokens"].as_u64().unwrap_or(0)
        }
    };
    Tokens {
        premium_requests: None,
        // input: usage["input_tokens"].as_u64().unwrap_or(0),
        input,
        cached: usage["cache_read_tokens"].as_u64().unwrap_or(0),
        // output: usage["output_tokens"].as_u64().unwrap_or(0)
        //     + usage["thinking_tokens"].as_u64().unwrap_or(0),
        output,
    }
}

fn quota_message(value: &Value) -> Option<&str> {
    match value {
        Value::String(message) if codex::quota_error("agy", message) => Some(message),
        Value::Object(fields) => {
            let matched = fields.values().find_map(quota_message)?;
            Some(
                fields
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(matched),
            )
        }
        Value::Array(values) => values.iter().find_map(quota_message),
        _ => None,
    }
}

fn exhausted(message: &str) -> (Outcome, Report) {
    let message = codex::one_line(message);
    (
        Outcome {
            timed_out: false,
            exhausted: true,
            success: false,
            error: message.clone(),
        },
        worker::failed(message),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // Until 2026-11-13 the list was Claude Opus 4.6, Claude Sonnet 4.6, Gemini 3.8 Flash:
    // #[test]
    // fn strongest_listed_model_and_its_highest_effort() {
    //     let list = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    //     // `agy models` on 2026-10-29.
    //     let today = list("gemini-3.8-flash-high gemini-3.8-flash-medium gemini-3.8-flash-low claude-sonnet-4-6 claude-opus-4-6-thinking gpt-oss-120b-medium");
    //     let after = list("gemini-3.8-flash-high gemini-3.8-flash-medium gemini-3.8-flash-low");
    //     let opus = MODELS[0].to_string();
    //     let gemini = (GEMINI.to_string(), Some("high".to_string()));
    //     assert_eq!(pick(MODELS[0], None, &today), (opus.clone(), None));
    //     // agy refuses --effort for a model listed bare.
    //     assert_eq!(pick(MODELS[0], Some("low".into()), &today), (opus, None));
    //     assert_eq!(pick(MODELS[0], None, &after), gemini);
    //     assert_eq!(pick(GEMINI, None, &today), gemini);
    //     assert_eq!(
    //         pick(GEMINI, Some("low".into()), &after),
    //         (GEMINI.into(), Some("low".into()))
    //     );
    //     // A model outside baton's list is kept; an empty list changes nothing.
    //     assert_eq!(pick("custom", None, &today), ("custom".into(), None));
    //     assert_eq!(pick(MODELS[0], None, &[]), (MODELS[0].into(), None));
    // }

    fn list(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }
    // `agy models` on 2026-11-13 (Pro plan).
    const PRO: &str = "gemini-4-argon-medium gemini-4-argon-high gemini-4-argon-low gemini-4-hydrogen-high gemini-4-hydrogen-xhigh gemini-4-hydrogen-medium gemini-4-hydrogen-low gemini-pro-orchestrator-g1-pro gemini-3.8-flash-high gemini-3.8-flash-medium gemini-3.8-flash-low claude-opus-5-5-low claude-opus-5-5-medium claude-opus-5-5-high claude-sonnet-5-5-low claude-sonnet-5-5-medium claude-sonnet-5-5-high gpt-oss-120b-medium";

    #[test]
    fn explicit_model_falls_back_at_its_highest_listed_effort() {
        let pro = list(PRO);
        let flash = list("gemini-3.8-flash-high gemini-3.8-flash-medium gemini-3.8-flash-low");
        let some = |m: &str, e: &str| (m.to_string(), Some(e.to_string()));
        assert_eq!(pick(ARGON, None, &pro), some(ARGON, "high"));
        assert_eq!(pick(HYDROGEN, None, &pro), some(HYDROGEN, "xhigh"));
        // An argon-xhigh listed later is picked up; the caller's effort wins.
        let later = list(&format!("{PRO} gemini-4-argon-xhigh"));
        assert_eq!(pick(ARGON, None, &later), some(ARGON, "xhigh"));
        assert_eq!(pick(HYDROGEN, Some("medium".into()), &pro), some(HYDROGEN, "medium"));
        assert_eq!(pick("claude-opus-5-5", None, &pro), some("claude-opus-5-5", "high"));
        // A model agy no longer lists falls back along MODELS, keeping the caller's effort.
        assert_eq!(pick(ARGON, None, &flash), some(FLASH, "high"));
        assert_eq!(pick(ARGON, Some("low".into()), &flash), some(FLASH, "low"));
        // A model outside baton's list is kept; a bare one gets no effort; an empty list changes nothing.
        assert_eq!(pick("custom", None, &pro), ("custom".into(), None));
        assert_eq!(pick("gpt-oss-120b-medium", Some("low".into()), &pro), ("gpt-oss-120b-medium".into(), None));
        assert_eq!(pick(ARGON, None, &[]), (ARGON.into(), None));
    }

    #[test]
    fn thinking_is_counted_once() {
        // A result usage of agy 1.5.7 (Argon, 2026-11-13).
        let t = tokens(&json!({"input_tokens":25108,"output_tokens":3273,"thinking_tokens":2434,"cache_read_tokens":10205,"total_tokens":28381}));
        assert_eq!((t.input, t.cached, t.output), (25108, 10205, 3273));
        // Without a total, thinking is added as before.
        let t = tokens(&json!({"input_tokens":100,"output_tokens":10,"thinking_tokens":3}));
        assert_eq!(t.output, 13);
    }

    #[test]
    fn policy_tiers_quota_and_last_fallback() {
        let pro = list(PRO);
        let some = |m: &str, e: &str| (m.to_string(), Some(e.to_string()));
        let all = |_: &str| true;
        let judgement = tiers("implement", 2);
        assert_eq!(first(&judgement, all, &pro), some(ARGON, "high"));
        assert_eq!(first(&tiers("review", 1), all, &pro), some(ARGON, "high"));
        // Argon used up: Hydrogen stands in, at its highest effort.
        assert_eq!(first(&judgement, |m: &str| m != ARGON, &pro), some(HYDROGEN, "xhigh"));
        for (kind, d) in [("mechanical", 1), ("docs", 1), ("research", 2), ("bugfix", 1)] {
            assert_eq!(first(&tiers(kind, d), all, &pro), some(HYDROGEN, "xhigh"), "{kind}");
        }
        // Neither listed, or no quota for either: Flash at its highest.
        let flash = list("gemini-3.8-flash-high gemini-3.8-flash-medium gemini-3.8-flash-low");
        assert_eq!(first(&judgement, all, &flash), some(FLASH, "high"));
        assert_eq!(first(&judgement, |_: &str| false, &pro), some(FLASH, "high"));
        // agy models failed: the quota alone decides, and agy picks the effort.
        assert_eq!(first(&judgement, all, &[]), (ARGON.to_string(), None));
        assert!(meters(ARGON, "gemini-argon-wtu-weekly") && !meters(ARGON, "gemini-weekly"));
        for bucket in ["gemini-weekly", "gemini-5h"] {
            assert!(meters(HYDROGEN, bucket) && meters(FLASH, bucket) && !meters(ARGON, bucket));
        }
        assert!(meters(HYDROGEN, "gemini-weekly") && !meters(HYDROGEN, "gemini-argon-wtu-weekly"));
        assert!(meters("claude-opus-5-5", "3p-5h") && !meters("claude-opus-5-5", "gemini-5h"));
    }

    #[test]
    fn listed_files_are_capped_and_stay_under_the_root() {
        let root = std::env::temp_dir().join(format!("baton-listed-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("node_modules/src")).unwrap();
        for i in 0..205 {
            fs::write(root.join(format!("src/{i:03}.rs")), "").unwrap();
        }
        fs::write(root.join("docs/a.md"), "").unwrap();
        let (paths, more) = listed(&root, &["src/**".into()]);
        assert_eq!((paths.len(), more), (LISTED, true));
        assert_eq!(paths[0], "src/000.rs");
        let absolute = format!("{}/docs/", root.display());
        let (paths, more) = listed(
            &root,
            &[
                "docs/a.md` the notes".into(),
                absolute,
                "/etc/passwd".into(),
            ],
        );
        assert_eq!((paths, more), (vec!["docs/a.md".to_string()], false));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn walk_starts_at_the_literal_directory_and_flags_truncation() {
        let root = std::env::temp_dir().join(format!("baton-walk-{}", std::process::id()));
        // A decoy larger than the walk budget sorts before src/.
        for dir in ["aaa", "src/a", "src/b/deep"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for i in 0..WALK + 10 {
            fs::write(root.join(format!("aaa/{i:04}.txt")), "").unwrap();
            fs::write(root.join(format!("src/b/deep/{i:04}.rs")), "").unwrap();
        }
        fs::write(root.join("src/a/mod.rs"), "").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        let (paths, more) = listed(&root, &["src/*.rs".into(), "src/*/mod.rs".into()]);
        assert_eq!(
            (paths, more),
            (
                vec!["src/a/mod.rs".to_string(), "src/lib.rs".to_string()],
                false
            )
        );
        // A root-level wildcard walks from the root and reports the exhausted budget.
        let (paths, more) = listed(&root, &["**/*.rs".into()]);
        assert!(paths.is_empty() && more);
        let (_, more) = listed(&root, &["../**".into()]);
        assert!(!more);
        fs::remove_dir_all(&root).unwrap();
    }
}
