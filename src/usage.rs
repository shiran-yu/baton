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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    let rates = if rollout {
        v
    } else {
        &v["result"]["rateLimits"]
    };
    let mut source = Source {
        agent: "codex".into(),
        stale: rollout,
        plan_type: rates[if rollout { "plan_type" } else { "planType" }]
            .as_str()
            .map(String::from),
        rate_limit_reached_type: rates[if rollout {
            "rate_limit_reached_type"
        } else {
            "rateLimitReachedType"
        }]
        .as_str()
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
            return parse_codex(&v, paths::now(), false);
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
    let file = fs::File::open(
        files
            .first()
            .ok_or_else(|| Error::new(1, "codex unavailable; no rollout quota cache"))?,
    )?;
    let mut last = None;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.contains("\"rate_limits\"") {
            last = Some(line);
        }
    }
    let v: Value = serde_json::from_str(&last.ok_or_else(invalid)?)?;
    let rates = v
        .get("rate_limits")
        .or_else(|| v.get("payload").and_then(|p| p.get("rate_limits")))
        .ok_or_else(invalid)?;
    parse_codex(rates, paths::now(), true)
}
pub fn parse_agy(v: &Value, now: u64) -> Res<Source> {
    let mut source = Source {
        agent: "agy".into(),
        ..Source::default()
    };
    for group in v["command"]["data"]["groups"]
        .as_array()
        .ok_or_else(invalid)?
    {
        for bucket in group["buckets"].as_array().ok_or_else(invalid)? {
            let id = bucket["id"].as_str().ok_or_else(invalid)?;
            if bucket["window"] != "weekly" {
                return Err(invalid());
            }
            source.windows.push(window(
                "agy",
                id,
                1.0 - number(&bucket["remaining_fraction"])?,
                utc_epoch(bucket["reset_time"].as_str().ok_or_else(invalid)?)?,
                604800,
                now,
            )?);
        }
    }
    if source.windows.is_empty() {
        return Err(invalid());
    }
    Ok(source)
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
pub fn read(home: &Path, refresh: bool) -> Res<Snapshot> {
    if !refresh {
        if let Some(snapshot) = cache(home) {
            if paths::now().saturating_sub(snapshot.fetched_at) < 300 {
                return Ok(snapshot);
            }
        }
    }
    type Reader = fn() -> Res<Source>;
    let sources: [(&str, Reader); 3] = [
        ("codex", || live_codex().or_else(|_| fallback_codex())),
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
    ];
    let handles: Vec<_> = sources
        .into_iter()
        .map(|(agent, f)| (agent, std::thread::spawn(f)))
        .collect();
    let agents = handles
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
        for w in &s.windows {
            let (name, amount, verb) = if s.agent == "agy" {
                (
                    match w.window.as_str() {
                        "gemini-weekly" => "gemini",
                        "3p-weekly" => "claude+gpt",
                        other => other,
                    },
                    1.0 - w.used,
                    "left",
                )
            } else {
                (w.window.as_str(), w.used, "used")
            };
            out.push_str(&format!(
                "  {name} {} {verb} (resets {})",
                percent(amount),
                reset_time(w.resets_at)
            ));
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
