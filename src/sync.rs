use crate::{hook_adapt, paths, Error, Res};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub struct Options {
    pub dry_run: bool,
    pub hosts: Vec<String>,
    pub only: Vec<String>,
    pub include_trading: bool,
}
impl Default for Options {
    fn default() -> Self {
        let mut hosts = vec!["codex".into(), "agy".into()];
        // Copilot is a default host only where its CLI is installed.
        if crate::setup::installed("copilot") {
            hosts.push("copilot".into());
        }
        Self {
            dry_run: false,
            hosts,
            only: Vec::new(),
            include_trading: false,
        }
    }
}
#[derive(Deserialize)]
struct Config {
    #[serde(default = "own_marketplaces")]
    own_marketplaces: Vec<String>,
    #[serde(default = "default_exclude")]
    exclude: Vec<String>,
}
/// OpenAI's codex plugin drives Codex from Claude Code; mirroring it into Codex or agy is pointless.
fn default_exclude() -> Vec<String> {
    vec!["codex".into()]
}
fn own_marketplaces() -> Vec<String> {
    [
        "my-develop-plugins",
        "yushiran-research",
        "trading-plugins",
        "baton",
    ]
    .map(String::from)
    .into()
}
#[derive(Deserialize)]
struct Inventory {
    plugins: BTreeMap<String, Vec<Installation>>,
}
#[derive(Deserialize)]
struct Installation {
    #[serde(rename = "installPath")]
    path: PathBuf,
    // Claude Code writes no version for a plugin that declares none (it caches it by commit); filled from the
    // install folder's name below
    #[serde(default)]
    version: String,
}
#[derive(Deserialize)]
struct Marketplace {
    // missing for marketplaces Claude Code has not cloned yet; those are dropped after loading
    #[serde(default, rename = "installLocation")]
    location: PathBuf,
    // source is intentionally not fetched: synchronization uses the local checkout.
    #[serde(default, rename = "source")]
    _source: Value,
}

pub fn run(home: &Path, options: &Options) -> Res<i32> {
    let plugins = match std::env::var_os("BATON_CLAUDE_PLUGINS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| Error::usage("HOME or BATON_CLAUDE_PLUGINS must be set"))?,
        )
        .join(".claude/plugins"),
    };
    let mut inventory: Inventory = paths::read_json(&plugins.join("installed_plugins.json"))?;
    for entry in inventory.plugins.values_mut().flatten() {
        if entry.version.is_empty() {
            entry.version = entry.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        }
    }
    let mut marketplaces: BTreeMap<String, Marketplace> =
        paths::read_json(&plugins.join("known_marketplaces.json"))?;
    // an empty location would resolve catalog paths against the current directory
    marketplaces.retain(|_, m| m.location.is_absolute());
    let config: Config = if home.join("sync.json").exists() {
        paths::read_json(&home.join("sync.json"))?
    } else {
        Config {
            own_marketplaces: own_marketplaces(),
            exclude: default_exclude(),
        }
    };
    let mut codex = Codex::default();
    let mut copilot = Copilot::default();
    if options.hosts.iter().any(|host| host == "codex") {
        for (name, marketplace) in &marketplaces {
            if config.own_marketplaces.contains(name)
                && marketplace
                    .location
                    .join(".agents/plugins/marketplace.json")
                    .is_file()
            {
                codex.ensure_marketplace(marketplace, options.dry_run)?;
            }
        }
    }
    let mut failed = false;
    for (id, entries) in inventory.plugins {
        let (name, market) = id
            .rsplit_once('@')
            .ok_or_else(|| Error::usage(format!("invalid plugin ID: {id}")))?;
        if !options.only.is_empty() && !options.only.iter().any(|n| n == name || n == &id) {
            continue;
        }
        for host in &options.hosts {
            let skip = if !config.own_marketplaces.iter().any(|m| m == market) {
                Some("not an own marketplace")
            } else if config.exclude.iter().any(|n| n == name || n == &id) {
                Some("excluded")
            } else if host == "agy" && market == "trading-plugins" && !options.include_trading {
                Some("trading guards not proven for agy; use --include-trading")
            } else if host == "copilot" && market == "trading-plugins" && !options.include_trading {
                Some("trading guards not proven for copilot; use --include-trading")
            } else {
                None
            };
            let outcome = if let Some(reason) = skip {
                Ok(format!("skipped ({reason})"))
            } else if let Some(entry) = entries.first() {
                if host == "codex" {
                    codex.sync(name, market, &marketplaces, options.dry_run)
                } else if host == "copilot" {
                    copilot.sync(name, market, &marketplaces, options.dry_run)
                } else {
                    // agy(home, name, entry, options.dry_run)
                    let skills = skill_paths(&marketplaces, market, name, &entry.path);
                    agy(home, name, entry, &skills, options.dry_run)
                }
            } else {
                Err(Error::new(1, "no installation entry"))
            };
            match outcome {
                Ok(status) => println!("{name} {host}: {status}"),
                Err(error) => {
                    failed = true;
                    println!("{name} {host}: error ({error})");
                }
            }
        }
    }
    Ok(i32::from(failed))
}

pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn binary(host: &str) -> std::ffi::OsString {
    std::env::var_os(format!("BATON_{}", host.to_uppercase())).unwrap_or_else(|| host.into())
}
fn invoke(host: &str, args: &[&str]) -> Res<String> {
    let mut command = Command::new(binary(host));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // A first-run update prompt must not stall the sync.
    if host == "copilot" {
        command.env("COPILOT_AUTO_UPDATE", "false");
    }
    let mut child = command.spawn()?;
    let (tx, rx) = mpsc::channel();
    for (index, stream) in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut text);
            }
            let _ = tx.send((index, text));
        });
    }
    // BATON_SYNC_TIMEOUT (seconds) exists for tests.
    let timeout = std::env::var("BATON_SYNC_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let timed_out = |child: &mut std::process::Child| {
        // Kill the whole group, so a child holding the pipes cannot outlive the sync.
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
        Error::new(
            5,
            format!("{host} {}: timed out after {timeout}s", args.join(" ")),
        )
    };
    let mut output = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, text)) => output[index] = text,
            Err(_) => return Err(timed_out(&mut child)),
        }
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            return Err(timed_out(&mut child));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let [stdout, stderr] = output;
    if !status.success() {
        return Err(Error::new(
            1,
            format!(
                "{host} {}: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&stdout).trim(),
                String::from_utf8_lossy(&stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}
fn planned(host: &str, args: &[&str]) {
    println!(
        "dry-run: {} {}",
        quote(&binary(host).to_string_lossy()),
        args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
    );
}

#[derive(Default)]
struct Codex {
    marketplaces: Option<String>,
    plugins: Option<Value>,
    registered: BTreeSet<String>,
    added: BTreeSet<String>,
}
impl Codex {
    fn ensure_marketplace(&mut self, market: &Marketplace, dry: bool) -> Res<Value> {
        let catalog: Value =
            paths::read_json(&market.location.join(".agents/plugins/marketplace.json"))?;
        let market_name = catalog["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(1, "Codex catalog has no name"))?;
        if self.marketplaces.is_none() {
            self.marketplaces = Some(invoke("codex", &["plugin", "marketplace", "list"])?);
        }
        let location = market.location.to_string_lossy();
        if !self.registered.contains(market_name) {
            if !marketplace_present(
                self.marketplaces.as_deref().unwrap_or(""),
                market_name,
                &location,
            ) {
                let args = ["plugin", "marketplace", "add", &location];
                if dry {
                    planned("codex", &args);
                } else {
                    invoke("codex", &args)?;
                }
            }
            self.registered.insert(market_name.into());
        }
        Ok(catalog)
    }

    fn sync(
        &mut self,
        name: &str,
        market: &str,
        markets: &BTreeMap<String, Marketplace>,
        dry: bool,
    ) -> Res<String> {
        let Some(market) = markets.get(market) else {
            return Ok("skipped (no codex manifest)".into());
        };
        let catalog_path = market.location.join(".agents/plugins/marketplace.json");
        if !catalog_path.is_file() {
            return Ok("skipped (no codex manifest)".into());
        }
        let catalog = self.ensure_marketplace(market, dry)?;
        let market_name = catalog["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(1, "Codex catalog has no name"))?;
        if !catalog["plugins"]
            .as_array()
            .is_some_and(|plugins| plugins.iter().any(|p| p["name"] == name))
        {
            return Ok("skipped (no codex manifest)".into());
        }
        if self.plugins.is_none() {
            self.plugins = Some(serde_json::from_str(&invoke(
                "codex",
                &["plugin", "list", "--json"],
            )?)?);
        }
        let id = format!("{name}@{market_name}");
        if self.added.contains(&id) || installed(self.plugins.as_ref().unwrap(), name, market_name)
        {
            return Ok("up to date".into());
        }
        let args = ["plugin", "add", &id];
        if dry {
            planned("codex", &args);
        } else {
            invoke("codex", &args)?;
        }
        self.added.insert(id);
        Ok(if dry {
            "skipped (dry-run; would install)"
        } else {
            "installed"
        }
        .into())
    }
}
fn marketplace_present(text: &str, name: &str, location: &str) -> bool {
    fn contains(value: &Value, name: &str, location: &str) -> bool {
        match value {
            Value::String(s) => s == name || s == location,
            Value::Array(values) => values.iter().any(|v| contains(v, name, location)),
            Value::Object(values) => {
                values.contains_key(name) || values.values().any(|v| contains(v, name, location))
            }
            _ => false,
        }
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return contains(&value, name, location);
    }
    text.lines().any(|line| {
        line.trim() == location
            || line
                .split(|c: char| c.is_whitespace() || ":()[]\"'│,".contains(c))
                .any(|part| part == name || part == location)
    })
}
fn installed(value: &Value, name: &str, market: &str) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|v| installed(v, name, market)),
        Value::Object(values) => {
            if values.get("installed") == Some(&Value::Bool(false))
                || values.get("isInstalled") == Some(&Value::Bool(false))
            {
                return false;
            }
            let id = format!("{name}@{market}");
            if values.get("id").and_then(Value::as_str) == Some(&id)
                || values.get("name").and_then(Value::as_str) == Some(&id)
            {
                return true;
            }
            if values.get("name").and_then(Value::as_str) == Some(name) {
                let origin = values
                    .get("marketplaceName")
                    .or_else(|| values.get("marketplace"));
                return origin.is_none_or(|v| v == market || v["name"] == market);
            }
            values
                .iter()
                .any(|(key, v)| (key == &id && v != false) || installed(v, name, market))
        }
        Value::String(s) => s == &format!("{name}@{market}"),
        _ => false,
    }
}

// Copilot CLI reads Claude's .claude-plugin/marketplace.json natively, so no conversion is needed.
#[derive(Default)]
struct Copilot {
    marketplaces: Option<String>,
    plugins: Option<String>,
    registered: BTreeSet<String>,
    added: BTreeSet<String>,
}
impl Copilot {
    fn sync(
        &mut self,
        name: &str,
        market: &str,
        markets: &BTreeMap<String, Marketplace>,
        dry: bool,
    ) -> Res<String> {
        let Some(market) = markets.get(market) else {
            return Ok("skipped (no marketplace checkout)".into());
        };
        let catalog_path = market.location.join(".claude-plugin/marketplace.json");
        if !catalog_path.is_file() {
            return Ok("skipped (no marketplace manifest)".into());
        }
        let catalog: Value = paths::read_json(&catalog_path)?;
        // Copilot registers a marketplace under the name in its manifest.
        let market_name = catalog["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(1, "marketplace manifest has no name"))?;
        if !catalog["plugins"]
            .as_array()
            .is_some_and(|plugins| plugins.iter().any(|p| p["name"] == name))
        {
            return Ok("skipped (not in marketplace manifest)".into());
        }
        if self.marketplaces.is_none() {
            self.marketplaces = Some(invoke("copilot", &["plugin", "marketplace", "list"])?);
        }
        let location = market.location.to_string_lossy();
        if !self.registered.contains(market_name) {
            if !marketplace_present(
                self.marketplaces.as_deref().unwrap_or(""),
                market_name,
                &location,
            ) {
                let args = ["plugin", "marketplace", "add", &location];
                if dry {
                    planned("copilot", &args);
                } else {
                    invoke("copilot", &args)?;
                }
            }
            self.registered.insert(market_name.into());
        }
        if self.plugins.is_none() {
            // Older CLIs have no --json; fall back to the plain list.
            self.plugins = Some(
                invoke("copilot", &["plugin", "list", "--json"])
                    .or_else(|_| invoke("copilot", &["plugin", "list"]))?,
            );
        }
        let id = format!("{name}@{market_name}");
        let list = self.plugins.as_deref().unwrap_or("");
        let present = match serde_json::from_str::<Value>(list) {
            Ok(value) => installed(&value, name, market_name),
            Err(_) => list.lines().any(|line| {
                line.split(|c: char| c.is_whitespace() || "()[]\"'│,".contains(c))
                    .any(|part| part == id || part == name)
            }),
        };
        if self.added.contains(&id) || present {
            return Ok("up to date".into());
        }
        let args = ["plugin", "install", &id];
        if dry {
            planned("copilot", &args);
        } else {
            invoke("copilot", &args)?;
        }
        self.added.insert(id);
        Ok(if dry {
            "skipped (dry-run; would install)"
        } else {
            "installed"
        }
        .into())
    }
}

// Bump when the staged layout changes: every plugin is then re-staged on the next sync.
const CONVERTER: u64 = 2;
// A referenced plugin-root entry larger than this stays behind (mo-academic-taste's corpus).
const RESOURCE_CAP: u64 = 2 << 20;
// agy's limit on a SKILL.md body.
const SKILL_LIMIT: u64 = 24 * 1024;

/// The skill folders a Claude marketplace entry or plugin manifest names, relative to the plugin.
fn skill_paths(
    markets: &BTreeMap<String, Marketplace>,
    market: &str,
    name: &str,
    source: &Path,
) -> Vec<String> {
    let mut found = Vec::new();
    let mut add = |value: &Value| match value {
        Value::String(path) => found.push(path.clone()),
        Value::Array(list) => {
            found.extend(list.iter().filter_map(Value::as_str).map(String::from))
        }
        _ => {}
    };
    if let Some(m) = markets.get(market) {
        let catalog = paths::read_json::<Value>(&m.location.join(".claude-plugin/marketplace.json"));
        if let Some(entry) = catalog.ok().and_then(|c| {
            c["plugins"]
                .as_array()
                .and_then(|list| list.iter().find(|p| p["name"] == name).cloned())
        }) {
            add(&entry["skills"]);
        }
    }
    if let Ok(manifest) = paths::read_json::<Value>(&source.join(".claude-plugin/plugin.json")) {
        add(&manifest["skills"]);
    }
    found
}

// fn agy(home: &Path, name: &str, entry: &Installation, dry: bool) -> Res<String> {
fn agy(
    home: &Path,
    name: &str,
    entry: &Installation,
    skills: &[String],
    dry: bool,
) -> Res<String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::new(1, "invalid agy plugin name"));
    }
    let source = fs::canonicalize(&entry.path)?;
    let dest = home.join("agy-plugins").join(name);
    let marker = dest.join(".baton-sync.json");
    if marker.is_file() {
        let previous: Value = paths::read_json(&marker)?;
        if previous["version"] == entry.version
            && previous["source"] == source.to_string_lossy().as_ref()
            && previous["installed"] == true
            && previous["converter"] == CONVERTER
        {
            return Ok("up to date".into());
        }
    }
    let dir = dest.to_string_lossy();
    // Dry run: only report the plan; no files are generated and agy is never invoked.
    if dry {
        planned("agy", &["plugin", "validate", "--json", &dir]);
        planned("agy", &["plugin", "install", &dir]);
        return Ok("skipped (dry-run; would install)".into());
    }
    if dest.symlink_metadata().is_ok() {
        // Never follow a destination symlink when materializing generated files.
        if dest.symlink_metadata()?.file_type().is_symlink() {
            fs::remove_file(&dest)?;
        } else {
            fs::remove_dir_all(&dest)?;
        }
    }
    fs::create_dir_all(&dest)?;
    // build_agy(name, &entry.version, &source, &dest)?;
    let notes = build_agy(name, &entry.version, &source, &dest, skills)?;
    let validation = invoke("agy", &["plugin", "validate", "--json", &dir])?;
    if let Ok(value) = serde_json::from_str::<Value>(&validation) {
        if value["valid"] == false
            || value["success"] == false
            || value["errors"].as_array().is_some_and(|v| !v.is_empty())
        {
            return Err(Error::new(1, format!("agy validation: {validation}")));
        }
        if value["warnings"].as_array().is_some_and(|v| !v.is_empty()) {
            eprintln!("{name} agy validation: {validation}");
        }
    }
    invoke("agy", &["plugin", "install", &dir])?;
    paths::write_json(
        &marker,
        // &json!({"version":entry.version, "source":source, "installed":true}),
        &json!({"version":entry.version, "source":source, "installed":true, "converter":CONVERTER}),
    )?;
    // Ok("installed".into())
    Ok(if notes.is_empty() {
        "installed".into()
    } else {
        format!("installed ({})", notes.join("; "))
    })
}

fn copy_tree(source: &Path, dest: &Path) -> Res<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if SKIPPED.iter().any(|s| entry.file_name() == *s) {
            continue;
        }
        let target = dest.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, target)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
