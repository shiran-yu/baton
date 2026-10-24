use crate::{paths, usage, Res};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub fn run(home: &Path, mode: &str) {
    if mode == "line" {
        if let Some(snapshot) = usage::cache(home) {
            let line = compact(&snapshot);
            if !line.is_empty() {
                println!("{line}");
            }
        }
    } else {
        let result = (|| -> Res<String> {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            subagent(home, &serde_json::from_str(&input)?)
        })();
        if let Ok(output) = result {
            print!("{output}");
        }
    }
}
fn quota(snapshot: &usage::Snapshot, agent: &str, compact: bool) -> String {
    snapshot
        .agents
        .iter()
        .find(|s| s.agent == agent)
        .map(|s| {
            s.windows
                .iter()
                .map(|w| {
                    let name = match w.window.as_str() {
                        "weekly" => "wk",
                        "gemini-weekly" => "gem",
                        "3p-weekly" => "3p",
                        other => other,
                    };
                    format!(
                        "{name} {}",
                        usage::percent(if agent == "agy" { 1.0 - w.used } else { w.used })
                    )
                })
                .collect::<Vec<_>>()
                .join(if compact { " " } else { " · " })
        })
        .unwrap_or_default()
}
pub fn compact(snapshot: &usage::Snapshot) -> String {
    [("codex", "CX"), ("agy", "AG"), ("claude", "CC")]
        .into_iter()
        .filter_map(|(agent, label)| {
            let q = quota(snapshot, agent, true);
            (!q.is_empty()).then(|| format!("{label} {q}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
fn agent(row: &Value) -> Option<&str> {
    match row["agentType"].as_str()? {
        "baton:codex" | "codex" => Some("codex"),
        "baton:agy" | "agy" => Some("agy"),
        "baton:copilot" | "copilot" => Some("copilot"),
        _ => None,
    }
}
fn task_id(text: &str) -> Option<&str> {
    text.as_bytes().windows(22).enumerate().find_map(|(i, b)| {
        (b.starts_with(b"t-")
            && b[2..10].iter().all(u8::is_ascii_digit)
            && b[10] == b'-'
            && b[11..17].iter().all(u8::is_ascii_digit)
            && b[17] == b'-'
            && b[18..22]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)))
        .then(|| text.get(i..i + 22))
        .flatten()
    })
}
fn explicit(row: &Value) -> Option<&str> {
    ["description", "label"]
        .into_iter()
        .find_map(|k| row[k].as_str().and_then(task_id))
}
fn recent(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(home.join("tasks")) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries
        .flatten()
        .map(|e| e.path().join("status.json"))
        .collect();
    entries.sort_by_cached_key(|p| {
        (
            std::cmp::Reverse(fs::metadata(p).and_then(|m| m.modified()).ok()),
            std::cmp::Reverse(p.clone()),
        )
    });
    entries.truncate(20);
    entries
}
pub fn subagent(home: &Path, input: &Value) -> Res<String> {
    let rows = input["tasks"]
        .as_array()
        .ok_or_else(|| crate::Error::new(1, "invalid tasks"))?;
    let snapshot = usage::cache(home);
    // Reserve explicit references before time matching so a fallback cannot claim them.
    let mut claimed: HashSet<String> = rows
        .iter()
        .filter(|r| agent(r).is_some())
        .filter_map(explicit)
        .map(String::from)
        .collect();
    let needs_recent = rows
        .iter()
        .any(|r| agent(r).is_some() && explicit(r).is_none());
    let mut candidates: Vec<Value> = if needs_recent {
        recent(home)
            .iter()
            .filter_map(|p| paths::read_json(p).ok())
            .collect()
    } else {
        Vec::new()
    };
    candidates.sort_by_key(|s| std::cmp::Reverse(s["started"].as_u64().unwrap_or(0)));
    let mut output = String::new();
    let mut emitted = HashSet::new();
    for row in rows {
        let Some(agent) = agent(row) else {
            continue;
        };
        let Some(id) = row["id"].as_str() else {
            return Err(crate::Error::new(1, "missing row id"));
        };
        let status = if let Some(task) = explicit(row) {
            if !emitted.insert(task.to_string()) {
                continue;
            }
            paths::read_json::<Value>(&home.join("tasks").join(task).join("status.json"))
                .ok()
                .filter(|s| s["agent"] == agent)
        } else {
            let Some(start) = row["startTime"].as_f64() else {
                continue;
            };
            candidates
                .iter()
                .find(|s| {
                    let Some(task) = s["task"].as_str() else {
                        return false;
                    };
                    let Some(started) = s["started"].as_f64() else {
                        return false;
                    };
                    s["agent"] == agent
                        && !claimed.contains(task)
                        && (-5000.0..=600000.0).contains(&(started * 1000.0 - start))
                })
                .cloned()
                .inspect(|s| {
                    if let Some(task) = s["task"].as_str() {
                        claimed.insert(task.to_string());
                    }
                })
        };
        let Some(status) = status else {
            continue;
        };
        let mut parts = vec![agent.to_string()];
        if let Some(effort) = status["effort"].as_str().filter(|s| !s.is_empty()) {
            parts.push(effort.into());
        }
        for (key, label) in [("files", "files"), ("commands", "cmds")] {
            if let Some(n) = status[key].as_u64().filter(|n| *n > 0) {
                parts.push(format!("{n} {label}"));
            }
        }
        if let Some(tests) = status["tests"].as_str().filter(|s| !s.is_empty()) {
            parts.push(format!("tests {tests}"));
        }
        let tokens = &status["tokens"];
        let total = tokens["input"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(tokens["output"].as_u64().unwrap_or(0));
        if let Some(n) = tokens["premium_requests"].as_f64().filter(|n| *n > 0.0) {
            parts.push(format!(
                "{n} premium request{}",
                if n == 1.0 { "" } else { "s" }
            ));
        }
        if total > 0 {
            // Codex reports usage only when a turn ends; leave it out until then
            parts.push(if total >= 1000 {
                format!("{}k tok", total / 1000)
            } else {
                format!("{total} tok")
            });
        }
        if let Some(snapshot) = &snapshot {
            let q = quota(snapshot, agent, false);
            if !q.is_empty() {
                parts.push(q);
            }
        }
        output.push_str(&serde_json::to_string(
            &json!({"id":id,"content":parts.join(" · ")}),
        )?);
        output.push('\n');
    }
    Ok(output)
}
