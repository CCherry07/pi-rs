use pi_core::ToolError;
use serde_json::Value;

pub(super) const MAX_RENDERED_TEXT_BYTES: usize = 2_000;

const DEFAULT_QUERY_LIMIT: usize = 10;
const MAX_QUERY_LIMIT: usize = 50;
const MAX_TOOL_OUTPUT_BYTES: usize = 32 * 1024;

pub(super) fn query_limit(input: &Value) -> Result<usize, ToolError> {
    let Some(value) = input.get("limit") else {
        return Ok(DEFAULT_QUERY_LIMIT);
    };
    let value = value.as_u64().ok_or_else(|| {
        ToolError::InvalidArguments("limit must be a positive integer".to_string())
    })?;
    usize::try_from(value)
        .ok()
        .filter(|value| (1..=MAX_QUERY_LIMIT).contains(value))
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!("limit must be between 1 and {MAX_QUERY_LIMIT}"))
        })
}

pub(super) fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{key} must be a non-empty string")))
}

pub(super) fn bounded_lines(lines: &[String]) -> String {
    let mut output = String::new();
    let mut truncated = false;
    for line in lines {
        let separator_bytes = usize::from(!output.is_empty());
        if output
            .len()
            .saturating_add(separator_bytes)
            .saturating_add(line.len())
            > MAX_TOOL_OUTPUT_BYTES
        {
            truncated = true;
            break;
        }
        if separator_bytes == 1 {
            output.push('\n');
        }
        output.push_str(line);
    }
    if truncated {
        output.push_str("\n[More matches omitted; narrow the query or lower the limit.]");
    }
    output
}

pub(super) fn execution(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}
