use crate::{
    paths,
    result::{Meta, Result, Tokens},
    Error, Res,
};
use serde::Deserialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub agent: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub difficulty: Option<u8>,
    pub status: String,
    pub verified: bool,
    pub rounds: u32,
    pub tokens: Tokens,
    pub seconds: u64,
    pub timestamp: Option<u64>,
}

impl Entry {
    pub fn first_pass(&self) -> bool {
        self.verified && self.status == "done" && self.rounds == 0
    }
    pub fn total_tokens(&self) -> u64 {
        // Cached input is already included in input, not an additional token charge.
        self.tokens.input.saturating_add(self.tokens.output)
    }
}

pub fn read(home: &Path) -> Res<Vec<Entry>> {
    let text = match fs::read_to_string(home.join("ledger.jsonl")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line)
                .map_err(|e| Error::new(1, format!("ledger line {}: {e}", i + 1)))
        })
        .collect()
}

pub fn append(home: &Path, result: &Result, meta: &Meta) -> Res<()> {
    let mut row = serde_json::to_value(result)?;
    row["type"] = meta.task_type.clone().into();
    row["difficulty"] = meta.difficulty.into();
    row["rounds"] = meta.rounds.into();
    row["timestamp"] = paths::now().into();
    row["quota_before"] = serde_json::Value::Null;
    row["quota_after"] = serde_json::Value::Null;
    let mut bytes = serde_json::to_vec(&row)?;
    bytes.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("ledger.jsonl"))?
        .write_all(&bytes)?;
    Ok(())
}

pub fn median(mut values: Vec<u64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        values[mid - 1] as f64 / 2.0 + values[mid] as f64 / 2.0
    } else {
        values[mid] as f64
    })
}
