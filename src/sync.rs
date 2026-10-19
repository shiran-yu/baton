//! Mirror the user's plugins that already provide native host manifests.
use crate::{paths, process, setup, usage, Error, Res};
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path, process::Command};

fn run_agent(agent: &str, args: &[&str]) -> Res<()> {
    let status = Command::new(process::executable(agent))
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(1, format!("{agent} {} failed", args.join(" "))))
    }
}
pub fn run() -> Res<i32> {
    let plugins = usage::user_home().join(".claude/plugins");
    let installed = plugins.join("installed_plugins.json");
    if !installed.exists() {
        println!("sync: no installed Claude Code plugins");
        return Ok(0);
    }
    let registry: Value = paths::read_json(&installed)?;
    let known = plugins.join("known_marketplaces.json");
    let known: Value = if known.exists() {
        paths::read_json(&known)?
    } else {
        Value::Null
    };
    let mut marketplaces = BTreeSet::new();
    let mut synced = BTreeSet::new();
    if let Some(plugins) = registry["plugins"].as_object() {
        for (selector, installs) in plugins {
            let Some((name, market)) = selector.split_once('@') else {
                continue;
            };
            let source = &known[market]["source"];
            let Some(repo) = source["repo"]
                .as_str()
                .filter(|r| r.starts_with("shiran-yu/"))
            else {
                continue;
            };
            for install in installs.as_array().into_iter().flatten() {
                let Some(path) = install["installPath"].as_str() else {
                    continue;
                };
                let path = Path::new(path);
                if path.join(".codex-plugin/plugin.json").is_file()
                    && synced.insert((selector.clone(), "codex"))
                {
                    if !setup::installed("codex") {
                        return Err(Error::usage("sync requires codex; run baton setup"));
                    }
                    // A checkout may expose the actual Codex marketplace name; cached plugin-only
                    // installs use the user's convention of a '-codex' marketplace suffix.
                    let codex_market = path
                        .ancestors()
                        .find_map(|ancestor| {
                            let text = fs::read_to_string(
                                ancestor.join(".agents/plugins/marketplace.json"),
                            )
                            .ok()?;
                            serde_json::from_str::<Value>(&text).ok()?["name"]
                                .as_str()
                                .map(String::from)
                        })
                        .unwrap_or_else(|| format!("{market}-codex"));
                    if marketplaces.insert(repo.to_string()) {
                        run_agent("codex", &["plugin", "marketplace", "add", repo])?;
                    }
                    run_agent(
                        "codex",
                        &["plugin", "add", &format!("{name}@{codex_market}")],
                    )?;
                }
                if path.join("plugin.json").is_file() && synced.insert((selector.clone(), "agy")) {
                    if !setup::installed("agy") {
                        return Err(Error::usage("sync requires agy; run baton setup"));
                    }
                    run_agent(
                        "agy",
                        &[
                            "plugin",
                            "install",
                            path.to_str()
                                .ok_or_else(|| Error::usage("plugin path must be UTF-8"))?,
                        ],
                    )?;
                }
            }
        }
    }
    println!("sync: installed {} native plugin copies", synced.len());
    Ok(0)
}
