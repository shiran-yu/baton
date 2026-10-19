use crate::{Error, Res};
use std::collections::BTreeMap;

pub struct Brief {
    pub text: String,
    pub goal: String,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    pub title: Option<String>,
}

pub fn lint(text: &str) -> Res<()> {
    let lower = text.to_lowercase();
    for risk in [
        "keyfob",
        "t212",
        "trading212",
        "binance-cli",
        "kraken order",
        "/equity/orders",
        "api_key",
        "secret",
    ] {
        if lower.contains(risk) {
            return Err(Error::usage(format!(
                "risk lint: brief mentions '{risk}'; keep this task with the lead"
            )));
        }
    }
    Ok(())
}

impl Brief {
    pub fn parse(text: String) -> Res<Self> {
        lint(&text)?;
        Self::parse_for_route(text)
    }

    // Routing must inspect risky briefs so it can hand them back to the lead.
    pub fn parse_for_route(text: String) -> Res<Self> {
        let mut sections: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        let mut heading = String::new();
        let mut fence: Option<(char, usize)> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if fence.is_none() && line.starts_with("## ") {
                heading = line[3..].trim().to_string();
                if sections.contains_key(&heading) {
                    return Err(Error::usage(format!("duplicate heading: {heading}")));
                }
                sections.insert(heading.clone(), Vec::new());
                continue;
            }
            if let Some((c, n)) = fence {
                if trimmed.chars().take_while(|x| *x == c).count() >= n {
                    fence = None;
                }
            } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                let c = trimmed.chars().next().unwrap_or('`');
                fence = Some((c, trimmed.chars().take_while(|x| *x == c).count()));
            }
            if let Some(body) = sections.get_mut(&heading) {
                body.push(line);
            }
        }
        for required in ["Goal", "Files", "Forbidden", "Acceptance"] {
            if !sections.contains_key(required) {
                return Err(Error::usage(format!("brief is missing ## {required}")));
            }
        }
        let goal = sections["Goal"]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if goal.is_empty() {
            return Err(Error::usage("## Goal must not be empty"));
        }
        let files = sections["Files"]
            .iter()
            .map(|l| {
                l.trim()
                    .trim_start_matches("- ")
                    .trim_start_matches("* ")
                    .trim_matches('`')
                    .to_string()
            })
            .filter(|l| !l.is_empty() && l != "none")
            .collect();
        let mut commands = Vec::new();
        let mut open = None;
        for line in &sections["Acceptance"] {
            let line = line.trim();
            if let Some((c, n)) = open {
                if line.chars().take_while(|x| *x == c).count() >= n {
                    break;
                }
                if !line.is_empty() && !line.starts_with('#') {
                    commands.push(line.to_string());
                }
            } else if line.starts_with("```") || line.starts_with("~~~") {
                let c = line.chars().next().unwrap_or('`');
                open = Some((c, line.chars().take_while(|x| *x == c).count()));
            }
        }
        let title = sections.get("Title").map(|lines| lines.join(" "));
        Ok(Self {
            text,
            goal,
            files,
            commands,
            title,
        })
    }

    pub fn subject(&self, task: &str) -> String {
        let title = self.title.as_deref().filter(|s| !s.trim().is_empty());
        let plain = strip_markdown(title.unwrap_or(&self.goal));
        let text = if title.is_some() {
            plain.as_str()
        } else {
            let end = plain.char_indices().find_map(|(i, c)| {
                (matches!(c, '.' | '!' | '?')
                    && plain[i + c.len_utf8()..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace))
                .then_some(i + c.len_utf8())
            });
            &plain[..end.unwrap_or(plain.len())]
        };
        let subject = format!("baton {task}: {text}");
        if subject.chars().count() > 72 {
            format!(
                "{}…",
                subject.chars().take(71).collect::<String>().trim_end()
            )
        } else {
            subject
        }
    }
}

