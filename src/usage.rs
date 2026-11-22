use crate::{paths, process, Error, Res};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Window {
    pub agent: String,
    pub window: String,
    pub used: f64,
    pub resets_at: u64,
    pub window_secs: u64,
    pub pace: f64,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Source {
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entitlement: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_reset_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exhausted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_reached_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordinary_usage_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
    /// Codex: the strongest model the account lists and its highest effort (codex::best).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub fetched_at: u64,
    pub agents: Vec<Source>,
}
fn invalid() -> Error {
    Error::new(1, "invalid quota data")
}
fn number(v: &Value) -> Res<f64> {
    v.as_f64().filter(|n| n.is_finite()).ok_or_else(invalid)
}
fn integer(v: &Value) -> Res<u64> {
    v.as_u64().ok_or_else(invalid)
}
fn window(agent: &str, name: &str, used: f64, reset: u64, secs: u64, now: u64) -> Res<Window> {
    if secs == 0 {
        return Err(invalid());
    }
    let used = used.clamp(0.0, 1.0);
    let elapsed = (1.0 - (reset as f64 - now as f64) / secs as f64).clamp(0.01, 1.0);
    Ok(Window {
        agent: agent.into(),
        window: name.into(),
        used,
        resets_at: reset,
        window_secs: secs,
        pace: used / elapsed,
    })
}
pub fn parse_codex(v: &Value, now: u64, rollout: bool) -> Res<Source> {
    // rateLimits may describe another bucket (e.g. "premium" with null windows); prefer the codex one.
    let top = &v["result"]["rateLimits"];
    let by_id = &v["result"]["rateLimitsByLimitId"]["codex"];
    let rates = if rollout {
        v
    } else if by_id.is_object() {
        by_id
    } else {
        top
    };
    let limit = &rates[if rollout { "limit_id" } else { "limitId" }];
    if !limit.is_null() && limit != "codex" {
        return Err(invalid());
    }
    // Fields the per-id entry may omit come from rateLimits when it is the same bucket.
    let same = |key: &str| {
        rates[key].as_str().or_else(|| {
            (!rollout && (top["limitId"].is_null() || top["limitId"] == "codex"))
                .then(|| top[key].as_str())
                .flatten()
        })
    };
    let mut source = Source {
        agent: "codex".into(),
        stale: rollout,
        // The plan is per account, so any bucket's value will do.
        plan_type: rates[if rollout { "plan_type" } else { "planType" }]
            .as_str()
            .or_else(|| top["planType"].as_str())
            .map(String::from),
        rate_limit_reached_type: same(if rollout {
            "rate_limit_reached_type"
        } else {
            "rateLimitReachedType"
        })
        .map(String::from),
        ordinary_usage_allowed: v["result"]["ordinaryUsageAllowed"]
            .as_bool()
            .or_else(|| rates["ordinaryUsageAllowed"].as_bool()),
        ..Source::default()
    };
    for key in ["primary", "secondary"] {
        let w = &rates[key];
        if w.is_null() {
            continue;
        }
        let secs = integer(
            &w[if rollout {
                "window_minutes"
            } else {
                "windowDurationMins"
            }],
        )?
        .checked_mul(60)
        .ok_or_else(invalid)?;
        let name = match secs {
            18000 => "5h".into(),
            604800 => "weekly".into(),
            _ => format!("{}m", secs / 60),
        };
        source.windows.push(window(
            "codex",
            &name,
            number(
                &w[if rollout {
                    "used_percent"
                } else {
                    "usedPercent"
                }],
            )? / 100.0,
            integer(&w[if rollout { "resets_at" } else { "resetsAt" }])?,
            secs,
            now,
        )?);
    }
    if source.windows.is_empty() {
        return Err(invalid());
    }
    // An old rollout may predate a reset: a window past its reset is unused again.
    if rollout {
        for w in &mut source.windows {
            if w.resets_at <= now {
                w.used = 0.0;
                w.pace = 0.0;
            }
        }
        if source.windows.iter().all(|w| w.resets_at <= now) {
            source.rate_limit_reached_type = None;
        }
    }
    Ok(source)
}
fn live_codex() -> Res<Source> {
    let mut process = process::Running::spawn(
        Command::new(process::executable("codex")).arg("app-server"),
        true,
    )?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let input = process.child.stdin.as_mut().ok_or_else(invalid)?;
    writeln!(
        input,
        "{}",
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"baton","version":env!("CARGO_PKG_VERSION")}}})
    )?;
    input.flush()?;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::new(5, "codex quota read timed out"));
        }
        let line = process
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Error::new(5, "codex quota read failed or timed out"))?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["id"] == 1 {
            if !v["error"].is_null() {
                return Err(invalid());
            }
            let input = process.child.stdin.as_mut().ok_or_else(invalid)?;
            writeln!(input, "{}", json!({"method":"initialized"}))?;
            writeln!(
                input,
                "{}",
                json!({"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}})
            )?;
            input.flush()?;
        } else if v["id"] == 2 {
            // return parse_codex(&v, paths::now(), false);
            let mut source = parse_codex(&v, paths::now(), false)?;
            // Then the models this account may run; a failure or timeout leaves codex::MODEL.
            if let Some(input) = process.child.stdin.as_mut() {
                let _ = writeln!(input, "{}", json!({"id":3,"method":"model/list","params":{}}));
                let _ = input.flush();
            }
            let models_deadline = Instant::now() + Duration::from_secs(3);
            while let Ok(line) = process
                .lines
                .recv_timeout(models_deadline.saturating_duration_since(Instant::now()))
            {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v["id"] == 3 {
                    if let Some((model, effort)) = crate::codex::best(&v["result"]["data"]) {
                        source.model = Some(model);
                        source.effort = Some(effort);
                    }
                    break;
                }
            }
            return Ok(source);
        }
    }
}
fn rollouts(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            rollouts(&path, files);
        } else if kind.is_file()
            && entry.file_name().to_string_lossy().starts_with("rollout-")
            && path.extension().is_some_and(|s| s == "jsonl")
        {
            files.push(path);
        }
    }
}
fn fallback_codex() -> Res<Source> {
    let base = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".codex"));
    let mut files = Vec::new();
    rollouts(&base.join("sessions"), &mut files);
    files
        .sort_by_cached_key(|p| std::cmp::Reverse(fs::metadata(p).and_then(|m| m.modified()).ok()));
    if files.is_empty() {
        return Err(Error::new(1, "codex unavailable; no rollout quota cache"));
    }
    // Newest rollout first; a rollout may end with a non-codex bucket or none at all.
    for path in files.iter().take(10) {
        let Ok(file) = fs::File::open(path) else {
            continue;
        };
        let mut last = None;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("\"rate_limits\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let rates = v
                .get("rate_limits")
                .or_else(|| v.get("payload").and_then(|p| p.get("rate_limits")));
            if let Some(Ok(source)) = rates.map(|r| parse_codex(r, paths::now(), true)) {
                last = Some(source);
            }
        }
        if let Some(source) = last {
            return Ok(source);
        }
    }
    Err(Error::new(1, "no codex rate limits in recent rollouts"))
}
pub const COPILOT_ENDED: &str = "copilot unavailable: subscription ended";
pub fn parse_copilot(v: &Value, now: u64) -> Res<Source> {
    // An ended subscription reports no quota_snapshots at all.
    if v["access_type_sku"] == "subscription_ended" {
        return Err(Error::new(1, COPILOT_ENDED));
    }
    let q = &v["quota_snapshots"]["premium_interactions"];
    let entitlement = number(&q["entitlement"])?;
    let remaining = number(&q["remaining"])?;
    let percent = number(&q["percent_remaining"])?;
    if entitlement <= 0.0 || !(0.0..=100.0).contains(&percent) {
        return Err(invalid());
    }
    let date = v["quota_reset_date"].as_str().ok_or_else(invalid)?;
    if date.len() != 10 {
        return Err(invalid());
    }
    let reset = utc_epoch(&format!("{date}T00:00:00Z"))?;
    let year: u32 = date[..4].parse().map_err(|_| invalid())?;
    let month: u32 = date[5..7].parse().map_err(|_| invalid())?;
    let (year, month) = if month == 1 {
        (year.checked_sub(1).ok_or_else(invalid)?, 12)
    } else {
        (year, month - 1)
    };
    let start = utc_epoch(&format!("{year:04}-{month:02}-01T00:00:00Z"))?;
    Ok(Source {
        agent: "copilot".into(),
        entitlement: Some(entitlement),
        remaining: Some(remaining),
        percent_remaining: Some(percent),
        quota_reset_date: Some(date.into()),
        plan_type: v["copilot_plan"].as_str().map(String::from),
        windows: vec![window(
            "copilot",
            "premium-monthly",
            1.0 - percent / 100.0,
            reset,
            reset.checked_sub(start).ok_or_else(invalid)?,
            now,
        )?],
        ..Source::default()
    })
}
fn live_copilot() -> Res<Source> {
    let gh = || {
        let mut gh = Command::new("gh");
        gh.args(["api", "/copilot_internal/user"]);
        gh
    };
    // Read the quota of the account the worker runs as; the token is passed, never logged.
    let worker = std::env::var_os("COPILOT_GITHUB_TOKEN").and_then(|token| {
        process::capture(gh().env("GH_TOKEN", token), Duration::from_secs(20)).ok()
    });
    // A token gh rejects falls back to gh's own login.
    let raw = match worker {
        Some(raw) => raw,
        None => process::capture(&mut gh(), Duration::from_secs(20))
            .map_err(|e| Error::new(1, format!("copilot quota via gh unavailable: {e}")))?,
    };
    parse_copilot(&serde_json::from_str(&raw)?, paths::now())
}

