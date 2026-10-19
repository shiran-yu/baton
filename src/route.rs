use crate::{
    brief::{self, Brief},
    ledger::{self, Entry},
    usage::Usage,
    Error, Res,
};
use serde::Serialize;

pub const TYPES: [&str; 8] = [
    "implement",
    "refactor",
    "bugfix",
    "test",
    "docs",
    "mechanical",
    "review",
    "research",
];
// 2026-11-02T00:00:00Z. The clock is an explicit routing input.
pub const THIRD_PARTY_CUTOFF: u64 = 1_793_577_600;

#[derive(Debug, Serialize)]
pub struct Classification {
    #[serde(rename = "type")]
    pub task_type: String,
    pub difficulty: u8,
    pub risky: bool,
}

pub fn classify(
    brief: &Brief,
    task_type: Option<&str>,
    difficulty: Option<u8>,
) -> Res<Classification> {
    if task_type.is_some_and(|t| !TYPES.contains(&t)) {
        return Err(Error::usage("invalid --type"));
    }
    if difficulty.is_some_and(|d| !(1..=4).contains(&d)) {
        return Err(Error::usage("--difficulty must be 1 through 4"));
    }
    let lower = brief.goal.to_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let rules: &[(&str, &[&str])] = &[
        ("bugfix", &["fix", "bug", "crash"]),
        (
            "mechanical",
            &["rename", "move", "format", "bump", "replace"],
        ),
        (
            "docs",
            &["doc", "docs", "documentation", "readme", "comment"],
        ),
        ("test", &["test", "tests", "coverage"]),
        ("refactor", &["refactor", "extract", "split"]),
        ("review", &["review", "audit"]),
        ("research", &["investigate", "explain", "find"]),
    ];
    let only_docs = !brief.files.is_empty()
        && brief.files.iter().all(|p| {
            let p = p.trim_start_matches("./").to_lowercase();
            p.ends_with(".md") || p.starts_with("docs/") || p.contains("/docs/")
        });
    let inferred = if only_docs {
        "docs"
    } else {
        rules
            .iter()
            .find(|(_, keys)| words.iter().any(|w| keys.contains(w)))
            .map(|r| r.0)
            .unwrap_or("implement")
    };
    let task_type = task_type.unwrap_or(inferred).to_string();
    let mut d = 2_i32;
    if brief.files.len() > 6 || brief.files.iter().any(|p| p.contains("**")) {
        d += 1;
    }
    if brief.goal.split_whitespace().count() > 120 {
        d += 1;
    }
    if ["mechanical", "docs"].contains(&task_type.as_str()) {
        d -= 1;
    }
    if brief.commands.is_empty() {
        d = 4;
    }
    Ok(Classification {
        task_type,
        difficulty: difficulty.unwrap_or(d.clamp(1, 4) as u8),
        risky: brief::lint(&brief.text).is_err(),
    })
}

pub fn posterior(agent: &str, task_type: &str, difficulty: u8, history: &[Entry]) -> (f64, usize) {
    // Research has no measured prior yet; use the neutral Beta(2, 2).
    let (a, b) = match (agent, task_type) {
        ("codex", "implement" | "refactor" | "bugfix" | "test" | "docs" | "mechanical") => {
            (3.0, 1.0)
        }
        ("codex", "review") => (2.0, 1.0),
        ("agy", "docs" | "mechanical") => (3.0, 1.0),
        ("agy", "implement" | "refactor" | "bugfix") if difficulty <= 2 => (1.5, 2.0),
        ("agy", "implement" | "refactor" | "bugfix") => (1.0, 3.0),
        _ => (2.0, 2.0),
    };
    let entries: Vec<_> = history
        .iter()
        .filter(|e| {
            e.agent == agent && e.task_type == task_type && e.difficulty == Some(difficulty)
        })
        .collect();
    let successes = entries.iter().filter(|e| e.first_pass()).count();
    (
        (successes as f64 + a) / (entries.len() as f64 + a + b),
        entries.len(),
    )
}

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub score: Option<f64>,
    pub p: Option<f64>,
    pub runs: usize,
    pub cost: Option<f64>,
    pub remaining: Option<f64>,
    pub window: Option<String>,
    pub pace: Option<f64>,
    pub skipped: Option<String>,
    pub reason: String,
}
#[derive(Serialize)]
pub struct Routing {
    pub choice: Candidate,
    pub runner_up: Option<Candidate>,
    pub candidates: Vec<Candidate>,
}

