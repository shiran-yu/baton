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

/// Whether the top mount on /tmp is a bind of a subdirectory (pam_namespace, e.g. Isambard login nodes).
fn tmp_is_bind(mountinfo: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            (fields.len() > 4 && fields[4] == "/tmp").then(|| fields[3] != "/")
        })
        .last()
        .unwrap_or(false)
}

/// Codex 0.163's read-only and workspace-write sandboxes fail on such a /tmp ("bwrap: Can't mkdir
/// parents for /local/tmp-inst/<user>/codex-daemon-<uid>"); run it in a user+mount namespace with
/// a tmpfs /tmp instead, when the kernel allows one.
fn private_tmp() -> bool {
    tmp_is_bind(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default())
        && probe_private_tmp()
}

pub(crate) fn probe_private_tmp() -> bool {
    Command::new("unshare")
        .args(["-rm", "sh", "-c", "mount -t tmpfs tmpfs /tmp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub struct Outcome {
    pub timed_out: bool,
    pub exhausted: bool,
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
    // let mut command = Command::new(binary);
    let mut command = if private_tmp() {
        // Codex's bwrap cannot mask its socket dir under a polyinstantiated /tmp: give it a plain one.
        let mut wrapped = Command::new("unshare");
        wrapped
            .args([
                "-rm",
                "sh",
                "-c",
                "mount -t tmpfs tmpfs /tmp && exec \"$0\" \"$@\"",
            ])
            .arg(binary);
        wrapped
    } else {
        Command::new(binary)
    };
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
    ]);
    let sandbox = if meta.mode == "read" {
        "read-only"
    } else {
        "workspace-write"
    };
    if resume {
        // `codex exec resume` takes neither -s, -C nor -o: the sandbox goes through config, the
        // working directory is the process cwd, and the final message is read from the events.
        command.args(["-c", &format!("sandbox_mode=\"{sandbox}\"")]);
    } else {
        command.args(["-s", sandbox, "-C", &meta.worktree]);
    }
    // A read task outside git has no base commit; codex refuses such a folder without this flag.
    if meta.base_commit.is_empty() {
        command.arg("--skip-git-repo-check");
    }
    command.arg("--output-schema").arg(dir.join("schema.json"));
    if !resume {
        command.arg("-o").arg(&last);
    }
    command.args(["--json", "-"]);
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
    let mut final_message = String::new();
    let mut error = String::new();
    let mut failed_event = false;
    let mut timed_out = false;
    let mut exhausted = false;
    let mut rollout = Rollout::default();
    let mut completed = false;
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
                            "item.completed" if event["item"]["type"] == "agent_message" => {
                                if let Some(text) = event["item"]["text"].as_str() {
                                    final_message = text.to_string();
                                }
                            }
                            "turn.completed" => {
                                completed = true;
                                let usage = &event["usage"];
                                progress.status.tokens = Tokens {
                                    premium_requests: None,
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
                                    .or(event["error"].as_str())
                                    .unwrap_or("Codex turn failed");
                                error = one_line(msg);
                                exhausted |= quota_error("codex", msg);
                                progress.log(msg)?;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::Stderr(line)) => {
                    progress.log(&line)?;
                    if quota_error("codex", &line) {
                        exhausted = true;
                        failed_event = true;
                        error = one_line(&line);
                    }
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
        rollout.poll(meta.thread_id.as_deref(), progress, !completed);
        progress.update()?;
        if exhausted {
            kill_group(&mut child);
            break;
        }
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        if exited.is_some() && disconnected {
            break;
        }
    }
    // With --output-schema the last agent message is the result; `exec resume` has no -o.
    if !last.exists() && !final_message.is_empty() {
        fs::write(&last, &final_message)?;
    }
    Ok(Outcome {
        timed_out,
        exhausted,
        success: !timed_out && !failed_event && exited.is_some_and(|s| s.success()),
        error,
    })
}

pub fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn quota_error(agent: &str, message: &str) -> bool {
    let lower = message.to_lowercase();
    let markers: &[&str] = if agent == "agy" {
        &[
            "resource_exhausted",
            "individual quota reached",
            "credits balance is too low",
        ]
    } else {
        &["usage limit", "rate limit", "quota"]
    };
    markers.iter().any(|s| lower.contains(s))
        || ["authentication required", "not logged in"]
            .iter()
            .any(|s| lower.contains(s))
}

#[derive(Default)]
struct Rollout {
    file: Option<fs::File>,
    pending: Vec<u8>,
    last_read: Option<Instant>,
}

// UTC civil date from days since the Unix epoch, using Gregorian 400-year eras.
fn session_date(epoch: u64) -> String {
    let days = (epoch / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}/{month:02}/{day:02}")
}

impl Rollout {
    fn poll(&mut self, thread: Option<&str>, progress: &mut Progress, live_tokens: bool) {
        let Some(thread) = thread else { return };
        if self
            .last_read
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        self.last_read = Some(Instant::now());
        if self.file.is_none() {
            let base = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| crate::usage::user_home().join(".codex"));
            let now = paths::now();
            for date in [now, now.saturating_sub(86400)] {
                let Ok(entries) = fs::read_dir(base.join("sessions").join(session_date(date)))
                else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("rollout-") && name.ends_with(&format!("-{thread}.jsonl")) {
                        self.file = fs::File::open(entry.path()).ok();
                        break;
                    }
                }
                if self.file.is_some() {
                    break;
                }
            }
        }
        if let Some(file) = self.file.as_mut() {
            // Retain an incomplete JSON line until the next poll.
            let _ = file.read_to_end(&mut self.pending);
            let mut consumed = 0;
            for (index, byte) in self.pending.iter().enumerate() {
                if *byte != b'\n' {
                    continue;
                }
                if let Ok(event) = serde_json::from_slice::<Value>(&self.pending[consumed..index]) {
                    if event["type"] == "event_msg" && event["payload"]["type"] == "token_count" {
                        let payload = &event["payload"];
                        let usage = &payload["info"]["total_token_usage"];
                        if live_tokens && usage.is_object() {
                            progress.status.tokens = Tokens {
                                premium_requests: None,
                                input: usage["input_tokens"].as_u64().unwrap_or(0),
                                cached: usage["cached_input_tokens"].as_u64().unwrap_or(0),
                                output: usage["output_tokens"].as_u64().unwrap_or(0),
                            };
                        }
                        let rates = &payload["rate_limits"];
                        let quota: Vec<_> = ["primary", "secondary"]
                            .iter()
                            .filter_map(|key| {
                                let w = &rates[key];
                                let used = w["used_percent"].as_f64()?;
                                let minutes = w["window_minutes"].as_u64()?;
                                let label = match minutes {
                                    10080 => "wk".into(),
                                    m if m % 60 == 0 => format!("{}h", m / 60),
                                    m => format!("{m}m"),
                                };
                                Some(format!("{label} {used:.0}%"))
                            })
                            .collect();
                        if !quota.is_empty() {
                            progress.status.quota = quota.join(" · ");
                        }
                    }
                }
                consumed = index + 1;
            }
            self.pending.drain(..consumed);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::tmp_is_bind;

    #[test]
    fn polyinstantiated_tmp_is_detected() {
        // Isambard login44: a tmpfs on /tmp, then the per-user bind on top of it.
        let login = "2038 2036 0:43 /tmp /tmp rw,nodev - tmpfs tmpfs rw\n\
                     2469 2038 254:0 /tmp-inst/u /tmp rw,relatime - ext4 /dev/mapper/local rw\n";
        assert!(tmp_is_bind(login));
        assert!(!tmp_is_bind(
            "30 1 0:26 / /tmp rw,nosuid - tmpfs tmpfs rw\n"
        ));
        assert!(!tmp_is_bind("25 1 8:1 / / rw - ext4 /dev/sda1 rw\n"));
    }
}
