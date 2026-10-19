use crate::{
    brief::{glob, Brief},
    codex, git,
    result::{Acceptance, Meta},
    status::Progress,
    Res,
};
use std::path::Path;

pub struct Verification {
    pub verified: bool,
    pub files: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub acceptance: Vec<Acceptance>,
    pub diffstat: String,
}
pub fn run(
    dir: &Path,
    meta: &Meta,
    brief: &Brief,
    progress: &mut Progress,
    skip_commands: bool,
) -> Res<Verification> {
    let cwd = Path::new(&meta.worktree);
    progress.status.phase = "verifying".into();
    let worker_files = git::changed(cwd, &meta.base_commit)?;
    progress.status.files = worker_files.len();
    progress.update()?;
    let mut acceptance = Vec::new();
    if !skip_commands {
        for command in &brief.commands {
            progress.log(&format!("verifying: {command}"))?;
            let (passed, tail) = codex::shell(
                cwd,
                command,
                meta.timeout,
                &dir.join("acceptance-output.tmp"),
                progress,
            )?;
            acceptance.push(Acceptance {
                command: command.clone(),
                passed,
                tail,
            });
            progress.status.tests = format!(
                "{}/{}",
                acceptance.iter().filter(|a| a.passed).count(),
                brief.commands.len()
            );
            progress.update()?;
        }
    }
    // Inspect after acceptance as well: commands may create or modify files.
    let files = git::changed(cwd, &meta.base_commit)?;
    let out_of_scope: Vec<String> = files
        .iter()
        .chain(&worker_files)
        .filter(|p| !brief.files.iter().any(|g| glob(g, p)))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let verified = !skip_commands
        && (meta.mode == "read" || (!worker_files.is_empty() && !files.is_empty()))
        && out_of_scope.is_empty()
        && acceptance.iter().all(|a| a.passed);
    let diffstat = git::diffstat(cwd, &meta.base_commit, files.len())?;
    progress.status.files = files.len();
    Ok(Verification {
        verified,
        files,
        out_of_scope,
        acceptance,
        diffstat,
    })
}