pub fn parse_agy(v: &Value, now: u64) -> Res<Source> {
    let mut source = Source {
        agent: "agy".into(),
        ..Source::default()
    };
    // for group in v["command"]["data"]["groups"]
    //     .as_array()
    //     .ok_or_else(invalid)?
    // {
    //     for bucket in group["buckets"].as_array().ok_or_else(invalid)? {
    //         let id = bucket["id"].as_str().ok_or_else(invalid)?;
    //         if bucket["window"] != "weekly" {
    //             return Err(invalid());
    //         }
    //         source.windows.push(window(
    //             "agy",
    //             id,
    //             1.0 - number(&bucket["remaining_fraction"])?,
    //             utc_epoch(bucket["reset_time"].as_str().ok_or_else(invalid)?)?,
    //             604800,
    //             now,
    //         )?);
    //     }
    // }
    // Pro (2026-11-13): a weekly and a 5 h bucket per group plus an Argon group. Key on `window`;
    // a bucket that cannot be read is skipped, never the whole source.
    let groups = v["command"]["data"]["groups"]
        .as_array()
        .ok_or_else(invalid)?;
    for bucket in groups
        .iter()
        .filter_map(|g| g["buckets"].as_array())
        .flatten()
    {
        let (Some(id), Some(secs), Ok(left), Some(Ok(reset))) = (
            bucket["id"].as_str(),
            seconds(&bucket["window"]),
            number(&bucket["remaining_fraction"]),
            bucket["reset_time"].as_str().map(utc_epoch),
        ) else {
            continue;
        };
        source
            .windows
            .push(window("agy", id, 1.0 - left, reset, secs, now)?);
    }
    if source.windows.is_empty() {
        return Err(invalid());
    }
    // Argon first, then the Gemini group's 5 h and weekly, then the rest.
    source
        .windows
        .sort_by_key(|w| (agy_group(&w.window).0, w.window_secs));
    Ok(source)
}
// A bucket's `window`: "weekly", "daily", or a count with m, h or d ("5h").
fn seconds(window: &Value) -> Option<u64> {
    let text = window.as_str().filter(|s| s.is_ascii() && s.len() > 1)?;
    match text {
        "weekly" => Some(604800),
        "daily" => Some(86400),
        _ => {
            let (count, unit) = text.split_at(text.len() - 1);
            let unit = match unit {
                "m" => 60,
                "h" => 3600,
                "d" => 86400,
                _ => return None,
            };
            count
                .parse::<u64>()
                .ok()?
                .checked_mul(unit)
                .filter(|s| *s > 0)
        }
    }
}
// An agy bucket's sort rank and group label, from its id.
fn agy_group(id: &str) -> (u8, &str) {
    if id.starts_with("gemini-argon") {
        (0, "argon")
    } else if id.starts_with("gemini-") {
        (1, "gemini")
    } else if id.starts_with("3p-") {
        (2, "claude+gpt")
    } else {
        (3, id)
    }
}
/// An agy bucket's label: its group, then 5h or wk (`argon wk`, `gemini 5h`).
pub fn agy_label(w: &Window) -> String {
    let (rank, group) = agy_group(&w.window);
    match (rank, w.window_secs) {
        (3, _) => w.window.clone(),
        (_, 18000) => format!("{group} 5h"),
        (_, 604800) => format!("{group} wk"),
        _ => w.window.clone(),
    }
}
// Gregorian civil date conversion; accepts RFC 3339 UTC with optional fractional seconds.
fn utc_epoch(s: &str) -> Res<u64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(invalid());
    }
    let part = |a, z| -> Res<i64> {
        s.get(a..z)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())
    };
    let (mut y, m, d, h, min, sec) = (
        part(0, 4)?,
        part(5, 7)?,
        part(8, 10)?,
        part(11, 13)?,
        part(14, 16)?,
        part(17, 19)?,
    );
    let suffix = s.get(19..).ok_or_else(invalid)?;
    let suffix = if let Some(fraction) = suffix.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return Err(invalid());
        }
        &fraction[n..]
    } else {
        suffix
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(invalid()),
    };
    if !["Z", "+00:00"].contains(&suffix)
        || !(1..=days).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
        || !(0..60).contains(&sec)
    {
        return Err(invalid());
    }
    y -= i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let day = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + d - 1 - 719468;
    u64::try_from(day * 86400 + h * 3600 + min * 60 + sec).map_err(|_| invalid())
}
pub fn parse_claude(v: &Value, now: u64) -> Res<Source> {
    let u = &v["usage"];
    Ok(Source {
        agent: "claude".into(),
        fetched_at: Some(integer(&u["fetched_at"])?),
        windows: vec![
            window(
                "claude",
                "5h",
                number(&u["fh"])? / 100.0,
                integer(&u["fh_reset"])?,
                18000,
                now,
            )?,
            window(
                "claude",
                "7d",
                number(&u["sd"])? / 100.0,
                integer(&u["sd_reset"])?,
                604800,
                now,
            )?,
        ],
        ..Source::default()
    })
}
fn claude() -> Res<Source> {
    let dir = std::env::var_os("BATON_CLAUDE_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".claude/usage-accounts"));
    // Never propagate store contents or parser errors: these files can contain credentials.
    let entries = fs::read_dir(dir).map_err(|_| Error::new(1, "Claude usage store unavailable"))?;
    let mut latest = None;
    for entry in entries.flatten() {
        if entry.path().extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(seen) = v["seenAt"].as_u64() else {
            continue;
        };
        if latest.as_ref().is_none_or(|(n, _)| seen > *n) {
            latest = Some((seen, v));
        }
    }
    parse_claude(
        &latest
            .ok_or_else(|| Error::new(1, "no Claude usage accounts"))?
            .1,
        paths::now(),
    )
}
pub fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}
pub fn cache(home: &Path) -> Option<Snapshot> {
    paths::read_json(&home.join("usage.json")).ok()
}
/// Seconds an opencode exhausted mark holds: its free tier publishes no reset time.
const OPENCODE_COOLDOWN: u64 = 3600;

