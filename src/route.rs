use crate::{
    brief::{self, Brief},
    ledger::{self, Entry},
    quota::Usage,
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
#[allow(dead_code)] // unused while agy's Claude route is commented out (2026-11-12)
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
    let headline = brief
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            let end = brief
                .goal
                .char_indices()
                .find_map(|(i, c)| {
                    (matches!(c, '.' | '!' | '?')
                        && brief.goal[i + c.len_utf8()..]
                            .chars()
                            .next()
                            .is_none_or(char::is_whitespace))
                    .then_some(i + c.len_utf8())
                })
                .unwrap_or(brief.goal.len());
            &brief.goal[..end]
        });
    let rules: &[(&str, &[&str])] = &[
        ("bugfix", &["fix", "bug", "crash"]),
        (
            "mechanical",
            &["rename", "move", "format", "bump", "replace"],
        ),
        (
            "docs",
            &[
                "doc",
                "docs",
                "document",
                "documentation",
                "readme",
                "comment",
            ],
        ),
        ("test", &["test", "tests", "coverage"]),
        ("refactor", &["refactor", "extract", "split"]),
        ("review", &["review", "audit"]),
        ("research", &["investigate", "explain", "find"]),
    ];
    let words = |text: &str| -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect()
    };
    let primary = words(headline);
    let context = words(&brief.text);
    let candidates: Vec<_> = rules
        .iter()
        .filter(|(_, keys)| primary.iter().any(|w| keys.contains(&w.as_str())))
        .collect();
    let inferred = if candidates.len() > 1 {
        candidates
            .iter()
            .max_by_key(|(_, keys)| {
                context
                    .iter()
                    .filter(|w| keys.contains(&w.as_str()))
                    .count()
            })
            .map(|r| r.0)
            .unwrap_or("implement")
    } else if let Some(rule) = candidates.first() {
        rule.0
    } else {
        "implement"
    };
    let task_type = task_type.unwrap_or(inferred).to_string();
    let mut d = 2_i32;
    // A wide scope or a very long goal makes a first-pass success less likely. One or two
    // recursive globs (`src/**`, `tests/**`) are an ordinary scope, and a detailed goal with
    // acceptance commands is a well-specified task, so the thresholds are generous.
    if brief.files.len() > 6 || brief.files.iter().filter(|p| p.contains("**")).count() > 2 {
        d += 1;
    }
    if brief.goal.split_whitespace().count() > 300 {
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

// Expected cost with no history: tokens, or premium requests for copilot.
fn default_cost(agent: &str, d: u8) -> f64 {
    match agent {
        "copilot" => 1.0,
        "codex" => [60_000.0, 120_000.0, 250_000.0, 400_000.0][(d - 1) as usize],
        _ => [20_000.0, 40_000.0, 80_000.0, 150_000.0][(d - 1) as usize],
    }
}

// The eligibility rule: why a worker with this quota cannot take a task of this cost.
pub fn short(agent: &str, remaining: f64, cost: f64) -> Option<&'static str> {
    if agent == "copilot" && remaining <= crate::copilot::floor() {
        Some("copilot premium quota at or below floor")
    } else if remaining < 1.5 * cost || remaining <= 0.0 {
        Some("remaining quota below 1.5 × expected cost")
    } else {
        None
    }
}

/// Whether the cached quota lets agy run `model` on a task of difficulty `d`, by the tightest
/// bucket metering it. With no cached agy usage at all the policy's first choice stands.
pub fn agy_allowed(usage: &Usage, model: &str, d: u8) -> bool {
    match usage.quota("agy", model) {
        Ok(q) => short("agy", q.remaining, default_cost("agy", d)).is_none(),
        Err(_) => usage.source("agy").is_null(),
    }
}

/// Prompt-hook advice from cached quota: the first worker eligible for a default D2 task.
pub fn hint(usage: &Usage) -> String {
    for agent in ["codex", "agy", "copilot"] {
        // let quota = usage.quota(agent, false);
        // agy is eligible when any model of its D2 tiers is.
        let models = if agent == "agy" {
            crate::agy::tiers("implement", 2)
        } else {
            vec![""]
        };
        let eligible = models.into_iter().any(|model| {
            usage
                .quota(agent, model)
                .is_ok_and(|q| short(agent, q.remaining, default_cost(agent, 2)).is_none())
        });
        if eligible {
            let read = if agent == "copilot" {
                " (read-only)"
            } else {
                ""
            };
            return format!("baton:{agent} first{read}");
        }
    }
    "workers exhausted; keep the work with the lead".into()
}