fn markdown_files(dir: &Path) -> Res<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
// Never copied into a staged plugin.
const SKIPPED: [&str; 3] = [".git", "node_modules", "__pycache__"];

// fn build_agy(name: &str, version: &str, source: &Path, dest: &Path) -> Res<()> {
fn build_agy(
    name: &str,
    version: &str,
    source: &Path,
    dest: &Path,
    skills: &[String],
) -> Res<Vec<String>> {
    let manifest_path = source.join(".claude-plugin/plugin.json");
    let manifest: Value = if manifest_path.is_file() {
        paths::read_json(&manifest_path)?
    } else {
        json!({})
    };
    paths::write_json(
        &dest.join("plugin.json"),
        &json!({"name":name,"description":manifest["description"].as_str().unwrap_or(""),"version":version}),
    )?;
    // copy_tree(&source.join("skills"), &dest.join("skills"))?;
    let notes = stage_skills(name, source, dest, skills)?;
    copy_tree(&source.join("bin"), &dest.join("bin"))?;
    for path in markdown_files(&source.join("commands"))? {
        let cmd = path.file_stem().unwrap().to_string_lossy();
        let skill = format!("{name}-{cmd}");
        let dir = dest.join("skills").join(&skill);
        // A copied skill may be a symlink; do not overwrite it through the link.
        if dir.symlink_metadata().is_ok() {
            return Err(Error::new(
                1,
                format!("command skill collides with existing skill: {skill}"),
            ));
        }
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("SKILL.md"),
            frontmatter(&fs::read_to_string(path)?, Some(&skill), false),
        )?;
    }
    for path in markdown_files(&source.join("agents"))? {
        let dir = dest.join("agents");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(path.file_name().unwrap()),
            frontmatter(&fs::read_to_string(&path)?, None, true),
        )?;
    }
    let mcp = source.join(".mcp.json");
    if mcp.is_file() {
        let mut value: Value = paths::read_json(&mcp)?;
        replace_root(&mut value, &source.to_string_lossy());
        if let Some(servers) = value["mcpServers"].as_object_mut() {
            for server in servers.values_mut() {
                if let Some(server) = server.as_object_mut() {
                    if let Some(url) = server.remove("url") {
                        server.insert("serverUrl".into(), url);
                    }
                }
            }
        }
        paths::write_json(
            &dest.join("mcp_config.json"),
            &json!({"mcpServers":value["mcpServers"]}),
        )?;
    }
    let hooks = source.join("hooks/hooks.json");
    if hooks.is_file() {
        let value: Value = paths::read_json(&hooks)?;
        paths::write_json(
            &dest.join("hooks.json"),
            &convert_hooks(name, &value, source)?,
        )?;
    }
    // Ok(())
    Ok(notes)
}

