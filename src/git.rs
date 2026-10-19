use crate::{Error, Res};
use std::{collections::BTreeSet, path::Path, process::Command};

pub fn run(cwd: &Path, args: &[&str]) -> Res<String> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(Error::new(
            1,
            format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
pub fn root() -> Res<std::path::PathBuf> {
    Ok(
        run(&std::env::current_dir()?, &["rev-parse", "--show-toplevel"])
            .map_err(|_| Error::usage("current directory must be inside a git repository"))?
            .trim()
            .into(),
    )
}
pub fn changed(cwd: &Path, base: &str) -> Res<Vec<String>> {
    let tracked = run(
        cwd,
        &["diff", "--name-only", "-z", "--no-renames", base, "--"],
    )?;
    let untracked = run(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(tracked
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
pub fn diffstat(cwd: &Path, base: &str, files: usize) -> Res<String> {
    let nums = run(cwd, &["diff", "--numstat", "--no-renames", base, "--"])?;
    let mut added = 0_u64;
    let mut deleted = 0_u64;
    for line in nums.lines() {
        let mut fields = line.split('\t');
        added += fields
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        deleted += fields
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
    }
    for path in run(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])?
        .split('\0')
        .filter(|p| !p.is_empty())
    {
        if let Ok(bytes) = std::fs::read(cwd.join(path)) {
            if !bytes.contains(&0) {
                added += bytes.split_inclusive(|b| *b == b'\n').count() as u64;
            }
        }
    }
    Ok(format!("+{added} -{deleted} in {files} files"))
}
pub fn commit(cwd: &Path, message: &str) -> Res<()> {
    run(cwd, &["add", "-A", "--", "."])?;
    let staged = run(cwd, &["diff", "--cached", "--name-only"])?;
    if !staged.is_empty() {
        run(
            cwd,
            &[
                "-c",
                "user.name=baton",
                "-c",
                "user.email=baton@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--author=baton <baton@localhost>",
                "-m",
                message,
            ],
        )?;
    }
    Ok(())
}
