use std::time::Duration;

use serde_json::Value;

#[derive(Debug)]
pub(crate) struct Source {
    pub code: String,
    pub timeout: Option<Duration>,
    /// Four UTF-16 code units per estimated token, not UTF-8 bytes.
    pub output_chars: usize,
}

const FIELDS: &str = "`max_output_tokens` and `timeout_ms`";
fn whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
fn integer(value: &Value, max: f64) -> Option<u64> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= max && v.fract() == 0.0)
        .map(|v| v as u64)
}

impl Source {
    pub fn parse(code: &str) -> Result<Self, String> {
        if code.trim_matches(whitespace).is_empty() {
            return Err("Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`.".into());
        }
        const PREFIX: &str = "// @options:";
        for (index, line) in code.split('\n').enumerate().skip(1) {
            if line.trim_start_matches(whitespace).starts_with(PREFIX) {
                return Err(format!(
                    "The {PREFIX} line must be the first line of the input; found one on line {}",
                    index + 1
                ));
            }
        }
        let mut parsed = Self {
            code: code.into(),
            timeout: None,
            output_chars: 10_000 * 4,
        };
        let first = code
            .split('\n')
            .next()
            .unwrap_or_default()
            .trim_start_matches(whitespace);
        let Some(options) = first.strip_prefix(PREFIX) else {
            return Ok(parsed);
        };
        let newline = code
            .find('\n')
            .ok_or("The @options line must be followed by JavaScript source on subsequent lines")?;
        parsed.code = code[newline..].to_owned();
        if parsed.code.trim_matches(whitespace).is_empty() {
            return Err(
                "The @options line must be followed by JavaScript source on subsequent lines"
                    .into(),
            );
        }
        let options = options.trim_matches(whitespace);
        if options.is_empty() {
            return Err(format!(
                "@options must be a JSON object with supported fields {FIELDS}"
            ));
        }
        let options: Value = serde_json::from_str(options).map_err(|error| {
            format!("@options must be valid JSON with supported fields {FIELDS}: {error}")
        })?;
        let fields = options.as_object().ok_or_else(|| {
            format!("@options must be a JSON object with supported fields {FIELDS}")
        })?;
        for key in fields.keys() {
            if key != "max_output_tokens" && key != "timeout_ms" {
                return Err(format!("@options only supports {FIELDS}; got `{key}`"));
            }
        }
        if let Some(value) = fields.get("max_output_tokens") {
            let count = integer(value, 9_007_199_254_740_991.0)
                .ok_or("@options field `max_output_tokens` must be a non-negative safe integer")?;
            parsed.output_chars = usize::try_from(count.saturating_mul(4)).unwrap_or(usize::MAX);
        }
        if let Some(value) = fields.get("timeout_ms") {
            let millis = integer(value, 2_147_483_647.0)
                .filter(|v| *v > 0)
                .ok_or("@options field `timeout_ms` must be a positive integer up to 2147483647")?;
            parsed.timeout = Some(Duration::from_millis(millis));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_default_and_large_source_follow_coding_agent_policy() {
        assert_eq!(Source::parse("return 1").unwrap().timeout, None);
        let source = format!("/*{}*/return 1", "x".repeat(1024 * 1024));
        assert!(Source::parse(&source).is_ok());
        let parsed = Source::parse(
            "\u{feff}// @options: {\"timeout_ms\":1.0,\"max_output_tokens\":1e6}\nreturn 1",
        )
        .unwrap();
        assert_eq!(parsed.timeout, Some(Duration::from_millis(1)));
        assert_eq!(parsed.output_chars, 4_000_000);
        assert_eq!(parsed.code, "\nreturn 1");
    }
}