fn strip_markdown(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '*' | '~' | '#' | '>' | '[' => {}
            '!' if chars.peek() == Some(&'[') => {}
            ']' if chars.peek() == Some(&'(') => {
                chars.next();
                let mut depth = 1;
                for c in chars.by_ref() {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
            }
            '_' if plain.ends_with(char::is_alphanumeric)
                && chars.peek().is_some_and(|c| c.is_alphanumeric()) =>
            {
                plain.push(c)
            }
            '_' | ']' => {}
            '\\' => {
                if let Some(c) = chars.next() {
                    plain.push(c);
                }
            }
            _ => plain.push(c),
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

// '*' and '?' never cross a directory; '**/' also matches zero directories.
pub fn glob(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern
        .strip_prefix("./")
        .unwrap_or(pattern)
        .chars()
        .collect();
    let s: Vec<char> = path.chars().collect();
    let mut previous = vec![false; s.len() + 1];
    previous[0] = true;
    let mut i = 0;
    while i < p.len() {
        let double = p[i] == '*' && p.get(i + 1) == Some(&'*');
        let directories = double && p.get(i + 2) == Some(&'/');
        let mut row = vec![false; s.len() + 1];
        if directories {
            let mut prefix = false;
            for j in 0..=s.len() {
                row[j] = previous[j] || (j > 0 && s[j - 1] == '/' && prefix);
                prefix |= previous[j];
            }
        } else if p[i] == '*' {
            row[0] = previous[0];
            for j in 1..=s.len() {
                row[j] = previous[j] || (row[j - 1] && (double || s[j - 1] != '/'));
            }
        } else {
            for j in 1..=s.len() {
                row[j] = previous[j - 1] && (p[i] == s[j - 1] || (p[i] == '?' && s[j - 1] != '/'));
            }
        }
        i += if directories {
            3
        } else if double {
            2
        } else {
            1
        };
        previous = row;
    }
    previous[s.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globs() {
        for path in ["src/main.rs", "src/a/b.rs"] {
            assert!(glob("src/**", path));
            assert!(glob("**/*.rs", path));
        }
        assert!(glob("**/*.rs", "main.rs"));
        assert!(glob("a?.rs", "ab.rs"));
        assert!(!glob("src/*", "src/a/b.rs"));
        assert!(!glob("a?.rs", "a/.rs"));
        assert!(!glob("**/foo", "xxfoo"));
        assert!(glob("**/foo", "a/b/foo"));
        assert!(glob("?.rs", "é.rs"));
    }
    #[test]
    fn headings_and_first_fence() {
        let brief = Brief::parse("## Goal\nDo it\n## Files\n- `src/**`\n## Forbidden\nNothing\n## Acceptance\n```sh\n# comment\n\ntrue\n```\n```\nfalse\n```".into()).expect("valid brief");
        assert_eq!(brief.commands, ["true"]);
        assert_eq!(brief.files, ["src/**"]);
        assert!(Brief::parse("## Goal\nOops".into()).is_err());
        assert!(lint("KEYFOB").is_err());
    }
}

#[cfg(test)]
mod subject_tests {
    use super::*;
    fn brief(goal: &str, title: &str) -> Brief {
        Brief::parse(format!("## Title\n{title}\n## Goal\n{goal}\n## Files\nnone\n## Forbidden\nNone\n## Acceptance\n")).unwrap()
    }
    #[test]
    fn title_or_first_sentence_stripped_and_truncated() {
        assert_eq!(
            brief("**Add** a [`widget`](src/widget.rs).\nTest it too.", "").subject("id"),
            "baton id: Add a widget."
        );
        assert_eq!(
            brief("Other goal.", "__Improve__ `the_widget` [docs](docs/a.md)").subject("id"),
            "baton id: Improve the_widget docs"
        );
        let subject = brief("Other goal.", &"é".repeat(100)).subject("id");
        assert_eq!(subject.chars().count(), 72);
        assert!(subject.ends_with('…'));
    }
}
