use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premium_requests: Option<f64>,
}
#[derive(Serialize, Deserialize)]
pub struct Acceptance {
    pub command: String,
    pub passed: bool,
    pub tail: String,
}
#[derive(Serialize, Deserialize)]
pub struct Result {
    pub task: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub status: String,
    pub verified: bool,
    pub files_changed: Vec<String>,
    pub diffstat: String,
    pub out_of_scope: Vec<String>,
    pub acceptance: Vec<Acceptance>,
    pub notes: String,
    pub questions: Vec<String>,
    pub tokens: Tokens,
    pub seconds: u64,
    #[serde(default)]
    pub rounds: u32,
    pub worktree: String,
    pub branch: String,
}
#[derive(Serialize, Deserialize)]
pub struct Meta {
    pub task: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub mode: String,
    #[serde(default, rename = "type")]
    pub task_type: String,
    #[serde(default)]
    pub difficulty: u8,
    pub base: String,
    pub base_commit: String,
    pub base_branch: String,
    pub repo_root: String,
    pub worktree: String,
    pub branch: String,
    pub thread_id: Option<String>,
    pub timeout: u64,
    pub rounds: u32,
    #[serde(default = "default_auto_fix")]
    pub auto_fix: u32,
    #[serde(default)]
    pub closed: bool,
}
#[derive(Deserialize)]
pub struct Worker {
    pub status: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub questions: Vec<String>,
}
pub fn tail(s: &str, n: usize) -> String {
    s.chars()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
pub fn first(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn default_auto_fix() -> u32 {
    1
}
