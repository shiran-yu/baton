use crate::{
    codex::{self, Outcome, WorkerProcess},
    paths,
    result::{Meta, Worker as Report},
    status::Progress,
    usage,
    worker::Worker,
    Error, Res,
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

const TAIL: &str = r#"

You are read-only. Inspect the files needed for this brief; never edit, create, or delete files.
End your reply with ONE JSON object of this shape:
{"status": "done"|"blocked"|"needs_decision", "notes": "...", "questions": [...]}
notes is the whole answer the Goal asks for, in the shape it asks for, with every number and its file:line;
the lead reads nothing else. Put any decisions needed from the lead in questions.
"#;

pub fn model() -> String {
    // "auto": the student plan rejects every explicit --model; auto picks from what the account may use.
    std::env::var("BATON_COPILOT_MODEL").unwrap_or_else(|_| "auto".into())
}

pub fn floor() -> f64 {
    std::env::var("BATON_COPILOT_FLOOR")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(20.0)
}

fn quota_guard() -> Res<()> {
    let home = paths::home()?;
    usage::read(&home, false)?;
    let quota = crate::quota::Usage::cached(&home)
        .quota("copilot", false)
        .map_err(|e| Error::new(4, format!("cannot check copilot premium quota: {e}")))?;
    if quota.remaining < floor() {
        return Err(Error::new(4, format!(
            "copilot premium quota has {} requests remaining, below floor {}; refusing to risk billed overage",
            quota.remaining, floor()
        )));
    }
    Ok(())
}

fn command(meta: &Meta) -> Res<Command> {
    let binary = std::env::var_os("BATON_COPILOT").unwrap_or_else(|| "copilot".into());
    let binary = if Path::new(&binary).is_relative() && Path::new(&binary).components().count() > 1
    {
        std::env::current_dir()?.join(binary).into_os_string()
    } else {
        binary
    };
    // Explicit test hook: integration tests use a fake worker, never a real writable Copilot.
    if std::env::var("BATON_COPILOT_NO_NS").as_deref() == Ok("1") {
        return Ok(Command::new(binary));
    }
    static NAMESPACE: OnceLock<bool> = OnceLock::new();
    if !*NAMESPACE.get_or_init(codex::probe_private_tmp) {
        return Err(Error::new(1,
            "copilot requires a working unshare user/mount namespace for read-only execution; refusing to run writable"));
    }
    let mut command = Command::new("unshare");
    command.args(["-rm", "sh", "-c",
        "mount -t tmpfs tmpfs /tmp && mount --bind \"$1\" \"$1\" && mount -o remount,bind,ro \"$1\" && shift && exec \"$@\"",
        "sh", &meta.worktree]).arg(binary);
    Ok(command)
}

// Walk complete outer objects rather than matching braces inside JSON strings or nested values.
fn report(message: &str, success: bool) -> Report {
    let mut last = None;
    let mut offset = 0;
    while let Some(index) = message[offset..].find('{') {
        let start = offset + index;
        let mut stream = serde_json::Deserializer::from_str(&message[start..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            offset = start + stream.byte_offset();
            if value.is_object() {
                last = Some(value);
            }
        } else {
            offset = start + 1;
        }
    }
    if let Some(value) = last {
        if let Ok(report) = serde_json::from_value::<Report>(value) {
            if ["done", "blocked", "needs_decision"].contains(&report.status.as_str()) {
                return report;
            }
        }
    }
    Report {
        status: if success { "done" } else { "failed" }.into(),
        notes: message.into(),
        questions: Vec::new(),
    }
}

enum Event {
    Out(String),
    Err(String),
    ReadError(String),
}
fn reader(stream: impl Read + Send + 'static, tx: mpsc::Sender<Event>, stderr: bool) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let event = match line {
                Ok(line) if stderr => Event::Err(line),
                Ok(line) => Event::Out(line),
                Err(e) => Event::ReadError(e.to_string()),
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
}

pub struct Copilot;
impl Worker for Copilot {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        _resume: bool,
        progress: &mut Progress,
    ) -> Res<(Outcome, Report)> {
        if meta.mode != "read" {
            return Err(Error::usage("copilot runs read briefs only"));
        }
        quota_guard()?;
        let mut command = command(meta)?;
        command.args(["--model", &meta.model]);
        // Copilot refuses a reasoning effort for "auto".
        if meta.model != "auto" {
            command.args(["--reasoning-effort", &meta.effort]);
        }
        command
            .args([
                "--allow-all-tools",
                "--no-ask-user",
                "--no-custom-instructions",
                "--disable-builtin-mcps",
                "--output-format",
                "json",
                "--usage-output-file",
            ])
            .arg(dir.join("usage.json"));
        codex::scrub(&mut command, &meta.task);
        for name in [
            "COPILOT_HOME",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "COPILOT_GITHUB_TOKEN",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.env("COPILOT_AUTO_UPDATE", "false");
        let mut child = WorkerProcess(
            command
                .current_dir(&meta.worktree)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .map_err(|e| Error::new(1, format!("cannot start copilot: {e}")))?,
        );
        if let Some(mut stdin) = child.stdin.take() {
            let prompt = format!("{input}{TAIL}");
            std::thread::spawn(move || {
                let _ = stdin.write_all(prompt.as_bytes());
            });
        }
        let (tx, rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            reader(stdout, tx.clone(), false);
        }
        if let Some(stderr) = child.stderr.take() {
            reader(stderr, tx.clone(), true);
        }
        drop(tx);
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        let start = Instant::now();
        let mut message = String::new();
        let mut error = String::new();
        let mut result_code = None;
        let mut disconnected = false;
        let mut exited = None;
        let mut timed_out = false;
        let prior = progress.status.tokens.premium_requests.unwrap_or(0.0);
        progress.status.tokens.premium_requests = Some(prior);
        progress.status.phase = "working".into();
        progress.log("copilot started (read-only)")?;
        loop {
            if start.elapsed().as_secs() >= meta.timeout {
                codex::kill_group(&mut child);
                timed_out = true;
                error = "worker timed out".into();
                break;
            }
            if disconnected {
                std::thread::sleep(Duration::from_millis(25));
            } else {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Event::Out(line)) => {
                        writeln!(events, "{line}")?;
                        if let Ok(event) = serde_json::from_str::<Value>(&line) {
                            let data = &event["data"];
                            match event["type"].as_str().unwrap_or("") {
                                "assistant.turn_start" => {
                                    progress.status.last = "assistant turn".into();
                                }
                                "tool.execution_start" => {
                                    progress.status.commands += 1;
                                    progress.status.last =
                                        data["toolName"].as_str().unwrap_or("tool").into();
                                    progress.log(&format!("tool: {}", progress.status.last))?;
                                }
                                "tool.execution_complete" => {
                                    progress
                                        .log(&format!("tool completed: {}", data["toolCallId"]))?;
                                }
                                "assistant.message" => {
                                    if let Some(content) = data["content"].as_str() {
                                        message = content.into();
                                        progress.status.last = crate::result::first(content, 120);
                                    }
                                }
                                "session.usage_checkpoint" => {
                                    if let Some(n) = data["totalPremiumRequests"].as_f64() {
                                        progress.status.tokens.premium_requests = Some(prior + n);
                                    }
                                }
                                "result" => {
                                    result_code = event["exitCode"].as_i64();
                                    if let Some(n) = event["usage"]["premiumRequests"].as_f64() {
                                        progress.status.tokens.premium_requests = Some(prior + n);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Event::Err(line)) => {
                        progress.log(&line)?;
                        error = crate::result::tail(&format!("{error}\n{line}"), 3000);
                    }
                    Ok(Event::ReadError(e)) => return Err(Error::new(1, e)),
                    Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            progress.update()?;
            if exited.is_none() {
                exited = child.try_wait()?;
            }
            if disconnected && exited.is_some() {
                break;
            }
        }
        let success = !timed_out && result_code == Some(0) && exited.is_some_and(|s| s.success());
        fs::write(dir.join("last.json"), &message)?;
        let report = report(&message, success);
        // Preserve the entire assistant message on fallback, including non-JSON failures.
        if !success && error.trim().is_empty() && message.is_empty() {
            error =
                format!("copilot exited without a successful result (exitCode {result_code:?})");
        }
        if !timed_out && !message.is_empty() {
            error.clear();
        }
        Ok((
            Outcome {
                success,
                timed_out,
                exhausted: false,
                error: error.trim().into(),
            },
            report,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_object_handles_nested_and_escaped_braces() {
        let r = report(
            r#"Earlier {"status":"blocked"} then {"status":"done","notes":"brace } and \"quoted\"","questions":["what?"]} trailing"#,
            true,
        );
        assert_eq!(r.status, "done");
        assert_eq!(r.notes, "brace } and \"quoted\"");
        assert_eq!(report("plain", true).notes, "plain");
        assert_eq!(report("plain", false).status, "failed");
    }
}