// Stage each skill Claude Code loads from the plugin, as its Claude-native SKILL.md with every
// plugin file it refers to copied beside it. agy reads only inside the installed plugin
// (~/.gemini/config/plugins/<name>/): a read of the Claude install path is soft-denied in
// headless runs, and `agy plugin install` refuses a symlink leaving the plugin (agy 1.5.7).
// - skills/<s>/SKILL.md, or the SKILL.md its adapter shim points at (`../../SKILL.md`);
// - the folders the marketplace entry or plugin manifest lists under `skills`;
// - else a root SKILL.md, as skill <name>.
// A root SKILL.md's plugin files go beside it in skills/<s>/; a skill folder's `../../X` and
// `${CLAUDE_PLUGIN_ROOT}/X` go to the staged plugin root, and the variable becomes its install path.
fn stage_skills(name: &str, source: &Path, dest: &Path, extra: &[String]) -> Res<Vec<String>> {
    let mut skills: Vec<(String, PathBuf)> = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(source.join("skills"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        let (Ok(text), Some(skill)) = (
            fs::read_to_string(dir.join("SKILL.md")),
            dir.file_name().and_then(|n| n.to_str()).map(String::from),
        ) else {
            continue;
        };
        let target = shim_target(&text, source).unwrap_or(dir);
        skills.push((skill, target));
    }
    for path in extra {
        let path = path.trim_start_matches("./").trim_end_matches('/');
        if path.split('/').any(|part| part == "..") {
            continue;
        }
        let dir = source.join(path);
        let skill = match dir.file_name().and_then(|n| n.to_str()) {
            Some(skill) if !path.is_empty() => skill.to_string(),
            _ => name.to_string(),
        };
        if dir.join("SKILL.md").is_file() {
            skills.push((skill, dir));
        }
    }
    if skills.is_empty() && source.join("SKILL.md").is_file() {
        skills.push((name.into(), source.into()));
    }
    let installed = crate::usage::user_home()
        .join(".gemini/config/plugins")
        .join(name);
    let (mut notes, mut staged): (Vec<String>, Vec<(String, PathBuf)>) = (Vec::new(), Vec::new());
    for (skill, dir) in skills {
        if staged.iter().any(|(s, d)| *s == skill || *d == dir)
            || !skill
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || skill.starts_with('.')
        {
            continue;
        }
        staged.push((skill.clone(), dir.clone()));
        let out = dest.join("skills").join(&skill);
        let root = dir == source;
        if root {
            fs::create_dir_all(&out)?;
            let text = fs::read_to_string(dir.join("SKILL.md"))?;
            // agy wants the skill's name to match its folder.
            fs::write(out.join("SKILL.md"), frontmatter(&text, Some(&skill), false))?;
        } else {
            copy_tree(&dir, &out)?;
        }
        // Where the plugin root's files go, and where agy will find them once installed.
        let (image, image_installed) = if root {
            (out.clone(), installed.join("skills").join(&skill))
        } else {
            (dest.to_path_buf(), installed.clone())
        };
        let mut text = String::new();
        for file in markdown_under(&out) {
            text.push_str(&fs::read_to_string(file).unwrap_or_default());
            text.push('\n');
        }
        let (copy, left) = referenced(source, &text, root)?;
        let mut rewritten = vec![out.clone()];
        for entry in copy {
            let target = image.join(&entry);
            if target.symlink_metadata().is_ok() {
                continue;
            }
            if source.join(&entry).is_dir() {
                copy_tree(&source.join(&entry), &target)?;
            } else {
                fs::copy(source.join(&entry), &target)?;
            }
            rewritten.push(target);
        }
        for (entry, size) in left {
            notes.push(format!(
                "{skill}: left out {entry}, {:.1} MB",
                size as f64 / 1_048_576.0
            ));
        }
        for folder in rewritten {
            for file in markdown_under(&folder) {
                let text = fs::read_to_string(&file)?;
                if text.contains("CLAUDE_PLUGIN_ROOT") {
                    let path = image_installed.to_string_lossy();
                    fs::write(
                        &file,
                        text.replace("${CLAUDE_PLUGIN_ROOT}", &path)
                            .replace("$CLAUDE_PLUGIN_ROOT", &path),
                    )?;
                }
            }
        }
        let bytes = fs::metadata(out.join("SKILL.md")).map_or(0, |m| m.len());
        if bytes > SKILL_LIMIT {
            notes.push(format!("{skill}: SKILL.md is {bytes} bytes, over agy's 24 KB"));
        }
    }
    Ok(notes)
}

// An adapter shim (skills/<s>/SKILL.md deferring to `../../SKILL.md` or `../../<dir>/SKILL.md`)
// gives the folder of the Claude-native SKILL.md it points at.
fn shim_target(text: &str, source: &Path) -> Option<PathBuf> {
    text.match_indices("../../").find_map(|(i, _)| {
        let rest = &text[i + "../../".len()..];
        let middle = &rest[..rest.find("SKILL.md")?];
        let folder = middle.strip_suffix('/').unwrap_or(middle);
        let plain = folder.is_empty()
            || (middle.ends_with('/')
                && !folder.starts_with('.')
                && folder
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)));
        let dir = source.join(folder);
        (plain && dir.join("SKILL.md").is_file()).then_some(dir)
    })
}