pub fn recommend(
    class: &Classification,
    brief: &Brief,
    history: &[Entry],
    usage: &Usage,
    // now: u64,
    _now: u64, // only agy's Claude cutoff read it (commented out 2026-11-12)
    mode: &str,
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
    // Quota and task type pick the worker, never a weaker model or effort (user 2026-10-29).
    // agy: Claude Opus on the 3p bucket until it leaves agy, else Gemini; strongest first.
    // agy runs Gemini by default since 2026-11-12 (user); a Claude model only through --model.
    // let mut models = vec![("codex", crate::codex::MODEL, false)];
    // if now < THIRD_PARTY_CUTOFF && usage.has_third_party() {
    //     models.push(("agy", crate::agy::MODELS[0], true));
    // }
    // models.push(("agy", crate::agy::GEMINI, false));
    // agy since 2026-11-13 (user): the model policy's tiers in agy.rs, strongest first, each gated
    // by the buckets that meter it (Argon, else Hydrogen; tool work Hydrogen).
    // Since 2026-11-18 (user): Argon, then Neon, then Hydrogen, for every task.
    // let mut models = vec![("codex", crate::codex::MODEL)];
    // Codex: the strongest model the account lists, at its highest effort (2026-11-21).
    let (codex_model, codex_effort) = usage.codex_model();
    let mut models = vec![("codex", codex_model.as_str())];
    for model in crate::agy::tiers(&class.task_type, d) {
        models.push(("agy", model));
    }
    if mode == "read" && d <= 2 {
        // models.push(("copilot", "auto", false));
        models.push(("copilot", "auto"));
    }
    let mut candidates = Vec::new();
    // for (agent, model, third_party) in models {
    for (agent, model) in models {
        let (p, runs) = posterior(agent, &class.task_type, d, history);
        let cost = if agent == "copilot" {
            default_cost(agent, d)
        } else {
            ledger::median(
                history
                    .iter()
                    .filter(|e| e.agent == agent && e.difficulty == Some(d))
                    .map(Entry::total_tokens)
                    .collect(),
            )
            .unwrap_or_else(|| default_cost(agent, d))
        };
        // agy's effort is the highest `agy models` lists, found at run time.
        let effort = match agent {
            // "codex" => Some(crate::codex::EFFORT.into()),
            "codex" => Some(codex_effort.clone()),
            "copilot" => Some(["low", "medium", "high", "xhigh"][(d - 1) as usize].into()),
            _ => None,
        };
        let mut c = Candidate {
            agent: agent.into(),
            model: Some(if agent == "copilot" {
                crate::copilot::model()
            } else {
                model.into()
            }),
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
        // match usage.quota(agent, third_party) {
        match usage.quota(agent, model) {
            Err(reason) => {
                c.skipped = Some(reason.clone());
                c.reason = reason;
            }
            Ok(q) => {
                c.remaining = Some(q.remaining);
                c.window = Some(q.window.clone());
                c.pace = Some(q.pace);
                if let Some(reason) = short(agent, q.remaining, cost) {
                    c.skipped = Some(reason.into());
                } else if candidates
                    .iter()
                    .any(|o: &Candidate| o.agent == agent && o.score.is_some())
                {
                    c.skipped = Some(format!("a stronger {agent} model is eligible"));
                } else {
                    c.score = Some(
                        p * 2_f64.powi(i32::from(d) - 1) - cost / q.remaining * q.pace.max(1.0),
                    );
                }
                c.reason = format!(
                    "{} D{d}, p={p:.2} from {runs} runs, {} {} vs {} left in {}, pace {:.1}{}",
                    class.task_type,
                    compact(cost),
                    if agent == "copilot" {
                        "premium request"
                    } else {
                        "tok"
                    },
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
            "edit",
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
        assert_eq!((c.task_type.as_str(), c.difficulty), ("implement", 2)); // one recursive glob is ordinary
        let c = classify(&b, Some("bugfix"), Some(1)).unwrap();
        assert_eq!((c.task_type.as_str(), c.difficulty), ("bugfix", 1));
        assert!(classify(&b, Some("invalid"), None).is_err());
        assert!(classify(&b, None, Some(5)).is_err());
        let mixed = brief("Add a widget", "README.md\nsrc/main.rs", "true");
        assert_eq!(classify(&mixed, None, None).unwrap().task_type, "implement");
    }
    #[test]
    fn repository_briefs_and_headline_priority() {
        for (text, expected) in [
            (
                include_str!("../tests/fixtures/briefs/brief-c5-sync-adapt.md"),
                "implement",
            ),
            (
                include_str!("../tests/fixtures/briefs/brief-c6-setup.md"),
                "implement",
            ),
            (
                include_str!("../tests/fixtures/briefs/mechanical.md"),
                "mechanical",
            ),
            (include_str!("../tests/fixtures/briefs/docs.md"), "docs"),
        ] {
            let b = Brief::parse_for_route(text.into()).unwrap();
            assert_eq!(classify(&b, None, None).unwrap().task_type, expected);
        }
        for (goal, expected) in [
            (
                "Add a widget. Fix bugs, rename modules, and update docs and tests.",
                "implement",
            ),
            ("Rename a widget! Fix bugs and add tests.", "mechanical"),
            (
                "Add a src/widget.rs module. Fix bugs in tests.",
                "implement",
            ),
        ] {
            assert_eq!(
                classify(&brief(goal, "src/**", "true"), None, None)
                    .unwrap()
                    .task_type,
                expected
            );
        }
        let b = Brief::parse_for_route("## Title\nRename the widget\n## Goal\nFix crashes and add tests.\n## Files\nsrc/**\n## Forbidden\nNone\n## Acceptance\n```sh\ntrue\n```".into()).unwrap();
        assert_eq!(classify(&b, None, None).unwrap().task_type, "mechanical");
        let b = brief(
            "Review and refactor the module. Refactor the parser and extract helpers.",
            "src/**",
            "true",
        );
        assert_eq!(classify(&b, None, None).unwrap().task_type, "refactor");
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
        for (count, expected) in [(300, 2), (301, 3)] {
            let goal = (0..count).map(|_| "word").collect::<Vec<_>>().join("\n");
            assert_eq!(
                classify(&brief(&goal, "src/**\ntests/**", "true"), None, None)
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
            "edit",
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
        assert_eq!(normal.choice.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(normal.choice.effort.as_deref(), Some("xhigh"));
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
                recommend(&class, &b, &[], &usage(), THIRD_PARTY_CUTOFF, "edit")
                    .choice
                    .agent,
                "claude"
            );
        }
        let b = brief("Build it", "src/main.rs", "true");
        let class = classify(&b, None, Some(4)).unwrap();
        assert_eq!(
            recommend(&class, &b, &[], &usage(), THIRD_PARTY_CUTOFF, "edit")
                .choice
                .agent,
            "claude"
        );
    }
    // Until 2026-11-12 routing proposed agy's Claude Opus on the 3p bucket before the cutoff:
    // #[test]
    // fn third_party_requires_bucket_and_stops_at_cutoff() {
    //     let b = brief("Build it", "src/main.rs", "true");
    //     let class = classify(&b, None, None).unwrap();
    //     let mut u = usage();
    //     assert_eq!(
    //         recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1, "edit")
    //             .candidates
    //             .len(),
    //         3
    //     );
    //     u.0["agy"]["windows"]
    //         .as_array_mut()
    //         .unwrap()
    //         .push(json!({"id":"3p-weekly","used":0.0}));
    //     let before = recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1, "edit");
    //     assert_eq!(before.candidates.len(), 4);
    //     let agy = |r: &Routing, model: &str| {
    //         r.candidates
    //             .iter()
    //             .find(|c| c.model.as_deref() == Some(model))
    //             .cloned()
    //             .unwrap()
    //     };
    //     let opus = agy(&before, "claude-opus-4-6-thinking");
    //     assert!(opus.effort.is_none() && opus.skipped.is_none() && opus.score.is_some());
    //     // agy runs its strongest eligible model, whatever the Gemini bucket holds.
    //     let gemini = agy(&before, "gemini-3.8-flash");
    //     assert_eq!(
    //         gemini.skipped.as_deref(),
    //         Some("a stronger agy model is eligible")
    //     );
    //     assert!(gemini.score.is_none());
    //     u.0["agy"]["windows"][1]["used"] = json!(0.999);
    //     let short = recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1, "edit");
    //     assert!(agy(&short, "gemini-3.8-flash").score.is_some());
    //     for now in [THIRD_PARTY_CUTOFF, THIRD_PARTY_CUTOFF + 1] {
    //         let after = recommend(&class, &b, &[], &u, now, "edit");
    //         assert_eq!(after.candidates.len(), 3);
    //         assert!(agy(&after, "gemini-3.8-flash").effort.is_none());
    //     }
    // }

    // From 2026-11-12 to 11-13 (user) agy was routed on Gemini 3.8 Flash only:
    // #[test]
    // fn agy_routes_gemini_only() {
    //     let b = brief("Build it", "src/main.rs", "true");
    //     let class = classify(&b, None, None).unwrap();
    //     let mut u = usage();
    //     u.0["agy"]["windows"]
    //         .as_array_mut()
    //         .unwrap()
    //         .push(json!({"id":"3p-weekly","used":0.0}));
    //     let r = recommend(&class, &b, &[], &u, THIRD_PARTY_CUTOFF - 1, "edit");
    //     assert_eq!(r.candidates.len(), 3);
    //     assert!(r
    //         .candidates
    //         .iter()
    //         .all(|c| c.model.as_deref() != Some("claude-opus-4-6-thinking")));
    //     let gemini = r
    //         .candidates
    //         .iter()
    //         .find(|c| c.model.as_deref() == Some("gemini-3.8-flash"))
    //         .unwrap();
    //     assert!(gemini.score.is_some() && gemini.skipped.is_none());
    // }

    // Since 2026-11-13 (user): agy's model policy (agy.rs), each model gated by its own buckets.
    // Since 2026-11-18 (user): one chain for every task, Argon, then Neon, then Hydrogen.
    #[test]
    // fn agy_routes_argon_for_judgement_and_hydrogen_for_tool_work() {
    //     use crate::agy::{ARGON, HYDROGEN};
    fn agy_routes_argon_then_neon_then_hydrogen() {
        use crate::agy::{ARGON, HYDROGEN, NEON};
        let mut u = usage();
        u.0["codex"]["error"] = json!("usage limit reached");
        u.0["agy"]["windows"].as_array_mut().unwrap().extend([
            json!({"id":"gemini-argon-wtu-weekly","used":0.1,"pace":0.5}),
            json!({"id":"gemini-5h","used":0.2,"pace":0.5}),
            json!({"id":"3p-weekly","used":0.0}),
        ]);
        let route = |goal: &str, u: &Usage| {
            let b = brief(goal, "src/main.rs", "true");
            recommend(&classify(&b, None, None).unwrap(), &b, &[], u, THIRD_PARTY_CUTOFF, "read")
        };
        let agy = |r: &Routing| -> Vec<(String, bool)> {
            r.candidates
                .iter()
                .filter(|c| c.agent == "agy")
                .map(|c| (c.model.clone().unwrap(), c.score.is_some()))
                .collect()
        };
        let review = route("Review the parser", &u);
        assert_eq!(review.choice.model.as_deref(), Some(ARGON));
        // agy's effort is the highest `agy models` lists, found at run time.
        assert!(review.choice.effort.is_none());
        // assert_eq!(agy(&review), [(ARGON.to_string(), true), (HYDROGEN.to_string(), false)]);
        let chain = [(ARGON.to_string(), true), (NEON.to_string(), false), (HYDROGEN.to_string(), false)];
        assert_eq!(agy(&review), chain);
        let mechanical = route("Rename the widget", &u);
        // assert_eq!(mechanical.choice.model.as_deref(), Some(HYDROGEN));
        // assert_eq!(agy(&mechanical), [(HYDROGEN.to_string(), true)]);
        assert_eq!(mechanical.choice.model.as_deref(), Some(ARGON));
        assert_eq!(agy(&mechanical), chain);
        // Argon used up: Hydrogen stands in.
        // Since 2026-11-18: Neon stands in.
        u.0["agy"]["windows"][1]["used"] = json!(0.999);
        let review = route("Review the parser", &u);
        // assert_eq!(review.choice.model.as_deref(), Some(HYDROGEN));
        assert_eq!(review.choice.model.as_deref(), Some(NEON));
        assert!(agy(&review).contains(&(ARGON.to_string(), false)));
        assert!(agy(&review).contains(&(HYDROGEN.to_string(), false)));
        // The Gemini group's 5 h bucket is the tighter one and gates Hydrogen.
        // It gates Neon too; with Argon out as well, no agy model is eligible.
        u.0["agy"]["windows"][2]["used"] = json!(0.999);
        let mechanical = route("Rename the widget", &u);
        // let hydrogen = &mechanical.candidates.iter().find(|c| c.agent == "agy").unwrap();
        // assert_eq!(hydrogen.window.as_deref(), Some("gemini-5h"));
        // assert!(hydrogen.score.is_none());
        for model in [NEON, HYDROGEN] {
            let c = mechanical.candidates.iter().find(|c| c.model.as_deref() == Some(model)).unwrap();
            assert_eq!(c.window.as_deref(), Some("gemini-5h"));
            assert!(c.score.is_none());
        }
        assert_eq!(mechanical.choice.agent, "claude");
    }
}
