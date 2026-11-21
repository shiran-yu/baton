//! Read-only view of the `baton usage` cache ($BATON_HOME/usage.json) for routing: no refreshes,
//! no worker processes. Also reads raw Codex rate limits and agy buckets.
//! Cache sources are keyed by agent (or in `sources`), with `windows` containing
//! name, used (fraction), and pace. Raw Codex rate limits and agy buckets also work.
use serde_json::Value;
use std::{fs, path::Path};

#[derive(Default)]
pub struct Usage(pub Value);

#[derive(Debug)]
pub struct Quota {
    pub remaining: f64,
    pub window: String,
    pub pace: f64,
}

impl Usage {
    pub fn cached(home: &Path) -> Self {
        Self(
            fs::read(home.join("usage.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or(Value::Null),
        )
    }

    /// Codex's model and effort: the strongest the account lists (cached by `baton usage`), else
    /// codex::MODEL at codex::EFFORT.
    pub fn codex_model(&self) -> (String, String) {
        let s = self.source("codex");
        match (s["model"].as_str(), s["effort"].as_str()) {
            (Some(model), Some(effort)) => (model.into(), effort.into()),
            _ => (crate::codex::MODEL.into(), crate::codex::EFFORT.into()),
        }
    }

    pub(crate) fn source(&self, agent: &str) -> &Value {
        let root = self
            .0
            .get("sources")
            .or_else(|| self.0.get("agents"))
            .unwrap_or(&self.0);
        if let Some(sources) = root.as_array() {
            sources
                .iter()
                .find(|s| s["agent"] == agent || s["source"] == agent)
                .unwrap_or(&Value::Null)
        } else {
            &root[agent]
        }
    }

    #[allow(dead_code)] // unused while agy's Claude route is commented out (2026-11-12)
    pub fn has_third_party(&self) -> bool {
        let source = self.source("agy");
        windows(source).iter().any(|w| name(w) == "3p-weekly")
    }

    // pub fn quota(&self, agent: &str, third_party: bool) -> std::result::Result<Quota, String> {
    /// For agy, only the buckets that meter `model` count (agy::meters); other agents ignore it.
    pub fn quota(&self, agent: &str, model: &str) -> std::result::Result<Quota, String> {
        let source = self.source(agent);
        if source.is_null() {
            return Err("no cached usage".into());
        }
        if source["error"] == crate::usage::COPILOT_ENDED {
            return Err(crate::usage::COPILOT_ENDED.into());
        }
        if has_error(source) {
            return Err("usage source errored or rate limited".into());
        }
        let data = source.get("data").unwrap_or(source);
        if has_error(data) {
            return Err("usage source errored or rate limited".into());
        }
        if agent == "copilot" {
            let remaining = data["remaining"]
                .as_f64()
                .or_else(|| {
                    data.pointer("/quota_snapshots/premium_interactions/remaining")
                        .and_then(Value::as_f64)
                })
                .filter(|n| n.is_finite())
                .ok_or_else(|| "invalid cached copilot premium quota".to_string())?;
            return Ok(Quota {
                remaining,
                window: "premium-monthly".into(),
                pace: 1.0,
            });
        }
        let limits = data
            .pointer("/result/rateLimits")
            .or_else(|| data.get("rateLimits"))
            .unwrap_or(data);
        if has_error(limits) {
            return Err("usage source errored or rate limited".into());
        }
        let mut remaining = f64::INFINITY;
        let mut tightest = String::new();
        let mut pace = source["pace"].as_f64();
        for w in windows(data) {
            let label = name(&w);
            // if agent == "agy"
            //     && label
            //         != if third_party {
            //             "3p-weekly"
            //         } else {
            //             "gemini-weekly"
            //         }
            // {
            //     continue;
            // }
            if agent == "agy" && !crate::agy::meters(model, &label) {
                continue;
            }
            let budget = if agent == "codex" {
                if label == "5h" || w["windowDurationMins"] == 300 || w["window_minutes"] == 300 {
                    2_500_000.0
                } else if label == "weekly"
                    || label == "week"
                    || label == "7d"
                    || w["windowDurationMins"] == 10080
                    || w["window_minutes"] == 10080
                {
                    12_000_000.0
                } else {
                    continue;
                }
            } else {
                6_000_000.0
            };
            if has_error(&w) {
                return Err("usage window errored or rate limited".into());
            }
            let used = w["used"]
                .as_f64()
                .or_else(|| w["used_fraction"].as_f64())
                .or_else(|| w["usedPercent"].as_f64().map(|v| v / 100.0))
                .or_else(|| w["used_percent"].as_f64().map(|v| v / 100.0))
                .or_else(|| w["remaining_fraction"].as_f64().map(|v| 1.0 - v));
            let Some(used) = used.filter(|u| u.is_finite() && (0.0..=1.0).contains(u)) else {
                return Err("invalid cached quota".into());
            };
            let left = (1.0 - used) * budget;
            if left < remaining {
                remaining = left;
                tightest = label;
            }
            if let Some(p) = w["pace"].as_f64() {
                pace = Some(pace.map_or(p, |current| current.max(p)));
            }
        }
        if !remaining.is_finite() {
            return Err("no cached quota windows".into());
        }
        Ok(Quota {
            remaining,
            window: tightest,
            pace: pace.unwrap_or(1.0),
        })
    }
}

fn has_error(v: &Value) -> bool {
    let error = &v["error"];
    (!error.is_null() && error != false && error != "")
        || v["status"] == "error"
        || v.get("rateLimitReachedType").is_some_and(|v| !v.is_null())
        || v.get("rate_limit_reached_type")
            .is_some_and(|v| !v.is_null())
}

fn name(w: &Value) -> String {
    w["id"]
        .as_str()
        .or_else(|| w["name"].as_str())
        .or_else(|| w["window"].as_str())
        .unwrap_or("")
        .into()
}

fn windows(source: &Value) -> Vec<Value> {
    let source = source.get("data").unwrap_or(source);
    if let Some(windows) = source["windows"].as_array() {
        return windows.clone();
    }
    if let Some(buckets) = source["buckets"].as_array() {
        return buckets.clone();
    }
    if let Some(groups) = source
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
    {
        return groups
            .iter()
            .filter_map(|g| g["buckets"].as_array())
            .flatten()
            .cloned()
            .collect();
    }
    let limits = source
        .pointer("/result/rateLimits")
        .or_else(|| source.get("rateLimits"))
        .unwrap_or(source);
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let mut w = limits.get(key)?.clone();
            w["name"] = if w["windowDurationMins"] == 300 {
                "5h"
            } else {
                "weekly"
            }
            .into();
            Some(w)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn codex_model_comes_from_the_cache_else_the_constants() {
        let cached = Usage(json!({"fetched_at": 1, "agents": [
            {"agent": "codex", "plan_type": "free", "model": "gpt-5.6-terra", "effort": "ultra"}]}));
        assert_eq!(cached.codex_model(), ("gpt-5.6-terra".into(), "ultra".into()));
        let old = Usage(json!({"fetched_at": 1, "agents": [{"agent": "codex", "windows": []}]}));
        assert_eq!(old.codex_model(), (crate::codex::MODEL.into(), crate::codex::EFFORT.into()));
        assert_eq!(Usage(Value::Null).codex_model().0, crate::codex::MODEL);
    }
    #[test]
    fn raw_fixture_limits_and_buckets() {
        let mut usage = Usage(json!({
            "sources": {
                "codex": serde_json::from_str::<Value>(include_str!("../tests/fixtures/usage/codex-ratelimits.json")).unwrap(),
                "agy": serde_json::from_str::<Value>(include_str!("../tests/fixtures/usage/agy-usage.json")).unwrap()
            }
        }));
        let q = usage.quota("codex", "").unwrap();
        assert_eq!(q.remaining, 2_325_000.0);
        assert_eq!(q.window, "5h");
        assert!(usage.has_third_party());
        assert_eq!(usage.quota("agy", "claude-opus-5-5").unwrap().remaining, 6_000_000.0);
        assert!(
            (usage.quota("agy", crate::agy::FLASH).unwrap().remaining - 0.5302495956420898 * 6_000_000.0).abs()
                < 1e-8
        );
        // A free-tier cache has no Argon bucket.
        assert!(usage.quota("agy", crate::agy::ARGON).is_err());
        usage.0["sources"]["codex"]["result"]["rateLimits"]["rateLimitReachedType"] =
            json!("primary");
        assert!(usage.quota("codex", "").is_err());
        usage.0["sources"]["agy"]["error"] = json!("unavailable");
        assert!(usage.quota("agy", "claude-opus-5-5").is_err());
    }

    #[test]
    fn pro_buckets_gate_each_model_by_its_tightest() {
        // `agy -p /usage --output-format json` on the Pro plan, 2026-11-13, with some use.
        let mut raw: Value =
            serde_json::from_str(include_str!("../tests/fixtures/usage/agy-usage-pro.json"))
                .unwrap();
        let usage = |raw: &Value| Usage(json!({"sources":{"agy":raw}}));
        let q = usage(&raw).quota("agy", crate::agy::ARGON).unwrap();
        assert_eq!(q.window, "gemini-argon-wtu-weekly");
        assert!((q.remaining - 0.75 * 6_000_000.0).abs() < 1e-6);
        // Hydrogen: the 5 h window is the tighter of the Gemini group's two.
        let q = usage(&raw).quota("agy", crate::agy::HYDROGEN).unwrap();
        assert_eq!(q.window, "gemini-5h");
        assert!((q.remaining - 0.4 * 6_000_000.0).abs() < 1e-6);
        raw["command"]["data"]["groups"][2]["buckets"][0]["remaining_fraction"] = json!(0.0);
        assert_eq!(usage(&raw).quota("agy", crate::agy::ARGON).unwrap().remaining, 0.0);
        assert!(usage(&raw).quota("agy", crate::agy::HYDROGEN).unwrap().remaining > 0.0);
    }
}