// Whether text names the plugin-root entry as a path: after `../../` or `${CLAUDE_PLUGIN_ROOT}/`,
// or, for a root SKILL.md (`bare`), standing on its own (`references/x.md`, `./scripts/`).
fn names(text: &str, entry: &str, dir: bool, bare: bool) -> bool {
    let path = |c: char| c.is_alphanumeric() || "_-./".contains(c);
    let needle = if dir {
        format!("{entry}/")
    } else {
        entry.to_string()
    };
    text.match_indices(&needle).any(|(i, _)| {
        let (before, after) = (&text[..i], &text[i + needle.len()..]);
        (dir || !after.starts_with(|c: char| c.is_alphanumeric() || "_-".contains(c)))
            && (["../../", "CLAUDE_PLUGIN_ROOT}/", "CLAUDE_PLUGIN_ROOT/"]
                .iter()
                .any(|p| before.ends_with(p))
                || bare && !before.strip_suffix("./").unwrap_or(before).ends_with(path))
    })
}

// The plugin-root entries a skill's Markdown refers to, followed through the Markdown they hold;
// an entry over RESOURCE_CAP is returned apart with its size and not followed.
type Referenced = (Vec<String>, Vec<(String, u64)>);
fn referenced(source: &Path, text: &str, bare: bool) -> Res<Referenced> {
    let mut entries: Vec<(String, bool)> = fs::read_dir(source)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let reserved = [
                "skills",
                "SKILL.md",
                "plugin.json",
                "hooks.json",
                "mcp_config.json",
            ];
            (!name.starts_with('.')
                && !reserved.contains(&name.as_str())
                && !SKIPPED.contains(&name.as_str()))
            .then(|| (name, e.path().is_dir()))
        })
        .collect();
    entries.sort();
    let (mut copy, mut left): Referenced = (Vec::new(), Vec::new());
    let mut texts = vec![text.to_string()];
    while let Some(text) = texts.pop() {
        for (entry, dir) in &entries {
            if copy.contains(entry)
                || left.iter().any(|(e, _)| e == entry)
                || !names(&text, entry, *dir, bare)
            {
                continue;
            }
            let size = size(&source.join(entry));
            if size > RESOURCE_CAP {
                left.push((entry.clone(), size));
                continue;
            }
            copy.push(entry.clone());
            for file in markdown_under(&source.join(entry)) {
                texts.push(fs::read_to_string(file).unwrap_or_default());
            }
        }
    }
    copy.sort();
    Ok((copy, left))
}
fn size(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| !SKIPPED.iter().any(|s| e.file_name() == *s))
                .map(|e| size(&e.path()))
                .sum()
        })
        .unwrap_or(0)
}
// Markdown files at or under path (a file counts if it is Markdown), skipping SKIPPED folders.
fn markdown_under(path: &Path) -> Vec<PathBuf> {
    let Ok(meta) = path.symlink_metadata() else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return if path.extension().is_some_and(|e| e == "md") {
            vec![path.into()]
        } else {
            Vec::new()
        };
    }
    let mut files: Vec<PathBuf> = fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| !SKIPPED.iter().any(|s| e.file_name() == *s))
                .flat_map(|e| markdown_under(&e.path()))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn replace_root(value: &mut Value, root: &str) {
    match value {
        Value::String(s) => {
            *s = s
                .replace("${CLAUDE_PLUGIN_ROOT}", root)
                .replace("$CLAUDE_PLUGIN_ROOT", root)
        }
        Value::Array(values) => {
            for v in values {
                replace_root(v, root);
            }
        }
        Value::Object(values) => {
            for v in values.values_mut() {
                replace_root(v, root);
            }
        }
        _ => {}
    }
}

