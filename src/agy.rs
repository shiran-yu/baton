use crate::{
    codex::{self, Outcome, WorkerProcess},
    git, paths,
    result::{Meta, Tokens, Worker as Report},
    status::Progress,
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
}

fn agent(meta: &Meta) -> Res<()> {
    if meta.base_commit.is_empty() {
        return Err(Error::usage(
            "agy requires a git repository for agent setup",
        ));
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
    let mut tools = vec!["view_file", "list_dir", "find_by_name", "grep_search"];
    if meta.mode == "edit" {
        tools.extend([
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ]);
    }
    tools.push("finish");
    let tools = tools
        .iter()
        .map(|tool| format!("  - {tool}\n"))
        .collect::<String>();
    fs::create_dir_all(cwd.join(".agents/agents"))?;
    fs::write(cwd.join(relative), format!("---\nname: baton-worker\ndescription: Bounded baton worker\nexcludeDefaultComponents: true\ninheritCustomizations: false\nsubagent: false\ntools:\n{tools}---\nStay inside the files listed in the brief. Never run commands; baton runs the acceptance commands. Call finish with the result object.\n"))?;
    Ok(())
}

impl Agy {
    pub fn new(dir: &Path, meta: &Meta) -> Res<Self> {
        agent(meta)?;
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
            "baton-worker",
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
    Tokens {
        premium_requests: None,
        input: usage["input_tokens"].as_u64().unwrap_or(0),
        cached: usage["cache_read_tokens"].as_u64().unwrap_or(0),
        output: usage["output_tokens"].as_u64().unwrap_or(0)
            + usage["thinking_tokens"].as_u64().unwrap_or(0),
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
