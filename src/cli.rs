use crate::{Error, Res};
use std::path::PathBuf;

pub enum Cli {
    Sync(crate::sync::Options),
    HookAdapt(crate::hook_adapt::Options),
    Run(Run),
    Route(Route),
    Scorecard { since: Option<u64>, json: bool },
    Review { task: String, by: String },
    Supervise(String),
    Wait { task: String, max: u64 },
    Fix { task: String, notes: PathBuf },
    Ps { json: bool },
    Show { task: String, view: String },
    Accept { task: String, squash: bool },
    Drop(String),
    Usage { json: bool, refresh: bool },
    Statusline(String),
    Doctor { quick: bool, out: Option<PathBuf> },
    Setup(crate::setup::Options),
    Help,
}

pub struct Run {
    pub agent: String,
    pub detach: bool,
    pub auto_fix: u32,
    pub brief: PathBuf,
    pub mode: String,
    pub effort: String,
    pub model: String,
    pub task: Option<String>,
    pub base: String,
    pub timeout: u64,
    pub no_worktree: bool,
    pub json: bool,
    pub task_type: Option<String>,
    pub difficulty: Option<u8>,
}

pub struct Route {
    pub brief: PathBuf,
    pub task_type: Option<String>,
    pub difficulty: Option<u8>,
    pub json: bool,
}

fn parse_type(value: String) -> Res<String> {
    if !crate::route::TYPES.contains(&value.as_str()) {
        return Err(Error::usage("invalid --type"));
    }
    Ok(value)
}