// Preserve frontmatter fields and their continuation lines without introducing a
// general YAML parser. Only the Claude scalar/list fields we translate are read.
fn frontmatter(text: &str, name: Option<&str>, agent: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = if lines.first() == Some(&"---") {
        lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, l)| **l == "---")
            .map(|(i, _)| i)
    } else {
        None
    };
    let mut fields: Vec<Vec<&str>> = Vec::new();
    if let Some(end) = end {
        for line in &lines[1..end] {
            if !line.starts_with(char::is_whitespace) && line.contains(':') {
                fields.push(vec![line]);
            } else if let Some(field) = fields.last_mut() {
                field.push(line);
            }
        }
    } else if name.is_none() {
        return text.into();
    }
    let mut result = String::from("---\n");
    if let Some(name) = name {
        result.push_str(&format!("name: {name}\n"));
    }
    for field in fields {
        let (key, value) = field[0].split_once(':').unwrap();
        if name.is_some() && key == "name" {
            continue;
        }
        if agent
            && [
                "maxTurns",
                "color",
                "effort",
                "isolation",
                "background",
                "disallowedTools",
            ]
            .contains(&key)
        {
            continue;
        }
        if agent && key == "tools" {
            let raw = std::iter::once(value)
                .chain(field.iter().skip(1).copied())
                .collect::<Vec<_>>()
                .join(",");
            let mut mapped = Vec::new();
            for tool in raw.split(',') {
                let tool = tool
                    .trim()
                    .trim_matches(|c| "[]\"'".contains(c))
                    .trim()
                    .trim_start_matches('-')
                    .trim();
                for mapped_tool in hook_adapt::tools(tool) {
                    if !mapped.contains(mapped_tool) {
                        mapped.push(*mapped_tool);
                    }
                }
            }
            if mapped.is_empty() {
                result.push_str("tools: []\n");
            } else {
                result.push_str("tools:\n");
                for tool in mapped {
                    result.push_str(&format!("  - {tool}\n"));
                }
            }
        } else if agent
            && key == "model"
            && ["sonnet", "opus", "haiku", "inherit"]
                .contains(&value.trim().trim_matches(['\'', '"']))
        {
            result.push_str("model: inherit\n");
        } else {
            for line in field {
                result.push_str(line);
                result.push('\n');
            }
        }
    }
    result.push_str("---\n");
    if let Some(end) = end {
        result.push_str(&lines[end + 1..].join("\n"));
        if text.ends_with('\n') {
            result.push('\n');
        }
    } else {
        result.push_str(text);
    }
    result
}

