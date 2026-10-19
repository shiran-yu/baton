use crate::{paths, result::Tokens, Res};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
pub struct Status {
    pub task: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub phase: String,
    pub commands: usize,
    pub last: String,
    pub files: usize,
    pub tests: String,
    pub tokens: Tokens,
    pub quota: String,
    pub started: u64,
    pub updated: u64,
}
pub struct Progress {
    pub status: Status,
    dir: PathBuf,
    last_write: Instant,
    start: Instant,
    json: bool,
}
impl Progress {
    pub fn new(dir: &Path, meta: &crate::result::Meta, json: bool) -> Res<Self> {
        let now = paths::now();
        let status = Status {
            task: meta.task.clone(),
            agent: meta.agent.clone(),
            model: meta.model.clone(),
            effort: meta.effort.clone(),
            phase: "starting".into(),
            commands: 0,
            last: String::new(),
            files: 0,
            tests: String::new(),
            tokens: Tokens::default(),
            quota: String::new(),
            started: now,
            updated: now,
        };
        paths::write_json(&dir.join("status.json"), &status)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("log.txt"))?;
        Ok(Self {
            status,
            dir: dir.into(),
            last_write: Instant::now(),
            start: Instant::now(),
            json,
        })
    }
    pub fn update(&mut self) -> Res<()> {
        if self.last_write.elapsed() >= Duration::from_millis(500) {
            self.status.updated = paths::now();
            paths::write_json(&self.dir.join("status.json"), &self.status)?;
            self.last_write = Instant::now();
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Res<()> {
        // Preserve the write rate even on very short runs and phase transitions.
        let remaining = Duration::from_millis(500).saturating_sub(self.last_write.elapsed());
        std::thread::sleep(remaining);
        self.update()
    }
    pub fn log(&self, text: &str) -> Res<()> {
        let secs = self.start.elapsed().as_secs();
        let line = format!(
            "[{} {:02}:{:02}] {text}",
            self.status.agent,
            secs / 60,
            secs % 60
        );
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join("log.txt"))?,
            "{line}"
        )?;
        if !self.json {
            println!("{line}");
        }
        Ok(())
    }
}

impl Status {
    pub fn line(&self) -> String {
        let secs = paths::now().saturating_sub(self.started);
        let tokens = self.tokens.input + self.tokens.output;
        let tokens = if tokens >= 1000 {
            format!("{}k", tokens / 1000)
        } else {
            tokens.to_string()
        };
        format!(
            "{} · {} · {} files · {} cmds · {} tok · {}m{:02}s",
            self.agent,
            self.effort,
            self.files,
            self.commands,
            tokens,
            secs / 60,
            secs % 60
        )
    }
}
