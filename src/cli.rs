use crate::{Error, Res};
use std::path::PathBuf;

pub enum Cli {
    Run(Run),
    Fix { task: String, notes: PathBuf },
    Ps { json: bool },
    Show { task: String, view: String },
    Accept { task: String, squash: bool },
    Drop(String),
    Help,
}

pub struct Run {
    pub brief: PathBuf,
    pub mode: String,
    pub effort: String,
    pub model: String,
    pub task: Option<String>,
    pub base: String,
    pub timeout: u64,
    pub no_worktree: bool,
    pub json: bool,
}

pub fn task_id(id: &str) -> Res<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.len() > 100
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(Error::usage(
            "task ID must contain only letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(())
}

pub fn parse(args: impl Iterator<Item = String>) -> Res<Cli> {
    let mut args = args.peekable();
    let cmd = args.next().unwrap_or_else(|| "--help".into());
    let value = |args: &mut std::iter::Peekable<_>, flag: &str| -> Res<String> {
        args.next()
            .ok_or_else(|| Error::usage(format!("{flag} requires a value")))
    };
    match cmd.as_str() {
        "--help" | "-h" | "help" => Ok(Cli::Help),
        "run" => {
            if args.next().as_deref() != Some("codex") {
                return Err(Error::usage("only 'run codex' is supported"));
            }
            let mut run = Run {
                brief: PathBuf::new(),
                mode: "edit".into(),
                effort: "medium".into(),
                model: "gpt-6.1-sol".into(),
                task: None,
                base: "HEAD".into(),
                timeout: 1800,
                no_worktree: false,
                json: false,
            };
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--brief" => run.brief = value(&mut args, &flag)?.into(),
                    "--mode" => run.mode = value(&mut args, &flag)?,
                    "--effort" => run.effort = value(&mut args, &flag)?,
                    "--model" => run.model = value(&mut args, &flag)?,
                    "--task" => {
                        let id = value(&mut args, &flag)?;
                        task_id(&id)?;
                        run.task = Some(id);
                    }
                    "--base" => run.base = value(&mut args, &flag)?,
                    "--timeout" => {
                        run.timeout = value(&mut args, &flag)?
                            .parse()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| Error::usage("--timeout must be a positive integer"))?
                    }
                    "--no-worktree" => run.no_worktree = true,
                    "--json" => run.json = true,
                    _ => return Err(Error::usage(format!("unknown option: {flag}"))),
                }
            }
            if run.brief.as_os_str().is_empty() {
                return Err(Error::usage("--brief is required"));
            }
            if !["edit", "read"].contains(&run.mode.as_str()) {
                return Err(Error::usage("--mode must be edit or read"));
            }
            if !["low", "medium", "high", "xhigh"].contains(&run.effort.as_str()) {
                return Err(Error::usage("invalid --effort"));
            }
            if run.model.is_empty() || run.base.is_empty() || run.base.starts_with('-') {
                return Err(Error::usage("invalid model or base ref"));
            }
            Ok(Cli::Run(run))
        }
        "ps" => {
            let json = match args.next().as_deref() {
                None => false,
                Some("--json") => true,
                _ => return Err(Error::usage("usage: baton ps [--json]")),
            };
            if args.next().is_some() {
                return Err(Error::usage("usage: baton ps [--json]"));
            }
            Ok(Cli::Ps { json })
        }
        "fix" | "show" | "accept" | "drop" => {
            let task = value(&mut args, "task")?;
            task_id(&task)?;
            let command = match cmd.as_str() {
                "fix" => {
                    if args.next().as_deref() != Some("--notes") {
                        return Err(Error::usage("usage: baton fix <task> --notes FILE"));
                    }
                    Cli::Fix {
                        task,
                        notes: value(&mut args, "--notes")?.into(),
                    }
                }
                "show" => {
                    let view = args.next().unwrap_or_default();
                    if !["", "--diff", "--log", "--brief"].contains(&view.as_str()) {
                        return Err(Error::usage("invalid show option"));
                    }
                    Cli::Show { task, view }
                }
                "accept" => {
                    let squash = match args.next().as_deref() {
                        None => false,
                        Some("--squash") => true,
                        _ => return Err(Error::usage("invalid accept option")),
                    };
                    Cli::Accept { task, squash }
                }
                _ => Cli::Drop(task),
            };
            if args.next().is_some() {
                return Err(Error::usage("unexpected argument"));
            }
            Ok(command)
        }
        _ => Err(Error::usage(format!("unknown command: {cmd}"))),
    }
}
