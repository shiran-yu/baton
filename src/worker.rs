// use crate::{
//     codex,
//     result::{Meta, Worker as Report},
//     status::Progress,
//     Res,
// };
// use std::{fs, path::Path};
use crate::{
    codex,
    copilot::{self, Event},
    paths, process,
    result::{Meta, Tokens, Worker as Report},
    status::Progress,
    Error, Res,
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::{mpsc, OnceLock},
    time::{Duration, Instant},
};

pub trait Worker {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        resume: bool,
        progress: &mut Progress,
    ) -> Res<(codex::Outcome, Report)>;
    fn finish(&mut self, _progress: &mut Progress) -> Res<Option<String>> {
        Ok(None)
    }
}

pub struct Codex;
impl Worker for Codex {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        resume: bool,
        progress: &mut Progress,
    ) -> Res<(codex::Outcome, Report)> {
        let outcome = codex::run(dir, meta, input, resume, progress)?;
        let raw = fs::read_to_string(dir.join("last.json")).unwrap_or_default();
        Ok((outcome, parse_report(&raw)))
    }
}

pub fn parse_report(raw: &str) -> Report {
    serde_json::from_str::<Report>(raw)
        .ok()
        .filter(|w| ["done", "partial", "blocked", "needs_decision"].contains(&w.status.as_str()))
        .unwrap_or_else(|| failed(crate::result::first(raw, 300)))
}
pub fn failed(notes: impl Into<String>) -> Report {
    Report {
        status: "failed".into(),
        notes: notes.into(),
        questions: Vec::new(),
    }
}

/// opencode on DeepSeek v4.1 Flash, free tier: shown as "deepseek", always at max (user 2026-11-22).
pub const OPENCODE_MODEL: &str = "opencode/deepseek-v4.1-flash-free";
pub const OPENCODE_ALIAS: &str = "deepseek";
pub const OPENCODE_EFFORT: &str = "max";

const OPENCODE_TAIL: &str = r#"

End your reply with ONE JSON object of this shape:
{"status": "done"|"partial"|"blocked"|"needs_decision", "notes": "...", "questions": [...]}
notes is what you did, or the whole answer the Goal asks for with file:line for every number;
the lead reads nothing else. Put any decisions needed from the lead in questions.
"#;

/// The model opencode takes: the alias means DeepSeek; any other --model passes through.
pub fn opencode_slug(model: &str) -> String {
    if model.is_empty() || model == OPENCODE_ALIAS {
        OPENCODE_MODEL.into()
    } else {
        model.into()
    }
}

// opencode has no sandbox: it runs in a user+mount namespace with $HOME read-only except its own
// folders; the worktree is writable in edit mode, read-only in read mode.
fn opencode_command(meta: &Meta) -> Res<Command> {
    let binary = process::executable("opencode");
    // Test hook: integration tests use a fake worker, no namespace.
    if std::env::var("BATON_OPENCODE_NO_NS").as_deref() == Ok("1") {
        return Ok(Command::new(binary));
    }
    static NAMESPACE: OnceLock<bool> = OnceLock::new();
    if !*NAMESPACE.get_or_init(codex::probe_private_tmp) {
        return Err(Error::new(
            1,
            "opencode requires a working unshare user/mount namespace; refusing to run unsandboxed",
        ));
    }
    let home = std::env::var("HOME").map_err(|_| Error::new(1, "HOME is not set"))?;
    let mut command = Command::new("unshare");
    // cd after the mounts: a cwd taken before them would still reach the writable tree.
    command.args([
        "-rm",
        "sh",
        "-c",
        "h=$1; w=$2; m=$3; shift 3; \
         mount --bind \"$h\" \"$h\" && mount -o remount,bind,ro \"$h\" && \
         mount --bind \"$w\" \"$w\" && mount -o remount,bind,$m \"$w\" || exit 1; \
         while [ \"$1\" != -- ]; do mount --bind \"$1\" \"$1\" && mount -o remount,bind,rw \"$1\" || exit 1; shift; done; \
         shift; cd \"$w\" && exec \"$@\"",
        "sh",
        home.as_str(),
        meta.worktree.as_str(),
        if meta.mode == "read" { "ro" } else { "rw" },
    ]);
    for dir in [".local/share/opencode", ".local/state/opencode", ".cache/opencode"] {
        let dir = Path::new(&home).join(dir);
        // A folder missing now could not be created under the read-only $HOME.
        fs::create_dir_all(&dir)?;
        command.arg(dir);
    }
    command.arg("--").arg(binary);
    Ok(command)
}