/// opencode's free DeepSeek is unmetered: the source only shows that the CLI runs.
fn live_opencode() -> Res<Source> {
    process::capture(
        Command::new(process::executable("opencode")).arg("--version"),
        Duration::from_secs(10),
    )
    .map_err(|_| Error::new(1, "opencode not installed"))?;
    Ok(Source {
        agent: "opencode".into(),
        plan_type: Some("free".into()),
        model: Some(crate::worker::OPENCODE_ALIAS.into()),
        effort: Some(crate::worker::OPENCODE_EFFORT.into()),
        ..Source::default()
    })
}

pub fn mark_exhausted(home: &Path, agent: &str, message: &str) -> Res<()> {
    let path = home.join("usage.json");
    let mut cache: Value = paths::read_json(&path)
        .unwrap_or_else(|_| json!({"fetched_at": paths::now(), "agents": []}));
    let key = if cache.get("sources").is_some() {
        Some("sources")
    } else if cache.get("agents").is_some() {
        Some("agents")
    } else {
        None
    };
    let root = match key {
        Some(key) => &mut cache[key],
        None => &mut cache,
    };
    let source = if let Some(sources) = root.as_array_mut() {
        let index = sources
            .iter()
            .position(|s| s["agent"] == agent || s["source"] == agent)
            .unwrap_or_else(|| {
                sources.push(json!({"agent":agent}));
                sources.len() - 1
            });
        &mut sources[index]
    } else {
        if !root.is_object() {
            *root = json!({});
        }
        if !root[agent].is_object() {
            root[agent] = json!({"agent":agent});
        }
        &mut root[agent]
    };
    source["error"] = json!(message);
    source["exhausted"] = json!(true);
    // When the mark was set: opencode's lapses after OPENCODE_COOLDOWN.
    if agent == "opencode" {
        source["fetched_at"] = json!(paths::now());
    }
    fs::create_dir_all(home)?;
    paths::write_json(&path, &cache)
}

