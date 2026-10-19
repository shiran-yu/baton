use crate::{hook_adapt, paths, Error, Res};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct Options {
    pub dry_run: bool,
    pub hosts: Vec<String>,
    pub only: Vec<String>,
    pub include_trading: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            dry_run: false,
            hosts: vec!["codex".into(), "agy".into()],
            only: Vec::new(),
            include_trading: false,
        }
    }
}
#[derive(Deserialize)]
struct Config {
    #[serde(default = "own_marketplaces")]
    own_marketplaces: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
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
    let inventory: Inventory = paths::read_json(&plugins.join("installed_plugins.json"))?;
    let mut marketplaces: BTreeMap<String, Marketplace> =
        paths::read_json(&plugins.join("known_marketplaces.json"))?;
    // an empty location would resolve catalog paths against the current directory
    marketplaces.retain(|_, m| m.location.is_absolute());
    let config: Config = if home.join("sync.json").exists() {
        paths::read_json(&home.join("sync.json"))?
    } else {
        Config {
            own_marketplaces: own_marketplaces(),
            exclude: Vec::new(),
        }
    };
    let mut codex = Codex::default();
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
            } else {
                None
            };
            let outcome = if let Some(reason) = skip {
                Ok(format!("skipped ({reason})"))
            } else if let Some(entry) = entries.first() {
                if host == "codex" {
                    codex.sync(name, market, &marketplaces, options.dry_run)
                } else {
                    agy(home, name, entry, options.dry_run)
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
    let output = Command::new(binary(host))
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::new(
            1,
            format!(
                "{host} {}: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

fn agy(home: &Path, name: &str, entry: &Installation, dry: bool) -> Res<String> {
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
        {
            return Ok("up to date".into());
        }
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
    build_agy(name, &entry.version, &source, &dest)?;
    let dir = dest.to_string_lossy();
    if dry {
        planned("agy", &["plugin", "validate", "--json", &dir]);
    }
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
    if dry {
        planned("agy", &["plugin", "install", &dir]);
    } else {
        invoke("agy", &["plugin", "install", &dir])?;
    }
    paths::write_json(
        &marker,
        &json!({"version":entry.version, "source":source, "installed":!dry}),
    )?;
    Ok(if dry {
        "skipped (dry-run; would install)"
    } else {
        "installed"
    }
    .into())
}

fn copy_tree(source: &Path, dest: &Path) -> Res<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
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
fn build_agy(name: &str, version: &str, source: &Path, dest: &Path) -> Res<()> {
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
    copy_tree(&source.join("skills"), &dest.join("skills"))?;
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
    Ok(())
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