fn convert_hooks(name: &str, input: &Value, source: &Path) -> Res<Value> {
    let mut events = serde_json::Map::new();
    for (claude, agy) in [
        ("SessionStart", "SessionStart"),
        ("UserPromptSubmit", "PreInvocation"),
        ("PreToolUse", "PreToolUse"),
        ("PostToolUse", "PostToolUse"),
        ("Stop", "Stop"),
    ] {
        let Some(groups) = input["hooks"][claude].as_array() else {
            continue;
        };
        let tool_event = matches!(claude, "PreToolUse" | "PostToolUse");
        let mut output = Vec::new();
        for group in groups {
            let mut handlers = Vec::new();
            let hooks = group["hooks"]
                .as_array()
                .ok_or_else(|| Error::new(1, "Claude hook group has no hooks list"))?;
            for handler in hooks {
                let command = handler["command"]
                    .as_str()
                    .ok_or_else(|| Error::new(1, "cannot adapt a non-command hook"))?;
                let mut handler = handler.clone();
                let command = command
                    .replace("${CLAUDE_PLUGIN_ROOT}", &source.to_string_lossy())
                    .replace("$CLAUDE_PLUGIN_ROOT", &source.to_string_lossy());
                handler["command"] = json!(format!(
                    "{} hook-adapt --from agy --event {claude} -- /bin/sh -c {}",
                    quote(&std::env::current_exe()?.to_string_lossy()),
                    quote(&command)
                ));
                handlers.push(handler);
            }
            if tool_event {
                let matcher = group["matcher"].as_str().unwrap_or("");
                let mapped = if matcher.is_empty() || matcher == "*" {
                    matcher.to_string()
                } else {
                    let mut mapped: Vec<&str> = matcher
                        .split('|')
                        .flat_map(|tool| hook_adapt::tools(tool.trim()).iter().copied())
                        .collect();
                    mapped.sort_unstable();
                    mapped.dedup();
                    // Keep all write operations together in the documented agy order.
                    if mapped.contains(&"write_to_file") {
                        mapped.retain(|t| *t != "write_to_file");
                        mapped.insert(0, "write_to_file");
                    }
                    if mapped.contains(&"replace_file_content")
                        && mapped.contains(&"multi_replace_file_content")
                    {
                        mapped.retain(|t| *t != "multi_replace_file_content");
                        let index = mapped
                            .iter()
                            .position(|t| *t == "replace_file_content")
                            .unwrap();
                        mapped.insert(index + 1, "multi_replace_file_content");
                    }
                    if mapped.is_empty() {
                        continue;
                    }
                    mapped.join("|")
                };
                output.push(json!({"matcher":mapped, "hooks":handlers}));
            } else {
                output.extend(handlers);
            }
        }
        events.insert(agy.into(), json!(output));
    }
    Ok(json!({name:events}))
}