pub fn read(home: &Path, refresh: bool) -> Res<Snapshot> {
    if !refresh {
        if let Some(snapshot) = cache(home) {
            if paths::now().saturating_sub(snapshot.fetched_at) < 300 {
                // if snapshot.agents.iter().any(|a| a.agent == "copilot") {
                // A cache from before opencode joined is read again.
                if ["copilot", "opencode"]
                    .iter()
                    .all(|agent| snapshot.agents.iter().any(|a| a.agent == *agent))
                {
                    return Ok(snapshot);
                }
            }
        }
    }
    type Reader = fn() -> Res<Source>;
    // let sources: [(&str, Reader); 4] = [
    let sources: [(&str, Reader); 5] = [
        ("codex", || {
            live_codex().or_else(|live| {
                fallback_codex().map_err(|stale| {
                    Error::new(stale.code, format!("live read: {live}; rollouts: {stale}"))
                })
            })
        }),
        ("agy", || {
            let raw = process::capture(
                Command::new(process::executable("agy")).args([
                    "-p",
                    "/usage",
                    "--output-format",
                    "json",
                ]),
                Duration::from_secs(20),
            )?;
            parse_agy(&serde_json::from_str(&raw)?, paths::now())
        }),
        ("claude", claude),
        ("copilot", live_copilot),
        ("opencode", live_opencode),
    ];
    let handles: Vec<_> = sources
        .into_iter()
        .map(|(agent, f)| (agent, std::thread::spawn(f)))
        .collect();
    let mut agents: Vec<Source> = handles
        .into_iter()
        .map(|(agent, h)| {
            h.join()
                .unwrap_or_else(|_| Err(Error::new(1, "quota reader failed")))
                .unwrap_or_else(|e| Source {
                    agent: agent.into(),
                    error: Some(e.to_string()),
                    ..Source::default()
                })
        })
        .collect();
    if !refresh {
        // Routing also accepts keyed/raw caches. Keep terminal failures sticky in all
        // supported cache layouts, even when the five-minute usage cache expires.
        let previous = crate::quota::Usage::cached(home);
        for current in &mut agents {
            // agy's /usage is free and per bucket: a live read replaces a sticky mark, so a run that
            // used up Argon never blocks Hydrogen past the next read.
            if current.agent == "agy" && current.error.is_none() {
                continue;
            }
            let source = previous.source(&current.agent);
            // opencode has no usage read to clear its mark: the mark lapses after an hour.
            if current.agent == "opencode"
                && paths::now().saturating_sub(source["fetched_at"].as_u64().unwrap_or(0))
                    >= OPENCODE_COOLDOWN
            {
                continue;
            }
            if source["exhausted"] == true {
                *current = serde_json::from_value(source.clone()).unwrap_or_else(|_| Source {
                    agent: current.agent.clone(),
                    error: source["error"].as_str().map(String::from),
                    exhausted: true,
                    ..Source::default()
                });
            }
        }
    }
    let snapshot = Snapshot {
        fetched_at: paths::now(),
        agents,
    };
    fs::create_dir_all(home)?;
    paths::write_json(&home.join("usage.json"), &snapshot)?;
    Ok(snapshot)
}
pub fn percent(used: f64) -> String {
    format!("{:.0}%", used * 100.0)
}
fn reset_time(epoch: u64) -> String {
    let secs = epoch.saturating_sub(paths::now());
    if secs >= 86400 {
        format!("{}d{:02}h", secs / 86400, secs / 3600 % 24)
    } else {
        format!("{}h{:02}m", secs / 3600, secs / 60 % 60)
    }
}
pub fn table(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    for s in &snapshot.agents {
        if let Some(error) = &s.error {
            out.push_str(&format!("{:<7} error: {error}\n", s.agent));
            continue;
        }
        out.push_str(&format!(
            "{:<7} {:<6}",
            s.agent,
            s.plan_type.as_deref().unwrap_or("")
        ));
        if s.agent == "copilot" {
            out.push_str(&format!(
                "  premium {}/{} left ({}%, resets {})",
                s.remaining.unwrap_or(0.0),
                s.entitlement.unwrap_or(0.0),
                s.percent_remaining.unwrap_or(0.0),
                s.quota_reset_date.as_deref().unwrap_or("unknown")
            ));
        }
        for w in s.windows.iter().filter(|_| s.agent != "copilot") {
            // let (name, amount, verb) = if s.agent == "agy" {
            //     (
            //         match w.window.as_str() {
            //             "gemini-weekly" => "gemini",
            //             "3p-weekly" => "claude+gpt",
            //             other => other,
            //         },
            //         1.0 - w.used,
            //         "left",
            //     )
            // } else {
            //     (w.window.as_str(), w.used, "used")
            // };
            let (name, amount, verb) = if s.agent == "agy" {
                (agy_label(w), 1.0 - w.used, "left")
            } else {
                (w.window.clone(), w.used, "used")
            };
            out.push_str(&format!(
                "  {name} {} {verb} (resets {})",
                percent(amount),
                reset_time(w.resets_at)
            ));
        }
        if s.agent == "opencode" {
            out.push_str("  deepseek at max, unmetered");
        }
        if s.agent == "codex" {
            out.push_str(&format!(
                "   pace {:.1}",
                s.windows.iter().map(|w| w.pace).fold(0.0, f64::max)
            ));
        }
        if s.stale {
            out.push_str(" [stale]");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_dates_and_invalid_calendar_values() {
        for (s, epoch) in [
            ("1970-01-01T00:00:00Z", 0),
            ("2000-02-29T12:34:56.123Z", 951827696),
            ("2026-10-26T12:50:34+00:00", 1793019034),
        ] {
            assert_eq!(utc_epoch(s).unwrap(), epoch);
        }
        for s in [
            "2026-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-10-26T24:00:00Z",
            "2026-10-26T00:00:00+01:00",
            "2026-10-26T00:00:00.Z",
            "garbage",
            "2026-10-26T00:00:00",
        ] {
            assert!(utc_epoch(s).is_err(), "{s}");
        }
    }
    #[test]
    fn agy_pro_buckets_parse_generically() {
        let mut v: Value =
            serde_json::from_str(include_str!("../tests/fixtures/usage/agy-usage-pro.json"))
                .unwrap();
        let now = utc_epoch("2026-11-13T04:00:00Z").unwrap();
        let s = parse_agy(&v, now).unwrap();
        let labels: Vec<_> = s.windows.iter().map(agy_label).collect();
        assert_eq!(
            labels,
            ["argon wk", "gemini 5h", "gemini wk", "claude+gpt 5h", "claude+gpt wk"]
        );
        assert_eq!(s.windows[0].window, "gemini-argon-wtu-weekly");
        assert!((s.windows[1].used - 0.6).abs() < 1e-12);
        assert_eq!(s.windows[1].window_secs, 18000);
        // An unknown window or a broken bucket is skipped, never fatal.
        let groups = &mut v["command"]["data"]["groups"];
        groups[0]["buckets"][0]["window"] = json!("fortnightly");
        groups[1]["buckets"][0]["remaining_fraction"] = json!("n/a");
        groups[2]["buckets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"gemini-argon-daily","window":"1d","remaining_fraction":0.5,"reset_time":"2026-11-14T00:00:00Z"}));
        let s = parse_agy(&v, now).unwrap();
        let ids: Vec<_> = s.windows.iter().map(|w| w.window.as_str()).collect();
        assert_eq!(
            ids,
            ["gemini-argon-daily", "gemini-argon-wtu-weekly", "gemini-5h", "3p-5h"]
        );
        v["command"]["data"]["groups"] = json!([]);
        assert!(parse_agy(&v, now).is_err());
    }

    #[test]
    fn pace_clamps_elapsed_and_used_and_preserves_codex_flags() {
        assert_eq!(
            window("codex", "5h", 0.2, 1100, 100, 1000).unwrap().pace,
            20.0
        );
        assert_eq!(
            window("codex", "5h", 0.2, 999, 100, 1000).unwrap().pace,
            0.2
        );
        assert_eq!(
            window("codex", "5h", 0.2, 1050, 100, 1000).unwrap().pace,
            0.4
        );
        assert_eq!(
            window("codex", "5h", 2.0, 999, 100, 1000).unwrap().used,
            1.0
        );
        assert_eq!(
            window("codex", "5h", -2.0, 999, 100, 1000).unwrap().used,
            0.0
        );
        let mut v: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/usage/codex-ratelimits.json"
        ))
        .unwrap();
        v["result"]["ordinaryUsageAllowed"] = json!(false);
        v["result"]["rateLimits"]["rateLimitReachedType"] = json!("primary");
        let s = parse_codex(&v, 1792420000, false).unwrap();
        assert_eq!(s.ordinary_usage_allowed, Some(false));
        assert_eq!(s.rate_limit_reached_type.as_deref(), Some("primary"));
    }
}