pub struct OpenCode;
impl Worker for OpenCode {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        resume: bool,
        progress: &mut Progress,
    ) -> Res<(codex::Outcome, Report)> {
        let mut command = opencode_command(meta)?;
        let model = if meta.effort.is_empty() {
            opencode_slug(&meta.model)
        } else {
            format!("{}#{}", opencode_slug(&meta.model), meta.effort)
        };
        // --archived keeps baton's sessions out of the user's session list.
        command.args(["run", "--model", &model, "--format", "json", "--auto", "--archived"]);
        if resume {
            let session = meta
                .thread_id
                .as_deref()
                .ok_or_else(|| Error::usage("task has no opencode session"))?;
            command.args(["--session", session]);
        }
        codex::scrub(&mut command, &meta.task);
        let mut child = codex::WorkerProcess(
            command
                .current_dir(&meta.worktree)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .map_err(|e| Error::new(1, format!("cannot start opencode: {e}")))?,
        );
        // The prompt goes through stdin: one argv string is capped at 128 KiB.
        if let Some(mut stdin) = child.stdin.take() {
            let read_only = if meta.mode == "read" {
                "\n\nYou are read-only. Inspect the files needed for this brief; never edit, create, or delete files."
            } else {
                ""
            };
            let prompt = format!("{input}{read_only}{OPENCODE_TAIL}");
            std::thread::spawn(move || {
                let _ = stdin.write_all(prompt.as_bytes());
            });
        }
        let (tx, rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            copilot::reader(stdout, tx.clone(), false);
        }
        if let Some(stderr) = child.stderr.take() {
            copilot::reader(stderr, tx.clone(), true);
        }
        drop(tx);
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        let prior = progress.status.tokens.clone();
        let mut used = Tokens::default();
        let start = Instant::now();
        let mut message = String::new();
        let mut error = String::new();
        let mut failed_event = false;
        let mut exhausted = false;
        let mut timed_out = false;
        let mut disconnected = false;
        let mut exited = None;
        progress.status.phase = "working".into();
        progress.log(if resume {
            "resuming opencode session"
        } else {
            "opencode started"
        })?;
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
                            // The session id is what a fix round resumes.
                            if let Some(id) = event["sessionID"].as_str().filter(|s| !s.is_empty()) {
                                if meta.thread_id.as_deref() != Some(id) {
                                    meta.thread_id = Some(id.into());
                                    paths::write_json(&dir.join("meta.json"), meta)?;
                                }
                            }
                            let part = &event["part"];
                            match event["type"].as_str().unwrap_or("") {
                                "tool_use" => {
                                    progress.status.commands += 1;
                                    let args = &part["state"]["input"];
                                    let target = args["command"]
                                        .as_str()
                                        .or(args["filePath"].as_str())
                                        .or(args["path"].as_str())
                                        .unwrap_or("");
                                    progress.status.last = codex::display_command(&format!(
                                        "{} {target}",
                                        part["tool"].as_str().unwrap_or("tool")
                                    ));
                                    progress.log(&format!("tool: {}", progress.status.last))?;
                                }
                                "text" => {
                                    if let Some(text) =
                                        part["text"].as_str().filter(|t| !t.trim().is_empty())
                                    {
                                        message = text.into();
                                        progress.status.last = crate::result::first(text, 120);
                                    }
                                }
                                "step_finish" => {
                                    // input excludes cache reads and writes; reasoning is billed as output.
                                    let t = &part["tokens"];
                                    let n = |v: &Value| v.as_u64().unwrap_or(0);
                                    used.input += n(&t["input"])
                                        + n(&t["cache"]["read"])
                                        + n(&t["cache"]["write"]);
                                    used.cached += n(&t["cache"]["read"]);
                                    used.output += n(&t["output"]) + n(&t["reasoning"]);
                                    progress.status.tokens = Tokens {
                                        premium_requests: None,
                                        input: prior.input + used.input,
                                        cached: prior.cached + used.cached,
                                        output: prior.output + used.output,
                                    };
                                }
                                "error" => {
                                    failed_event = true;
                                    let msg = event["error"]["message"]
                                        .as_str()
                                        .or(event["error"]["data"]["message"].as_str())
                                        .unwrap_or("opencode failed");
                                    error = codex::one_line(msg);
                                    exhausted |= codex::quota_error("opencode", msg);
                                    progress.log(msg)?;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Event::Err(line)) => {
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
            if exhausted {
                codex::kill_group(&mut child);
                break;
            }
            if exited.is_none() {
                exited = child.try_wait()?;
            }
            if disconnected && exited.is_some() {
                break;
            }
        }
        let success = !timed_out
            && !failed_event
            && !message.is_empty()
            && exited.is_some_and(|s| s.success());
        fs::write(dir.join("last.json"), &message)?;
        if !success && error.trim().is_empty() {
            error = "opencode exited without a reply".into();
        }
        Ok((
            codex::Outcome {
                success,
                timed_out,
                exhausted,
                error: error.trim().into(),
            },
            copilot::report(&message, success),
        ))
    }
}
