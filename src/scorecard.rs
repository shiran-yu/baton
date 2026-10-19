use crate::ledger::{median, Entry};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
pub struct Row {
    pub agent: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub runs: usize,
    pub first_pass_verified_rate: f64,
    pub mean_rounds: f64,
    pub median_tokens: f64,
    pub median_seconds: f64,
}
#[derive(Serialize)]
pub struct Scorecard {
    pub rows: Vec<Row>,
    pub total_tokens: BTreeMap<String, u64>,
}

pub fn aggregate(entries: &[Entry], since: Option<u64>, now: u64) -> Scorecard {
    let cutoff = since.map(|days| now.saturating_sub(days.saturating_mul(86400)));
    let mut groups: BTreeMap<(&str, &str), Vec<&Entry>> = BTreeMap::new();
    let mut total_tokens: BTreeMap<String, u64> = BTreeMap::new();
    for e in entries {
        // Old ledgers have no timestamp: include them only for an all-time scorecard.
        if cutoff.is_some_and(|cutoff| e.timestamp.is_none_or(|t| t < cutoff || t > now)) {
            continue;
        }
        groups.entry((&e.agent, &e.task_type)).or_default().push(e);
        let total = total_tokens.entry(e.agent.clone()).or_default();
        *total = total.saturating_add(e.total_tokens());
    }
    let rows = groups
        .into_iter()
        .map(|((agent, task_type), entries)| {
            let runs = entries.len();
            Row {
                agent: agent.into(),
                task_type: if task_type.is_empty() {
                    "unknown"
                } else {
                    task_type
                }
                .into(),
                runs,
                first_pass_verified_rate: entries.iter().filter(|e| e.first_pass()).count() as f64
                    / runs as f64,
                mean_rounds: entries.iter().map(|e| e.rounds as f64).sum::<f64>() / runs as f64,
                median_tokens: median(entries.iter().map(|e| e.total_tokens()).collect())
                    .unwrap_or(0.0),
                median_seconds: median(entries.iter().map(|e| e.seconds).collect()).unwrap_or(0.0),
            }
        })
        .collect();
    Scorecard { rows, total_tokens }
}

pub fn print(card: &Scorecard) {
    println!("agent  type  runs  first-pass  mean rounds  median tokens  median seconds");
    for r in &card.rows {
        println!(
            "{}  {}  {}  {:.1}%  {:.2}  {:.0}  {:.1}",
            r.agent,
            r.task_type,
            r.runs,
            r.first_pass_verified_rate * 100.0,
            r.mean_rounds,
            r.median_tokens,
            r.median_seconds
        );
    }
    for (agent, total) in &card.total_tokens {
        println!("{agent}: {total} total tokens");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_aggregation_and_period_filter() {
        let entries = include_str!("../tests/fixtures/ledger/scorecard.jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect::<Vec<Entry>>();
        let card = aggregate(&entries, None, 280_000);
        let row = card
            .rows
            .iter()
            .find(|r| r.agent == "codex" && r.task_type == "implement")
            .unwrap();
        assert_eq!(row.runs, 3);
        assert_eq!(row.first_pass_verified_rate, 1.0 / 3.0);
        assert_eq!(row.mean_rounds, 1.0 / 3.0);
        assert_eq!(row.median_tokens, 360.0);
        assert_eq!(row.median_seconds, 30.0);
        assert_eq!(card.total_tokens["codex"], 1490);
        assert_eq!(card.total_tokens["agy"], 60);
        let period = aggregate(&entries, Some(1), 280_000);
        assert_eq!(period.rows.len(), 2);
        assert_eq!(period.total_tokens["codex"], 1480);
        assert_eq!(period.total_tokens["agy"], 50);
        assert_eq!(aggregate(&entries, Some(0), 280_000).rows.len(), 0);
        assert_eq!(aggregate(&entries, Some(1), 199_999).rows.len(), 0);
    }
}
