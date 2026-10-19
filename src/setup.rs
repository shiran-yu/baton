//! Inspect first; mutations are confined to explicitly selected, consented steps.
use crate::{process, Error, Res};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const IDS: [&str; 9] = [
    "binary",
    "codex-install",
    "agy-install",
    "codex-login",
    "agy-login",
    "agy-permissions",
    "marketplaces",
    "sync",
    "selftest",
];
const MARKET: &str = "my-develop-plugins-codex";
const HELP: &str = "usage: baton setup [--plan [--json] | --apply ID[,ID...] [--yes]]";

#[derive(Default)]
pub struct Options {
    plan: bool,
    json: bool,
    apply: Option<Vec<String>>,
    yes: bool,
}
pub fn parse(args: impl Iterator<Item = String>) -> Res<Options> {
    let mut options = Options::default();
    let mut args = args.peekable();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--plan" if !options.plan => options.plan = true,
            "--json" if !options.json => options.json = true,
            "--yes" if !options.yes => options.yes = true,
            "--apply" if options.apply.is_none() => {
                let ids = args.next().ok_or_else(|| Error::usage(HELP))?;
                let ids: Vec<String> = ids.split(',').map(String::from).collect();
                if ids.iter().any(|id| !IDS.contains(&id.as_str())) {
                    return Err(Error::usage(format!(
                        "unknown setup step; choose {}",
                        IDS.join(",")
                    )));
                }
                options.apply = Some(ids);
            }
            _ => return Err(Error::usage(HELP)),
        }
    }
    if (options.plan && options.apply.is_some())
        || (options.json && !options.plan)
        || (options.yes && options.apply.is_none())
    {
        return Err(Error::usage(HELP));
    }
    Ok(options)
}

#[derive(Serialize)]
struct Step {
    id: &'static str,
    description: &'static str,
    commands: Vec<String>,
    needs_user: bool,
    needs_consent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_edit: Option<SettingsEdit>,
}

#[derive(Serialize)]
struct SettingsEdit {
    path: PathBuf,
    backup: PathBuf,
    allow: Vec<String>,
}

pub(crate) fn quote(value: impl AsRef<std::ffi::OsStr>) -> String {
    let value = value.as_ref().to_string_lossy();
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.@=,:".contains(&b))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
fn agent(agent: &str) -> String {
    quote(process::executable(agent))
}
pub(crate) fn installed(agent: &str) -> bool {
    let executable = process::executable(agent);
    let executable = Path::new(&executable);
    let check = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if executable.components().count() > 1 || executable.is_absolute() {
        check(executable)
    } else {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|p| check(&p.join(executable))))
    }
}

/// Resolve existing ancestors without creating directories during inspection.
pub fn agy_root() -> Res<PathBuf> {
    let root = std::env::var_os("BATON_AGY_WT_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("TMPDIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("baton-wt")
        });
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut resolved = PathBuf::new();
    for component in root.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            _ => {
                resolved.push(component);
                match fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(resolved)
}
fn user_home() -> Res<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::usage("HOME must be set for setup"))
}
fn settings_path() -> Res<PathBuf> {
    Ok(user_home()?.join(".gemini/antigravity-cli/settings.json"))
}
fn permissions() -> Res<Vec<String>> {
    let root = agy_root()?;
    let root = format!("{}/", root.to_string_lossy().trim_end_matches('/'));
    Ok(vec![
        format!("read_file({root})"),
        format!("write_file({root})"),
    ])
}
fn read_settings(path: &Path) -> Res<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok("{}\n".into()),
        Err(e) => Err(e.into()),
    }
}
fn missing_permissions(text: &str) -> Res<Vec<String>> {
    let value: Value = serde_json::from_str(text)?;
    if !value.is_object() {
        return Err(Error::usage("agy settings must be a JSON object"));
    }
    if let Some(p) = value.get("permissions") {
        if !p.is_object() || p.get("allow").is_some_and(|a| !a.is_array()) {
            return Err(Error::usage(
                "agy permissions must be an object and permissions.allow an array",
            ));
        }
    }
    let allow = value["permissions"]["allow"].as_array();
    Ok(permissions()?
        .into_iter()
        .filter(|p| !allow.is_some_and(|a| a.iter().any(|v| v.as_str() == Some(p))))
        .collect())
}

