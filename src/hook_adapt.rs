use crate::{Error, Res};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub struct Options {
    pub host: String,
    pub event: String,
    pub command: Vec<String>,
}

pub fn tools(name: &str) -> &'static [&'static str] {
    match name {
        "Read" => &["view_file"],
        "Write" => &["write_to_file"],
        "Edit" => &["replace_file_content", "multi_replace_file_content"],
        "Grep" => &["grep_search"],
        "Glob" => &["find_by_name"],
        "LS" => &["list_dir"],
        "Bash" => &["run_command"],
        "WebFetch" => &["read_url_content"],
        "WebSearch" => &["search_web"],
        "Task" => &["invoke_subagent"],
        _ => &[],
    }
}

fn claude_tool(name: &str) -> &str {
    match name {
        "run_command" => "Bash",
        "write_to_file" => "Write",
        "replace_file_content" | "multi_replace_file_content" => "Edit",
        "view_file" => "Read",
        "grep_search" => "Grep",
        "find_by_name" => "Glob",
        "list_dir" => "LS",
        "read_url_content" => "WebFetch",
        "search_web" => "WebSearch",
        "invoke_subagent" => "Task",
        _ => name,
    }
}

fn agy_input(input: &Value, event: &str) -> Value {
    let mut result = json!({
        "session_id": input["conversationId"],
        "cwd": input["workspacePaths"][0],
        "transcript_path": input["transcriptPath"],
        "hook_event_name": event,
    });
    if matches!(event, "PreToolUse" | "PostToolUse") {
        result["tool_name"] = json!(claude_tool(
            input["toolCall"]["name"].as_str().unwrap_or("")
        ));
        let mut args = input["toolCall"]["args"].clone();
        if let Some(args) = args.as_object_mut() {
            let command = ["CommandLine", "command", "Command", "cmd"]
                .iter()
                .find_map(|key| args.get(*key).and_then(Value::as_str))
                .map(String::from);
            if let Some(command) = command {
                args.insert("command".into(), json!(command));
            }
        }
        result["tool_input"] = args;
    }
    if event == "UserPromptSubmit" {
        result["prompt"] = json!("");
    }
    result
}

// Hook commands often contain a shell and an absolute script path. Walk only existing
// ancestors; never infer a plugin root from an arbitrary working directory.
fn plugin_root(command: &[String]) -> Option<PathBuf> {
    for arg in command {
        // Try argv paths directly, then shell words for commands passed to sh -c.
        for word in std::iter::once(arg.clone()).chain(shell_words(arg)) {
            let path = Path::new(&word);
            if !path.is_absolute() {
                continue;
            }
            for parent in path.ancestors() {
                if parent.join(".claude-plugin/plugin.json").is_file()
                    || parent.join(".codex-plugin/plugin.json").is_file()
                    || parent.join("plugin.json").is_file()
                {
                    return Some(parent.to_path_buf());
                }
            }
        }
    }
    None
}