fn lead(reason: &str) -> Candidate {
    Candidate {
        agent: "claude".into(),
        model: None,
        effort: None,
        score: None,
        p: None,
        runs: 0,
        cost: None,
        remaining: None,
        window: None,
        pace: None,
        skipped: None,
        reason: reason.into(),
    }
}

pub fn hard_rule(class: &Classification, brief: &Brief) -> Option<&'static str> {
    if class.risky {
        Some("risky brief; keep with the lead")
    } else if brief.commands.is_empty() {
        Some("no acceptance commands; keep with the lead")
    } else if class.difficulty == 4 {
        Some("D4 task; keep with the lead")
    } else {
        None
    }
}

pub fn recommend(
    class: &Classification,
    brief: &Brief,
    history: &[Entry],
    usage: &Usage,
    now: u64,
) -> Routing {
    if let Some(reason) = hard_rule(class, brief) {
        let choice = lead(reason);
        return Routing {
            candidates: vec![choice.clone()],
            choice,
            runner_up: None,
        };
    }
    let d = class.difficulty;
    let mut models = vec![
        ("codex", "gpt-6.1-sol", false),
        ("agy", "gemini-3.8-flash", false),
    ];
    if now < THIRD_PARTY_CUTOFF && usage.has_third_party() {
        models.extend([
            ("agy", "claude-sonnet-4-6", true),
            ("agy", "claude-opus-4-6-thinking", true),
        ]);
    }
    let mut candidates = Vec::new();
    for (agent, model, third_party) in models {
        let (p, runs) = posterior(agent, &class.task_type, d, history);
        let cost = ledger::median(
            history
                .iter()
                .filter(|e| e.agent == agent && e.difficulty == Some(d))
                .map(Entry::total_tokens)
                .collect(),
        )
        .unwrap_or_else(|| {
            (if agent == "codex" {
                [60_000.0, 120_000.0, 250_000.0, 400_000.0]
            } else {
                [20_000.0, 40_000.0, 80_000.0, 150_000.0]
            })[(d - 1) as usize]
        });
        let effort = (!third_party).then(|| {
            ["low", "medium", "high", "xhigh"]
                [(if agent == "agy" { d.min(3) } else { d } - 1) as usize]
                .into()
        });
        let mut c = Candidate {
            agent: agent.into(),
            model: Some(model.into()),
            effort,
            score: None,
            p: Some(p),
            runs,
            cost: Some(cost),
            remaining: None,
            window: None,
            pace: None,
            skipped: None,
            reason: String::new(),
        };
        match usage.quota(agent, third_party) {
            Err(reason) => {
                c.skipped = Some(reason.clone());
                c.reason = reason;
            }
            Ok(q) => {
                c.remaining = Some(q.remaining);
                c.window = Some(q.window.clone());
                c.pace = Some(q.pace);
                if q.remaining < 1.5 * cost || q.remaining <= 0.0 {
                    c.skipped = Some("remaining quota below 1.5 × expected cost".into());
                } else {
                    c.score = Some(
                        p * 2_f64.powi(i32::from(d) - 1) - cost / q.remaining * q.pace.max(1.0),
                    );
                }
                c.reason = format!(
                    "{} D{d}, p={p:.2} from {runs} runs, {} tok vs {} left in {}, pace {:.1}{}",
                    class.task_type,
                    compact(cost),
                    compact(q.remaining),
                    q.window,
                    q.pace,
                    c.skipped
                        .as_deref()
                        .map(|s| format!("; skipped: {s}"))
                        .unwrap_or_default()
                );
            }
        }
        candidates.push(c);
    }
    // Stable sorting keeps deterministic codex/Gemini/model preference on ties.
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let eligible: Vec<_> = candidates
        .iter()
        .filter(|c| c.score.is_some())
        .cloned()
        .collect();
    let fallback = lead(if eligible.is_empty() {
        "no eligible workers; keep with the lead"
    } else {
        "lead fallback"
    });
    let choice = eligible
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.clone());
    let runner_up = eligible
        .get(1)
        .cloned()
        .or_else(|| (!eligible.is_empty()).then(|| fallback.clone()));
    candidates.push(fallback);
    Routing {
        choice,
        runner_up,
        candidates,
    }
}