// JSON is validated above. Locate value spans with serde's streaming decoder, leaving every
// existing byte (including object key order, indentation and number spellings) untouched.
fn member(text: &str, key: &str) -> Res<Option<std::ops::Range<usize>>> {
    let mut offset = text
        .find('{')
        .ok_or_else(|| Error::usage("expected JSON object"))?
        + 1;
    loop {
        while text
            .as_bytes()
            .get(offset)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == b',')
        {
            offset += 1;
        }
        if text.as_bytes().get(offset) == Some(&b'}') {
            return Ok(None);
        }
        let mut stream = serde_json::Deserializer::from_str(&text[offset..]).into_iter::<String>();
        let name = stream
            .next()
            .ok_or_else(|| Error::usage("missing JSON key"))??;
        offset += stream.byte_offset();
        while text
            .as_bytes()
            .get(offset)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == b':')
        {
            offset += 1;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[offset..]).into_iter::<Value>();
        stream
            .next()
            .ok_or_else(|| Error::usage("missing JSON value"))??;
        let end = offset + stream.byte_offset();
        if name == key {
            return Ok(Some(offset..end));
        }
        offset = end;
    }
}
fn append(text: &str, entry: &str) -> String {
    let end = text.trim_end().len() - 1;
    let empty = text[1..end].trim().is_empty();
    format!(
        "{}{}{entry}{}",
        &text[..end],
        if empty { "" } else { "," },
        &text[end..]
    )
}
fn edit_permissions() -> Res<()> {
    let path = settings_path()?;
    let original = read_settings(&path)?;
    let missing = missing_permissions(&original)?;
    if missing.is_empty() {
        return Ok(());
    }
    let entries = missing
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    let updated = if let Some(p) = member(&original, "permissions")? {
        let permissions = &original[p.clone()];
        let edited = if let Some(a) = member(permissions, "allow")? {
            let array = append(&permissions[a.clone()], &entries);
            format!(
                "{}{}{}",
                &permissions[..a.start],
                array,
                &permissions[a.end..]
            )
        } else {
            append(permissions, &format!("\"allow\":[{entries}]"))
        };
        format!("{}{}{}", &original[..p.start], edited, &original[p.end..])
    } else {
        // Exclude leading whitespace so append can recognize an empty object.
        let start = original.find('{').unwrap();
        format!(
            "{}{}",
            &original[..start],
            append(
                &original[start..],
                &format!("\"permissions\":{{\"allow\":[{entries}]}}")
            )
        )
    };
    fs::create_dir_all(path.parent().unwrap())?;
    // Save the original even for a newly created settings file (the empty object).
    fs::write(path.with_file_name("settings.json.baton-bak"), &original)?;
    let temporary = path.with_file_name("settings.json.baton-tmp");
    fs::write(&temporary, updated)?;
    if let Ok(metadata) = fs::metadata(&path) {
        fs::set_permissions(&temporary, metadata.permissions())?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn steps(home: &Path) -> Res<Vec<Step>> {
    let executable = std::env::current_exe()?;
    let binary = home.join("bin/baton");
    let link = user_home()?.join(".local/bin/baton");
    let baton = quote(&executable);
    let codex = agent("codex");
    let agy = agent("agy");
    let mut binary_commands = vec![format!(
        "mkdir -p {} {}",
        quote(binary.parent().unwrap()),
        quote(link.parent().unwrap())
    )];
    if fs::canonicalize(&binary).ok().as_ref() != Some(&executable) {
        let temporary = binary.with_file_name("baton.setup-tmp");
        binary_commands.push(format!("cp {} {}", quote(&executable), quote(&temporary)));
        binary_commands.push(format!("mv -f {} {}", quote(&temporary), quote(&binary)));
    }
    binary_commands.push(format!("rm -f {}", quote(&link)));
    binary_commands.push(format!("ln -s {} {}", quote(&binary), quote(&link)));
    let descriptions = [
        "Install baton and its worker-accessible symlink",
        "Install Codex",
        "Install Antigravity",
        "Sign in to Codex",
        "Sign in to Antigravity",
        "Allow agy to read and write its worktree root",
        "Register the Codex marketplace and install the baton plugin",
        "Sync installed plugins",
        "Refresh usage and run diagnostics",
    ];
    let commands = [
        binary_commands,
        vec!["curl -fsSL https://chatgpt.com/codex/install.sh | sh".into()],
        vec!["curl -fsSL https://antigravity.google/cli/install.sh | bash".into()],
        vec![format!("{codex} login --device-auth")],
        vec![agy],
        vec![format!("{baton} setup --apply agy-permissions --yes")],
        vec![
            format!("{codex} plugin marketplace add shiran-yu/my-develop-plugins"),
            format!("{codex} plugin add baton@{MARKET}"),
        ],
        vec![format!("{baton} sync")],
        vec![
            format!("{baton} usage --refresh"),
            format!("{baton} doctor"),
        ],
    ];
    let mut steps: Vec<Step> = IDS
        .into_iter()
        .zip(descriptions)
        .zip(commands)
        .map(|((id, description), commands)| Step {
            id,
            description,
            commands,
            needs_user: id.ends_with("-login"),
            needs_consent: !id.ends_with("-login") && id != "selftest",
            settings_edit: None,
        })
        .collect();
    let path = settings_path()?;
    steps[5].settings_edit = Some(SettingsEdit {
        backup: path.with_file_name("settings.json.baton-bak"),
        path,
        allow: permissions()?,
    });
    Ok(steps)
}
fn probe(agent: &str, args: &[&str]) -> Res<String> {
    // Merge stderr as authentication failures may be reported there with a zero exit status.
    process::capture(
        Command::new("/bin/sh")
            .args(["-c", "exec \"$@\" 2>&1", "baton-probe"])
            .arg(process::executable(agent))
            .args(args),
        Duration::from_secs(10),
    )
}
fn has_marketplace() -> bool {
    probe("codex", &["plugin", "marketplace", "list", "--json"]).is_ok_and(|text| {
        fn contains(v: &Value) -> bool {
            match v {
                Value::String(s) => s == MARKET,
                Value::Object(m) => m.contains_key(MARKET) || m.values().any(contains),
                Value::Array(a) => a.iter().any(contains),
                _ => false,
            }
        }
        serde_json::from_str::<Value>(&text).is_ok_and(|v| contains(&v))
    })
}
fn binary_present(home: &Path) -> Res<bool> {
    let binary = home.join("bin/baton");
    let link = user_home()?.join(".local/bin/baton");
    Ok(
        fs::metadata(&binary).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            && fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink())
            && fs::canonicalize(&link)
                .ok()
                .zip(fs::canonicalize(&binary).ok())
                .is_some_and(|(a, b)| a == b),
    )
}
fn pending(home: &Path, id: &str) -> Res<bool> {
    Ok(match id {
        "binary" => !binary_present(home)?,
        "codex-install" => !installed("codex"),
        "agy-install" => !installed("agy"),
        "codex-login" => !installed("codex") || probe("codex", &["login", "status"]).is_err(),
        "agy-login" => {
            !installed("agy")
                || probe("agy", &["-p", "/usage", "--output-format", "json"]).map_or(true, |s| {
                    s.to_lowercase().contains("authentication required")
                })
        }
        "agy-permissions" => !missing_permissions(&read_settings(&settings_path()?)?)?.is_empty(),
        "marketplaces" => !installed("codex") || !has_marketplace(),
        // These checks intentionally run on every setup, since installed plugins and usage change.
        "sync" | "selftest" => true,
        _ => unreachable!(),
    })
}
fn confirm(step: &Step) -> Res<bool> {
    print!("Apply {}: {}? [y/N] ", step.id, step.description);
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}
pub(crate) fn shell(command: &str) -> Res<()> {
    let status = Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            status.code().unwrap_or(1),
            format!("setup command failed: {command}"),
        ))
    }
}
fn installer(url: &str, interpreter: &str) -> Res<()> {
    // Match the advertised curl | shell pipeline, while checking both exit statuses. A failed
    // download otherwise looks successful because an interpreter accepts empty input.
    let mut download = Command::new("curl")
        .args(["-fsSL", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let input = download
        .stdout
        .take()
        .ok_or_else(|| Error::new(1, "missing installer output"))?;
    let result = Command::new(interpreter).stdin(input).status();
    if result.is_err() {
        let _ = download.kill();
    }
    let downloaded = download.wait()?;
    let installed = result?;
    if !downloaded.success() || !installed.success() {
        return Err(Error::new(
            1,
            format!("installer failed: curl -fsSL {url} | {interpreter}"),
        ));
    }
    Ok(())
}
fn install_binary(home: &Path) -> Res<()> {
    let executable = std::env::current_exe()?;
    let target = home.join("bin/baton");
    let link = user_home()?.join(".local/bin/baton");
    if target == link {
        return Err(Error::usage(
            "BATON_HOME/bin/baton must differ from ~/.local/bin/baton",
        ));
    }
    fs::create_dir_all(target.parent().unwrap())?;
    fs::create_dir_all(link.parent().unwrap())?;
    if fs::canonicalize(&target).ok().as_ref() != Some(&executable) {
        let temporary = target.with_file_name("baton.setup-tmp");
        fs::copy(executable, &temporary)?;
        fs::rename(temporary, &target)?;
    }
    if fs::symlink_metadata(&link).is_ok() {
        fs::remove_file(&link)?;
    }
    symlink(target, link)?;
    Ok(())
}
fn apply(home: &Path, step: &Step) -> Res<()> {
    if step.needs_user {
        println!(
            "{}: run this command yourself: {}",
            step.id,
            step.commands.join("; ")
        );
        return Ok(());
    }
    match step.id {
        "binary" => install_binary(home),
        "agy-permissions" => edit_permissions(),
        "codex-install" if installed("codex") => Ok(()),
        "agy-install" if installed("agy") => Ok(()),
        "codex-install" => installer("https://chatgpt.com/codex/install.sh", "sh"),
        "agy-install" => installer("https://antigravity.google/cli/install.sh", "bash"),
        "marketplaces" if has_marketplace() => Ok(()),
        _ => {
            for command in &step.commands {
                shell(command)?;
            }
            Ok(())
        }
    }
}
pub fn run(home: &Path, options: Options) -> Res<i32> {
    let steps = steps(home)?;
    let tty = io::stdin().is_terminal();
    if let Some(ids) = options.apply {
        // Validate consent for the entire batch before allowing any mutation.
        if !options.yes
            && !tty
            && steps
                .iter()
                .any(|s| ids.iter().any(|id| id == s.id) && s.needs_consent)
        {
            return Err(Error::usage(
                "setup consent required: no TTY; ask the user, then retry --apply with --yes",
            ));
        }
        for step in steps.iter().filter(|s| ids.iter().any(|id| id == s.id)) {
            if step.needs_consent && !options.yes && !confirm(step)? {
                continue;
            }
            apply(home, step)?;
        }
        return Ok(0);
    }
    let mut plan = Vec::new();
    for step in steps {
        if pending(home, step.id)? {
            plan.push(step);
        }
    }
    if options.json {
        println!("{}", serde_json::to_string(&plan)?);
    } else {
        for step in &plan {
            println!(
                "{}: {} [user: {}, consent: {}]",
                step.id, step.description, step.needs_user, step.needs_consent
            );
            for command in &step.commands {
                println!("  {command}");
            }
            if step.id == "agy-permissions" {
                println!(
                    "  Add to {}: {}",
                    settings_path()?.display(),
                    permissions()?.join(", ")
                );
            }
        }
    }
    if !options.plan && tty {
        for step in &plan {
            if step.needs_user || confirm(step)? {
                apply(home, step)?;
            }
        }
    }
    Ok(0)
}
