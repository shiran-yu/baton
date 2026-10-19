use crate::{
    paths,
    result::{Meta, Tokens},
    status::Progress,
    Error, Res,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub const SCHEMA: &str = include_str!("../plugins/baton/schema/worker-result.json");

pub fn kill_group(child: &mut Child) {
    // A separate process group includes shells and grandchildren, without unsafe libc calls.
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

pub struct WorkerProcess(pub Child);
impl std::ops::Deref for WorkerProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for WorkerProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for WorkerProcess {
    fn drop(&mut self) {
        // Also reap the worker if a state/log write fails while it is running.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            kill_group(&mut self.0);
        }
    }
}

pub fn scrub(command: &mut Command, task: &str) {
    command.env_clear();
    for name in [
        "HOME",
        "PATH",
        "LANG",
        "CODEX_HOME",
        "TMPDIR",
        "USER",
        "SHELL",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("BATON_TASK", task);
}

pub struct Outcome {
    pub timed_out: bool,
    pub success: bool,
    pub error: String,
}

enum Event {
    Stdout(String),
    Stderr(String),
    ReadError(String),
}

pub fn run(
    dir: &Path,
    meta: &mut Meta,
    input: &str,
    resume: bool,
    progress: &mut Progress,
) -> Res<Outcome> {
    fs::write(dir.join("schema.json"), SCHEMA)?;
    let last = dir.join("last.json");
    let mut events = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("events.jsonl"))?;
    if last.exists() {
        fs::remove_file(&last)?;
    }
    let binary = std::env::var_os("BATON_CODEX").unwrap_or_else(|| "codex".into());
    // Resolve relative overrides before changing the worker's cwd.
    let binary = if Path::new(&binary).components().count() > 1 && Path::new(&binary).is_relative()
    {
        std::env::current_dir()?.join(binary).into_os_string()
    } else {
        binary
    };
    let mut command = Command::new(binary);
    command.arg("exec");
    if resume {
        command.arg("resume").arg(
            meta.thread_id
                .as_deref()
                .ok_or_else(|| Error::usage("task has no Codex thread_id"))?,
        );
    }
    command.args([
        "--ignore-user-config",
        "-m",
        &meta.model,
        "-c",
        &format!("model_reasoning_effort=\"{}\"", meta.effort),
        "-c",
        "model_reasoning_summary=\"none\"",
        "-c",
        "model_verbosity=\"low\"",
        "-c",
        "web_search=\"disabled\"",
        "-c",
        "features.apps=false",
        "-s",
        if meta.mode == "read" {
            "read-only"
        } else {
            "workspace-write"
        },
        "-C",
        &meta.worktree,
    ]);
    command
        .arg("--output-schema")
        .arg(dir.join("schema.json"))
        .arg("-o")
        .arg(&last)
        .args(["--json", "-"]);
    scrub(&mut command, &meta.task);
    command
        .current_dir(&meta.worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = WorkerProcess(
        command
            .spawn()
            .map_err(|e| Error::new(1, format!("cannot start Codex: {e}")))?,
    );
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let event = match line {
                    Ok(line) => Event::Stdout(line),
                    Err(e) => Event::ReadError(e.to_string()),
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let event = match line {
                    Ok(line) => Event::Stderr(line),
                    Err(e) => Event::ReadError(e.to_string()),
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let prior = progress.status.tokens.clone();
    let start = Instant::now();
    let mut counted = BTreeSet::new();
    let mut error = String::new();
    let mut failed_event = false;
    let mut timed_out = false;
    let mut disconnected = false;
    let mut exited = None;
    progress.status.phase = "working".into();
    progress.log(if resume {
        "resuming thread"
    } else {
        "worker started"
    })?;
    loop {
        if start.elapsed().as_secs() >= meta.timeout {
            kill_group(&mut child);
            timed_out = true;
            error = "worker timed out".into();
            break;
        }
        if disconnected {
            std::thread::sleep(Duration::from_millis(25));
        } else {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Stdout(line)) => {
                    writeln!(events, "{line}")?;
                    if let Ok(event) = serde_json::from_str::<Value>(&line) {
                        match event["type"].as_str().unwrap_or("") {
                            "thread.started" => {
                                if let Some(id) = event["thread_id"].as_str() {
                                    meta.thread_id = Some(id.into());
                                    paths::write_json(&dir.join("meta.json"), meta)?;
                                }
                            }
                            "item.started" | "item.completed"
                                if event["item"]["type"] == "command_execution" =>
                            {
                                let item = &event["item"];
                                let id = item["id"]
                                    .as_str()
                                    .or(item["command"].as_str())
                                    .unwrap_or("");
                                if counted.insert(id.to_string()) {
                                    progress.status.commands += 1;
                                }
                                let last = display_command(item["command"].as_str().unwrap_or(""));
                                progress.status.last = last.clone();
                                if event["type"] == "item.completed" {
                                    progress
                                        .log(&format!("$ {last}  (exit {})", item["exit_code"]))?;
                                }
                            }
                            "turn.completed" => {
                                let usage = &event["usage"];
                                progress.status.tokens = Tokens {
                                    input: prior.input
                                        + usage["input_tokens"].as_u64().unwrap_or(0),
                                    cached: prior.cached
                                        + usage["cached_input_tokens"].as_u64().unwrap_or(0),
                                    output: prior.output
                                        + usage["output_tokens"].as_u64().unwrap_or(0),
                                };
                            }
                            "turn.failed" | "error" => {
                                failed_event = true;
                                let msg = event["error"]["message"]
                                    .as_str()
                                    .or(event["message"].as_str())
                                    .unwrap_or("Codex turn failed");
                                error = msg.to_string();
                                progress.log(msg)?;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::Stderr(line)) => {
                    progress.log(&line)?;
                    if !failed_event {
                        error = crate::result::tail(&format!("{error}\n{line}"), 3000);
                    }
                }
                Ok(Event::ReadError(e)) => {
                    failed_event = true;
                    error = e;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        progress.update()?;
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        if exited.is_some() && disconnected {
            break;
        }
    }
    Ok(Outcome {
        timed_out,
        success: !timed_out && !failed_event && exited.is_some_and(|s| s.success()),
        error,
    })
}

pub fn display_command(command: &str) -> String {
    let command = command
        .strip_prefix("/bin/zsh -lc ")
        .or_else(|| command.strip_prefix("/bin/bash -lc "))
        .unwrap_or(command);
    let command = if command.len() >= 2
        && ((command.starts_with('"') && command.ends_with('"'))
            || (command.starts_with('\'') && command.ends_with('\'')))
    {
        &command[1..command.len() - 1]
    } else {
        command
    };
    command.to_string()
}

// Capture a shell's combined output to a file: no pipe can fill or hold up timeout handling.
pub fn shell(
    cwd: &Path,
    command: &str,
    timeout: u64,
    output: &Path,
    progress: &mut Progress,
) -> Res<(bool, String)> {
    let file = fs::File::create(output)?;
    let mut child = WorkerProcess(
        Command::new("sh")
            .args(["-c", command])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .process_group(0)
            .spawn()?,
    );
    let start = Instant::now();
    let mut timed_out = false;
    let passed = loop {
        progress.update()?;
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if start.elapsed().as_secs() >= timeout {
            kill_group(&mut child);
            timed_out = true;
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut file = fs::File::open(output)?;
    use std::io::{Seek, SeekFrom};
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(4096)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if timed_out {
        text.push_str("\nacceptance command timed out");
    }
    fs::remove_file(output)?;
    Ok((passed, crate::result::tail(&text, 300)))
}