pub fn compact(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1000.0 {
        format!("{:.0}k", n / 1000.0)
    } else {
        format!("{n:.0}")
    }
}

pub fn print(c: &Candidate) {
    println!(
        "{} {} {} — {}",
        c.agent,
        c.model.as_deref().unwrap_or("—"),
        c.effort.as_deref().unwrap_or("—"),
        c.reason
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn brief(goal: &str, files: &str, commands: &str) -> Brief {
        Brief::parse_for_route(format!("## Goal\n{goal}\n## Files\n{files}\n## Forbidden\nNone\n## Acceptance\n```sh\n{commands}\n```\n")).unwrap()
    }
    fn usage() -> Usage {
        Usage(json!({
            "codex":{"windows":[{"name":"5h","used":0.1,"pace":0.4},{"name":"weekly","used":0.1,"pace":0.3}]},
            "agy":{"windows":[{"id":"gemini-weekly","used":0.1,"pace":0.5}]}
        }))
    }
    fn recommend_brief(b: &Brief, u: &Usage) -> Routing {
        recommend(
            &classify(b, None, None).unwrap(),
            b,
            &[],
            u,
            THIRD_PARTY_CUTOFF,
        )
    }
    #[test]
    fn classification_keywords_files_and_overrides() {
        for (goal, kind, d) in [
            ("Add a widget", "implement", 2),
            ("Fix a crash", "bugfix", 2),
            ("Rename the widget", "mechanical", 1),
            ("Update a comment", "docs", 1),
            ("Improve test coverage", "test", 2),
            ("Extract a helper", "refactor", 2),
            ("Audit the widget", "review", 2),
            ("Investigate a slowdown", "research", 2),
            ("Add latest widget", "implement", 2),
        ] {
            let c = classify(&brief(goal, "src/main.rs", "true"), None, None).unwrap();
            assert_eq!((c.task_type.as_str(), c.difficulty), (kind, d), "{goal}");
        }
        let b = brief("Add a widget", "- `README.md`\n- docs/**", "true");
        let c = classify(&b, None, None).unwrap();
        assert_eq!((c.task_type.as_str(), c.difficulty), ("docs", 2));
        let c = classify(&b, Some("bugfix"), Some(1)).unwrap();
        assert_eq!((c.task_type.as_str(), c.difficulty), ("bugfix", 1));
        assert!(classify(&b, Some("invalid"), None).is_err());
        assert!(classify(&b, None, Some(5)).is_err());
        let mixed = brief("Add a widget", "README.md\nsrc/main.rs", "true");
        assert_eq!(classify(&mixed, None, None).unwrap().task_type, "implement");
    }
    #[test]
    fn difficulty_uses_entire_goal_and_file_boundaries() {
        let six = (0..6)
            .map(|i| format!("src/{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            classify(&brief("Build it", &six, "true"), None, None)
                .unwrap()
                .difficulty,
            2
        );
        assert_eq!(
            classify(
                &brief("Build it", &format!("{six}\nsrc/6.rs"), "true"),
                None,
                None
            )
            .unwrap()
            .difficulty,
            3
        );
        for (count, expected) in [(120, 3), (121, 4)] {
            let goal = (0..count).map(|_| "word").collect::<Vec<_>>().join("\n");
            assert_eq!(
                classify(&brief(&goal, "src/**", "true"), None, None)
                    .unwrap()
                    .difficulty,
                expected
            );
        }
        assert_eq!(
            classify(&brief("Format it", "src/main.rs", ""), None, None)
                .unwrap()
                .difficulty,
            4
        );
    }
    #[test]
    fn posterior_uses_first_pass_history_and_strata() {
        assert_eq!(posterior("codex", "implement", 2, &[]), (0.75, 0));
        assert_eq!(posterior("agy", "implement", 2, &[]), (1.5 / 3.5, 0));
        assert_eq!(posterior("agy", "bugfix", 3, &[]), (0.25, 0));
        assert_eq!(posterior("agy", "test", 2, &[]), (0.5, 0));
        assert_eq!(posterior("codex", "review", 2, &[]), (2.0 / 3.0, 0));
        assert_eq!(posterior("codex", "research", 2, &[]), (0.5, 0));
        let history: Vec<Entry> = include_str!("../tests/fixtures/ledger/scorecard.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(posterior("codex", "implement", 2, &history), (4.0 / 6.0, 2));
        let b = brief("Build it", "src/main.rs", "true");
        let r = recommend(
            &classify(&b, None, None).unwrap(),
            &b,
            &history,
            &usage(),
            THIRD_PARTY_CUTOFF,
        );
        let codex = r.candidates.iter().find(|c| c.agent == "codex").unwrap();
        assert_eq!(codex.cost, Some(240.0));
    }
    #[test]
    fn quota_and_pace_move_work_and_explain_skips() {
        let b = brief("Build it", "src/main.rs", "true");
        let mut u = usage();
        let normal = recommend_brief(&b, &u);
        assert_eq!(normal.choice.agent, "codex");
        assert_eq!(normal.choice.effort.as_deref(), Some("medium"));
        assert_eq!(normal.runner_up.unwrap().agent, "agy");
        assert!((normal.choice.score.unwrap() - (1.5 - 120_000.0 / 2_250_000.0)).abs() < 1e-10);
        assert!(normal.choice.reason.contains("pace 0.4"));
        u.0["codex"]["windows"][0]["used"] = json!(0.96);
        let low = recommend_brief(&b, &u);
        assert_eq!(low.choice.agent, "agy");
        assert_eq!(low.runner_up.as_ref().unwrap().agent, "claude");
        assert!(low
            .candidates
            .iter()
            .find(|c| c.agent == "codex")
            .unwrap()
            .skipped
            .is_some());
        u.0["codex"]["windows"][0]["used"] = json!(0.1);
        u.0["codex"]["windows"][1]["used"] = json!(0.995);
        assert_eq!(recommend_brief(&b, &u).choice.agent, "agy");
        u = usage();
        u.0["codex"]["windows"][0]["pace"] = json!(30.0);
        assert_eq!(recommend_brief(&b, &u).choice.agent, "agy");
        u.0["codex"]["error"] = json!("offline");
        u.0["agy"]["rateLimitReachedType"] = json!("weekly");
        assert_eq!(recommend_brief(&b, &u).choice.agent, "claude");
        assert_eq!(
            recommend_brief(&b, &Usage::default()).choice.agent,
            "claude"
        );
    }
    #[test]
    fn hard_rules_win_even_with_overrides() {
        for b in [
            brief("Read secrets", "src/main.rs", "true"),
            brief("Build it", "src/main.rs", ""),
        ] {
            let class = classify(&b, Some("docs"), Some(1)).unwrap();
            assert_eq!(
                recommend(&class, &b, &[], &usage(), THIRD_PARTY_CUTOFF)
                    .choice
                    .agent,
                "claude"
            );
        }
        let b = brief("Build it", "src/main.rs", "true");
        let class = classify(&b, None, Some(4)).unwrap();
        assert_eq!(
            recommend(&class, &b, &[], &usage(), THIRD_PARTY_CUTOFF)
                .choice
                .agent,
            "claude"
        );
    }
    #[test]
    fn third_party_requires_bucket_and_stops_at_cutoff() {
        let b = brief("Build it", "src/main.rs", "true");
        let class = classify(&b, None, None).unwrap();
        let mut u = usage();
        assert_eq!(
            recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1)
                .candidates
                .len(),
            3
        );
        u.0["agy"]["windows"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"3p-weekly","used":0.0}));
        let before = recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1);
        assert_eq!(before.candidates.len(), 5);
        for model in ["claude-sonnet-4-6", "claude-opus-4-6-thinking"] {
            let c = before
                .candidates
                .iter()
                .find(|c| c.model.as_deref() == Some(model))
                .unwrap();
            assert!(c.effort.is_none());
            assert!(c.skipped.is_none());
        }
        for now in [THIRD_PARTY_CUTOFF, THIRD_PARTY_CUTOFF + 1] {
            assert_eq!(recommend(&class, &b, &[], &u, now).candidates.len(), 3);
        }
    }
}
