use crate::{
    result::{Meta, Result},
    Res,
};
use std::{fs::OpenOptions, io::Write, path::Path};

pub fn append(home: &Path, result: &Result, meta: &Meta) -> Res<()> {
    let mut row = serde_json::to_value(result)?;
    row["type"] = "implement".into();
    row["difficulty"] = serde_json::Value::Null;
    row["rounds"] = meta.rounds.into();
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
