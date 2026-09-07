use serde_json::{json, Value};

const DEFAULT_COMMIT_MESSAGE_PROMPT: &str =
    "Generate a concise git commit message for the following changes. \
Follow conventional commit format (e.g., feat:, fix:, refactor:, docs:, etc.). \
Keep the summary line under 72 characters. \
Only output the commit message, nothing else.\n\n\
Changes:\n{diff}";

pub(crate) fn build_commit_message_prompt(diff: &str, template: &str) -> Result<String, String> {
    if diff.trim().is_empty() {
        return Err("No changes to generate commit message for".to_string());
    }
    let template = if template.trim().is_empty() {
        DEFAULT_COMMIT_MESSAGE_PROMPT
    } else {
        template
    };
    Ok(if template.contains("{diff}") {
        template.replace("{diff}", diff)
    } else {
        format!("{template}\n\nChanges:\n{diff}")
    })
}

pub(crate) fn build_run_metadata_prompt(task: &str) -> String {
    format!(
        "You create concise run metadata for a coding task.\n\
Return ONLY a JSON object with keys:\n\
- title: short, clear, 3-7 words, Title Case\n\
- worktreeName: lower-case, kebab-case slug prefixed with one of: \
feat/, fix/, chore/, test/, docs/, refactor/, perf/, build/, ci/, style/.\n\n\
Choose fix/ for bugs, errors, regressions, crashes, or cleanup. Use the closest prefix for other tasks.\n\n\
Task:\n{task}"
    )
}

pub(crate) fn parse_run_metadata(raw: &str) -> Result<Value, String> {
    let start = raw
        .find('{')
        .ok_or_else(|| "Failed to parse metadata JSON".to_string())?;
    let end = raw
        .rfind('}')
        .filter(|end| *end > start)
        .ok_or_else(|| "Failed to parse metadata JSON".to_string())?;
    let value: Value = serde_json::from_str(&raw[start..=end])
        .map_err(|_| "Failed to parse metadata JSON".to_string())?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing title in metadata".to_string())?;
    let worktree_name = value
        .get("worktreeName")
        .or_else(|| value.get("worktree_name"))
        .and_then(Value::as_str)
        .map(sanitize_worktree_name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing worktree name in metadata".to_string())?;
    Ok(json!({ "title": title, "worktreeName": worktree_name }))
}

fn sanitize_worktree_name(value: &str) -> String {
    let mut cleaned = String::new();
    let mut last_dash = false;
    for ch in value.trim().to_lowercase().chars() {
        let next = if ch.is_ascii_alphanumeric() || ch == '/' {
            last_dash = false;
            Some(ch)
        } else if ch == '-' || ch.is_whitespace() || ch == '_' {
            if last_dash {
                None
            } else {
                last_dash = true;
                Some('-')
            }
        } else {
            None
        };
        if let Some(ch) = next {
            cleaned.push(ch);
        }
    }
    while cleaned.ends_with('-') || cleaned.ends_with('/') {
        cleaned.pop();
    }
    const PREFIXES: [&str; 10] = [
        "feat/",
        "fix/",
        "chore/",
        "test/",
        "docs/",
        "refactor/",
        "perf/",
        "build/",
        "ci/",
        "style/",
    ];
    if PREFIXES.iter().any(|prefix| cleaned.starts_with(prefix)) {
        return cleaned;
    }
    for prefix in PREFIXES {
        let dashed = prefix.replace('/', "-");
        if cleaned.starts_with(&dashed) {
            return cleaned.replacen(&dashed, prefix, 1);
        }
    }
    format!("feat/{}", cleaned.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_prompt_requires_a_diff() {
        assert_eq!(
            build_commit_message_prompt("", "").unwrap_err(),
            "No changes to generate commit message for"
        );
    }

    #[test]
    fn parses_and_sanitizes_run_metadata() {
        assert_eq!(
            parse_run_metadata(
                "```json\n{\"title\":\"Fix Login\",\"worktreeName\":\"fix-login_loop\"}\n```"
            )
            .unwrap(),
            json!({ "title": "Fix Login", "worktreeName": "fix/login-loop" })
        );
    }
}
