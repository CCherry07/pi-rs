use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, invalid, optional_positive_usize, require_str, resolve_to_cwd, spec, truncate_head,
};
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
pub struct GrepPlugin;
pub struct GrepTool;
#[async_trait]
impl AgentPlugin for GrepPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("grep-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        c.register_tool(Arc::new(GrepTool))
    }
}
#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "grep",
                "Search text files for a pattern. Respects .gitignore and supports glob filtering.",
                json!({
                    "type":"object","properties":{
                        "pattern":{"type":"string"},"path":{"type":"string","description":"Directory or file to search (default: current directory)"},"glob":{"type":"string"},
                        "ignoreCase":{"type":"boolean"},"literal":{"type":"boolean"},
                        "context":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1},
                        "hashline":{"type":"boolean"}
                    },"required":["pattern"],"additionalProperties":false
                }),
            ),
            "Search file contents for patterns (respects .gitignore)",
            std::iter::empty::<&str>(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let pattern = require_str(&input, "pattern")?.to_string();
        let path = resolve_to_cwd(
            &context.cwd,
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let glob = input
            .get("glob")
            .and_then(Value::as_str)
            .map(build_glob)
            .transpose()?;
        let ignore_case = input
            .get("ignoreCase")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let literal = input
            .get("literal")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let context_lines = input
            .get("context")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
            .min(20);
        let limit = optional_positive_usize(&input, "limit", 100)?.min(10_000);
        let hashline = input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let regex = build_regex(&pattern, literal, ignore_case)?;
        let search_root = path.clone();
        let search_is_directory = path.is_dir();
        let signal = context.abort_signal.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut output = Vec::new();
            for entry in WalkBuilder::new(&path)
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
            {
                if signal.is_aborted() {
                    return Err(ToolError::Aborted);
                }
                let entry = match entry {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                let relative = if search_is_directory {
                    entry
                        .path()
                        .strip_prefix(&search_root)
                        .unwrap_or(entry.path())
                } else {
                    entry
                        .path()
                        .file_name()
                        .map(Path::new)
                        .unwrap_or(entry.path())
                };
                if glob
                    .as_ref()
                    .is_some_and(|matcher| !matcher.is_match(relative))
                {
                    continue;
                }
                let bytes = match std::fs::read(entry.path()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if bytes.contains(&0) || bytes.len() > 10 * 1024 * 1024 {
                    continue;
                }
                let text = match String::from_utf8(bytes) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let lines = text.lines().collect::<Vec<_>>();
                for (index, line) in lines.iter().enumerate() {
                    if regex.is_match(line) {
                        let from = index.saturating_sub(context_lines);
                        let to = (index + context_lines + 1).min(lines.len());
                        for (line_index, context_line) in
                            lines.iter().enumerate().take(to).skip(from)
                        {
                            let marker = if line_index == index { ":" } else { "-" };
                            let body = if hashline {
                                format!(
                                    "{}#{}:{}",
                                    line_index + 1,
                                    hash(line_index, context_line),
                                    context_line
                                )
                            } else {
                                (*context_line).to_string()
                            };
                            output.push(format!(
                                "{}{}{}:{}",
                                relative.display(),
                                marker,
                                line_index + 1,
                                body
                            ));
                        }
                        if output.len() >= limit {
                            return Ok((output, true));
                        }
                    }
                }
            }
            Ok((output, false))
        })
        .await
        .map_err(|e| execution(e.to_string()))??;
        let (text, byte_truncated) = truncate_head(result.0, limit);
        let mut tool_result = ToolResult::text(if text.is_empty() {
            "No matches found".to_string()
        } else {
            text
        });
        tool_result.details = Some(json!({"truncated":result.1 || byte_truncated}));
        Ok(tool_result)
    }
}

fn build_regex(pattern: &str, literal: bool, ignore_case: bool) -> Result<Regex, ToolError> {
    let escaped;
    let pattern = if literal {
        escaped = regex::escape(pattern);
        escaped.as_str()
    } else {
        pattern
    };
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| invalid(format!("invalid pattern: {e}")))
}

fn build_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|e| invalid(format!("invalid glob: {e}")))
}

fn hash(index: usize, line: &str) -> String {
    let mut value = index as u32;
    for byte in line.bytes() {
        value = value.wrapping_mul(16777619) ^ u32::from(byte);
    }
    let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    format!(
        "{}{}",
        chars[((value / 26) % 26) as usize] as char,
        chars[(value % 26) as usize] as char
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[tokio::test]
    async fn searches_text_and_skips_ignored_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "hello hidden\n").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = GrepTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"pattern":"hello","glob":"*.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.contains("visible.txt"));
        assert!(!text.contains("ignored.txt"));
    }
}