fn parse_difficulty(value: String) -> Res<u8> {
    value
        .parse()
        .ok()
        .filter(|d| (1..=4).contains(d))
        .ok_or_else(|| Error::usage("--difficulty must be 1 through 4"))
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
        "sync" => {
            let mut options = crate::sync::Options::default();
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--dry-run" => options.dry_run = true,
                    "--include-trading" => options.include_trading = true,
                    "--only" => {
                        options.only = value(&mut args, &flag)?
                            .split(',')
                            .map(String::from)
                            .collect();
                        for name in &options.only {
                            task_id(name)?;
                        }
                    }
                    "--to" => {
                        options.hosts = value(&mut args, &flag)?
                            .split(',')
                            .map(String::from)
                            .collect();
                        if options
                            .hosts
                            .iter()
                            .any(|host| !["codex", "agy"].contains(&host.as_str()))
                        {
                            return Err(Error::usage("--to must be codex, agy or codex,agy"));
                        }
                        options.hosts.dedup();
                    }
                    _ => return Err(Error::usage(format!("unknown option: {flag}"))),
                }
            }
            Ok(Cli::Sync(options))
        }
        "hook-adapt" => {
            let (mut host, mut event) = (String::new(), String::new());
            let mut command = Vec::new();
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--from" => host = value(&mut args, &flag)?,
                    "--event" => event = value(&mut args, &flag)?,
                    "--" => {
                        command.extend(args);
                        break;
                    }
                    _ => return Err(Error::usage(format!("unknown option: {flag}"))),
                }
            }
            if !["agy", "codex"].contains(&host.as_str()) || event.is_empty() || command.is_empty()
            {
                return Err(Error::usage("usage: baton hook-adapt --from <agy|codex> --event <ClaudeEvent> -- CMD [ARGS...]"));
            }
            Ok(Cli::HookAdapt(crate::hook_adapt::Options {
                host,
                event,
                command,
            }))
        }
        "--help" | "-h" | "help" => Ok(Cli::Help),
        "setup" => crate::setup::parse(args).map(Cli::Setup),
        "usage" => {
            let (mut json, mut refresh) = (false, false);
            for flag in args {
                match flag.as_str() {
                    "--json" => json = true,
                    "--refresh" => refresh = true,
                    _ => return Err(Error::usage("usage: baton usage [--json] [--refresh]")),
                }
            }
            Ok(Cli::Usage { json, refresh })
        }
        "statusline" => {
            let mode = value(&mut args, "statusline")?;
            if !["line", "subagent"].contains(&mode.as_str()) || args.next().is_some() {
                return Err(Error::usage("usage: baton statusline <line|subagent>"));
            }
            Ok(Cli::Statusline(mode))
        }
        "doctor" => {
            let (mut quick, mut scan, mut out) = (false, false, None);
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--quick" => quick = true,
                    "--scan" => scan = true,
                    "--out" => out = Some(PathBuf::from(value(&mut args, "--out")?)),
                    _ => {
                        return Err(Error::usage(
                            "usage: baton doctor [--quick | --scan --out DIR]",
                        ))
                    }
                }
            }
            if scan != out.is_some() || (quick && scan) {
                return Err(Error::usage(
                    "usage: baton doctor [--quick | --scan --out DIR]",
                ));
            }
            Ok(Cli::Doctor { quick, out })
        }
        "run" => {
            let agent = args.next().unwrap_or_default();
            if !["codex", "agy", "auto"].contains(&agent.as_str()) {
                return Err(Error::usage("worker must be codex, agy or auto"));
            }
            let mut run = Run {
                model: if agent == "agy" {
                    "gemini-3.8-flash"
                } else {
                    "gpt-6.1-sol"
                }
                .into(),
                agent,
                detach: false,
                auto_fix: 1,
                brief: PathBuf::new(),
                mode: "edit".into(),
                effort: "medium".into(),
                task: None,
                base: "HEAD".into(),
                timeout: 1800,
                no_worktree: false,
                json: false,
                task_type: None,
                difficulty: None,
            };
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--brief" => run.brief = value(&mut args, &flag)?.into(),
                    "--type" => run.task_type = Some(parse_type(value(&mut args, &flag)?)?),
                    "--difficulty" => {
                        run.difficulty = Some(parse_difficulty(value(&mut args, &flag)?)?)
                    }
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
                    "--detach" => run.detach = true,
                    "--auto-fix" => {
                        run.auto_fix = value(&mut args, &flag)?.parse().map_err(|_| {
                            Error::usage("--auto-fix must be a non-negative integer")
                        })?
                    }
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
            if run.agent == "agy" && run.effort == "xhigh" {
                return Err(Error::usage("agy effort must be low, medium or high"));
            }
            if run.model.is_empty() || run.base.is_empty() || run.base.starts_with('-') {
                return Err(Error::usage("invalid model or base ref"));
            }
            Ok(Cli::Run(run))
        }
        "route" => {
            let mut route = Route {
                brief: PathBuf::new(),
                task_type: None,
                difficulty: None,
                json: false,
            };
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--brief" => route.brief = value(&mut args, &flag)?.into(),
                    "--type" => route.task_type = Some(parse_type(value(&mut args, &flag)?)?),
                    "--difficulty" => {
                        route.difficulty = Some(parse_difficulty(value(&mut args, &flag)?)?)
                    }
                    "--json" => route.json = true,
                    _ => return Err(Error::usage(format!("unknown option: {flag}"))),
                }
            }
            if route.brief.as_os_str().is_empty() {
                return Err(Error::usage("--brief is required"));
            }
            Ok(Cli::Route(route))
        }
        "scorecard" => {
            let mut since = None;
            let mut json = false;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--since" => {
                        since = Some(value(&mut args, &flag)?.parse().map_err(|_| {
                            Error::usage("--since must be a non-negative number of days")
                        })?)
                    }
                    "--json" => json = true,
                    _ => return Err(Error::usage(format!("unknown option: {flag}"))),
                }
            }
            Ok(Cli::Scorecard { since, json })
        }
        "review" => {
            let task = value(&mut args, "task")?;
            task_id(&task)?;
            if args.next().as_deref() != Some("--by") {
                return Err(Error::usage("usage: baton review <task> --by <codex|agy>"));
            }
            let by = value(&mut args, "--by")?;
            if !["codex", "agy"].contains(&by.as_str()) || args.next().is_some() {
                return Err(Error::usage("usage: baton review <task> --by <codex|agy>"));
            }
            Ok(Cli::Review { task, by })
        }
        "wait" | "__supervise" => {
            let task = value(&mut args, "task")?;
            task_id(&task)?;
            let mut max = 570;
            if cmd == "wait" && args.peek().is_some() {
                if args.next().as_deref() != Some("--max") {
                    return Err(Error::usage("usage: baton wait <task> [--max SECS]"));
                }
                max = value(&mut args, "--max")?
                    .parse()
                    .map_err(|_| Error::usage("--max must be a non-negative integer"))?;
            }
            if args.next().is_some() {
                return Err(Error::usage("unexpected argument"));
            }
            Ok(if cmd == "wait" {
                Cli::Wait { task, max }
            } else {
                Cli::Supervise(task)
            })
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
