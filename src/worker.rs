use crate::{
    codex,
    result::{Meta, Worker as Report},
    status::Progress,
    Res,
};
use std::{fs, path::Path};

pub trait Worker {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        resume: bool,
        progress: &mut Progress,
    ) -> Res<(codex::Outcome, Report)>;
    fn finish(&mut self, _progress: &mut Progress) -> Res<Option<String>> {
        Ok(None)
    }
}

pub struct Codex;
impl Worker for Codex {
    fn turn(
        &mut self,
        dir: &Path,
        meta: &mut Meta,
        input: &str,
        resume: bool,
        progress: &mut Progress,
    ) -> Res<(codex::Outcome, Report)> {
        let outcome = codex::run(dir, meta, input, resume, progress)?;
        let raw = fs::read_to_string(dir.join("last.json")).unwrap_or_default();
        Ok((outcome, parse_report(&raw)))
    }
}

pub fn parse_report(raw: &str) -> Report {
    serde_json::from_str::<Report>(raw)
        .ok()
        .filter(|w| ["done", "partial", "blocked", "needs_decision"].contains(&w.status.as_str()))
        .unwrap_or_else(|| failed(crate::result::first(raw, 300)))
}
pub fn failed(notes: impl Into<String>) -> Report {
    Report {
        status: "failed".into(),
        notes: notes.into(),
        questions: Vec::new(),
    }
}
