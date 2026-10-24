use crate::{paths, process, usage, Res};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::Command,
    time::Duration,
};

fn installed(agent: &str) -> bool {
    let executable = process::executable(agent);
    let path = Path::new(&executable);
    let executable_file = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if path.components().count() > 1 {
        return executable_file(path);
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| executable_file(&p.join(path))))
}
pub fn quick() {
    let mut missing = Vec::new();
    for agent in ["codex", "agy"] {
        // An explicit executable override is sufficient for the hook's lightweight check.
        if std::env::var_os(format!("BATON_{}", agent.to_uppercase())).is_none()
            && !installed(agent)
        {
            missing.push(format!("{agent} on PATH"));
        }
    }
    let writable = paths::home().ok().is_some_and(|home| {
        let mut parent = home.as_path();
        while !parent.exists() {
            let Some(next) = parent.parent() else {
                return false;
            };
            parent = next;
        }
        // A create/remove probe verifies effective permissions, ACLs and read-only mounts.
        // Check permission bits too so this remains useful for privileged invocations.
        if !fs::metadata(parent).is_ok_and(|m| m.is_dir() && m.mode() & 0o222 != 0) {
            return false;
        }
        let probe = parent.join(format!(".baton-write-check-{}", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = fs::remove_file(probe);
                true
            }
            Err(_) => false,
        }
    });
    if !writable {
        missing.push("writable BATON_HOME".into());
    }
    if !missing.is_empty() {
        println!("baton doctor: missing {}", missing.join(", "));
    }
}
fn version(agent: &str) -> String {
    if !installed(agent) {
        return format!("{agent}: not installed\n");
    }
    match process::capture(
        Command::new(process::executable(agent)).arg("--version"),
        Duration::from_secs(10),
    ) {
        Ok(v) => format!("{agent}: {}\n", v.trim()),
        Err(e) => format!("{agent}: {e}\n"),
    }
}
fn versions() -> String {
    let mut output = format!("baton: {}\n", env!("CARGO_PKG_VERSION"));
    for agent in ["claude", "codex", "agy", "copilot"] {
        output.push_str(&version(agent));
    }
    output
}
pub fn run(home: &Path, out: Option<&Path>) -> Res<i32> {
    if let Some(out) = out {
        fs::create_dir_all(out)?;
        fs::write(out.join("versions.txt"), versions())?;
        for agent in ["claude", "codex", "agy", "copilot"] {
            if installed(agent) {
                let dir = out.join(agent);
                fs::create_dir_all(&dir)?;
                scan(agent, &dir, &[], &mut HashSet::new())?;
            }
        }
        return Ok(0);
    }
    print!("{}", versions());
    println!(
        "codex auth.json: {}",
        if usage::user_home().join(".codex/auth.json").exists() {
            "present"
        } else {
            "missing"
        }
    );
    let hooks = usage::user_home().join(".gemini/config/hooks.json");
    if hooks.exists() {
        match paths::read_json::<Value>(&hooks) {
            Ok(Value::Object(hooks)) => println!(
                "global agy hooks: {} (run on every worker call)",
                hooks.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
            _ => println!("global agy hooks: invalid config (hooks run on every worker call)"),
        }
    }
    print!("{}", usage::table(&usage::read(home, false)?));
    Ok(0)
}
fn scan(agent: &str, out: &Path, subs: &[String], seen: &mut HashSet<Vec<String>>) -> Res<()> {
    if !seen.insert(subs.to_vec()) {
        return Ok(());
    }
    let output = process::capture(
        Command::new(process::executable(agent))
            .args(subs)
            .arg("--help"),
        Duration::from_secs(10),
    );
    let filename = if subs.is_empty() {
        agent.into()
    } else {
        format!("{agent}_{}", subs.join("_"))
    };
    let text = match output {
        Ok(text) => text,
        Err(e) => format!("baton: {e}\n"),
    };
    fs::write(out.join(format!("{filename}.txt")), &text)?;
    if subs.len() < 3 {
        for sub in commands(&text) {
            let mut path = subs.to_vec();
            path.push(sub);
            scan(agent, out, &path, seen)?;
        }
    }
    Ok(())
}
fn commands(help: &str) -> Vec<String> {
    let mut inside = false;
    let mut command_indent = None;
    let mut commands = Vec::new();
    for line in help.lines() {
        let trimmed = line.trim();
        if ["Commands:", "Available subcommands:"].contains(&trimmed) {
            inside = true;
            command_indent = None;
            continue;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) || trimmed.ends_with(':') {
            inside = false;
            continue;
        }
        let name = trimmed.split_whitespace().next().unwrap_or("");
        let indent = line.len() - line.trim_start().len();
        if command_indent.is_some_and(|level| indent != level) {
            continue;
        }
        if name != "help"
            && !name.is_empty()
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            command_indent = Some(indent);
            commands.push(name.to_string());
        }
    }
    commands
}