fn shell_words(text: &str) -> Vec<String> {
    let (mut quote, mut escaped, mut word) = (None, false, String::new());
    let mut words = Vec::new();
    for c in text.chars() {
        if escaped {
            word.push(c);
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote.is_none() && (c.is_whitespace() || ";()|&".contains(c)) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn execute(command: &[String], input: &Value) -> Res<Output> {
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = plugin_root(command) {
        cmd.env("CLAUDE_PLUGIN_ROOT", root);
    }
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let bytes = serde_json::to_vec(input)?;
    // Drain stdout/stderr while writing, so a guard with substantial output cannot
    // deadlock the adapter. A closed stdin is normal for a guard that exits early.
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
        let output = child.wait_with_output();
        let _ = writer.join();
        output.map_err(Error::from)
    })
}

fn deny(host: &str, reason: &str) -> Value {
    if host == "agy" {
        json!({"decision":"deny", "reason":reason})
    } else {
        json!({"hookSpecificOutput":{"hookEventName":"PreToolUse", "permissionDecision":"deny", "permissionDecisionReason":reason}})
    }
}

fn reason(output: &Value) -> &str {
    output["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .or_else(|| output["reason"].as_str())
        .unwrap_or("guard requested permission; this host cannot safely prompt")
}

pub fn run(options: &Options) -> Res<i32> {
    let pre = options.event == "PreToolUse";
    let result = adapt(options);
    match result {
        Ok((output, code)) => {
            if let Some(output) = output {
                println!("{output}");
            }
            Ok(code)
        }
        Err(error) if pre => {
            println!("{}", deny(&options.host, &format!("hook failed: {error}")));
            Ok(0)
        }
        Err(error) => Err(error),
    }
}

fn adapt(options: &Options) -> Res<(Option<Value>, i32)> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    let input: Value = serde_json::from_str(&text)?;
    if !input.is_object() {
        return Err(Error::new(1, "hook input must be an object"));
    }
    if options.host == "agy"
        && options.event == "UserPromptSubmit"
        && input["invocationNum"].as_u64() != Some(0)
    {
        return Ok((Some(json!({})), 0));
    }
    let input = if options.host == "agy" {
        agy_input(&input, &options.event)
    } else {
        input
    };
    let result = execute(&options.command, &input)?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if options.host == "codex" && result.status.code() == Some(2) {
        eprint!("{stderr}");
        return Ok((None, 2));
    }
    if !result.status.success() {
        return Err(Error::new(
            result.status.code().unwrap_or(1),
            if stderr.trim().is_empty() {
                format!("guard exited with {}", result.status)
            } else {
                stderr.trim().into()
            },
        ));
    }
    let parsed = serde_json::from_slice::<Value>(&result.stdout);
    if options.event == "PreToolUse" {
        let mut output = parsed?;
        if !output.is_object() {
            return Err(Error::new(1, "guard output must be a JSON object"));
        }
        let permission = &output["hookSpecificOutput"]["permissionDecision"];
        let decision = permission.as_str().unwrap_or("");
        if !permission.is_null() && !permission.is_string() {
            return Err(Error::new(1, "guard permission decision must be a string"));
        }
        let blocked = decision == "ask" || decision == "deny" || output["decision"] == "block";
        if blocked && options.host == "agy" {
            return Ok((Some(deny("agy", reason(&output))), 0));
        }
        if decision == "ask" && options.host == "codex" {
            output["hookSpecificOutput"]["permissionDecision"] = json!("deny");
            if output["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                output["hookSpecificOutput"]["permissionDecisionReason"] =
                    json!("guard requested permission; Codex cannot safely prompt");
            }
            return Ok((Some(output), 0));
        }
        if !["", "allow", "deny"].contains(&decision) {
            return Err(Error::new(1, "unknown guard permission decision"));
        }
        if options.host == "agy" {
            output = json!({"decision":"allow"});
        }
        return Ok((Some(output), 0));
    }
    if options.host == "codex" {
        // Non-tool hooks may legitimately print plain context instead of JSON.
        print!("{stdout}");
        return Ok((None, 0));
    }
    if options.event == "PostToolUse" {
        return Ok((Some(json!({})), 0));
    }
    if let Ok(output) = &parsed {
        if options.event == "Stop" && output["decision"] == "block" {
            return Ok((
                Some(
                    json!({"decision":"continue", "reason":output["reason"].as_str().unwrap_or("")}),
                ),
                0,
            ));
        }
    }
    let context = match &parsed {
        Ok(output) if output.is_object() => output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .or_else(|| output["additionalContext"].as_str())
            .unwrap_or(""),
        _ => stdout.trim_end(),
    };
    let output = if context.is_empty() {
        json!({})
    } else {
        json!({"injectSteps":[{"ephemeralMessage":context}]})
    };
    Ok((Some(output), 0))
}
