use std::time::Duration;

use serde_json::Value;

pub(crate) const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Source {
    pub code: String,
    pub timeout: Duration,
    pub output_bytes: usize,
}

impl Source {
    pub fn parse(code: &str) -> Result<Self, String> {
        if code.len() > MAX_SOURCE_BYTES {
            return Err("JavaScript source exceeds the 1 MiB limit".into());
        }
        if code.trim().is_empty() {
            return Err("Expected non-empty JavaScript source".into());
        }
        const PREFIX: &str = "// @options:";
        for (index, line) in code.lines().enumerate().skip(1) {
            if line.trim_start().starts_with(PREFIX) {
                return Err(format!(
                    "The @options line must be first; found on line {}",
                    index + 1
                ));
            }
        }
        let mut parsed = Self {
            code: code.into(),
            // Deliberately bounded by default, including time spent in tools.
            timeout: Duration::from_secs(60),
            output_bytes: 10_000 * 4,
        };
        let first = code.lines().next().unwrap_or_default().trim_start();
        let Some(options) = first.strip_prefix(PREFIX) else {
            return Ok(parsed);
        };
        let options: Value = serde_json::from_str(options.trim())
            .map_err(|error| format!("@options must be a JSON object: {error}"))?;
        let fields = options
            .as_object()
            .ok_or("@options must be a JSON object")?;
        for (key, value) in fields {
            match key.as_str() {
                "timeout_ms" => {
                    let millis = value
                        .as_u64()
                        .filter(|v| *v > 0 && *v <= 2_147_483_647)
                        .ok_or("timeout_ms must be a positive integer up to 2147483647")?;
                    parsed.timeout = Duration::from_millis(millis);
                }
                "max_output_tokens" => {
                    let count = value
                        .as_u64()
                        .filter(|v| *v <= 9_007_199_254_740_991)
                        .ok_or("max_output_tokens must be a non-negative safe integer")?;
                    parsed.output_bytes =
                        count.saturating_mul(4).min(MAX_OUTPUT_BYTES as u64) as usize;
                }
                _ => return Err(format!("Unknown @options field: {key}")),
            }
        }
        let newline = code
            .find('\n')
            .ok_or("The @options line must be followed by JavaScript")?;
        parsed.code = code[newline..].to_owned();
        if parsed.code.trim().is_empty() {
            return Err("The @options line must be followed by JavaScript".into());
        }
        Ok(parsed)
    }
}
