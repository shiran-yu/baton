//! Bounded CLI reads. All children have their own process group, including helper scripts.
use crate::{Error, Res};
use std::{
    io::{BufRead, BufReader},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

pub struct Running {
    pub child: Child,
    pub lines: Receiver<String>,
    cleaned: bool,
}
impl Running {
    pub fn spawn(command: &mut Command, pipe_input: bool) -> Res<Self> {
        let mut child = command
            .stdin(if pipe_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::new(1, "missing stdout"))?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        Ok(Self {
            child,
            lines,
            cleaned: false,
        })
    }
    fn kill(&mut self) {
        if self.cleaned {
            return;
        }
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", self.child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.cleaned = true;
    }
    pub fn terminate(&mut self) {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", "--", &format!("-{}", self.child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.kill();
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn capture(command: &mut Command, timeout: Duration) -> Res<String> {
    let mut process = Running::spawn(command, false)?;
    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    loop {
        if Instant::now() >= deadline {
            process.terminate();
            return Err(Error::new(5, "command timed out"));
        }
        match process
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                process.terminate();
                return Err(Error::new(5, "command timed out"));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    loop {
        if let Some(status) = process.child.try_wait()? {
            return if status.success() {
                Ok(output)
            } else {
                Err(Error::new(1, "command failed"))
            };
        }
        if Instant::now() >= deadline {
            process.terminate();
            return Err(Error::new(5, "command timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn executable(agent: &str) -> std::ffi::OsString {
    std::env::var_os(format!("BATON_{}", agent.to_uppercase())).unwrap_or_else(|| agent.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_kills_a_process_ignoring_term() {
        let start = Instant::now();
        let result = capture(
            Command::new("/bin/sh").args(["-c", "trap '' TERM; while :; do /bin/sleep 1; done"]),
            Duration::from_millis(50),
        );
        assert_eq!(result.unwrap_err().code, 5);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn closing_stdout_does_not_bypass_timeout() {
        let result = capture(
            Command::new("/bin/sh").args(["-c", "exec 1>&-; /bin/sleep 10"]),
            Duration::from_millis(50),
        );
        assert_eq!(result.unwrap_err().code, 5);
    }
}
