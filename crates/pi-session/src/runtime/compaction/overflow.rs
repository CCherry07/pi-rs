//! Provider overflow signals, kept in sync with pi-ai/src/utils/overflow.ts.
//! Classification is separate from Session's bounded recovery policy.
use std::sync::LazyLock;

use pi_core::{AssistantMessage, StopReason};
use regex::RegexSet;

static OVERFLOW: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)prompt (?:is )?too long",
        r"(?i)prompt exceeds max length",
        r"(?i)request_too_large",
        r"(?i)input is too long for requested model",
        r"(?i)exceeds the context window",
        r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [0-9,]+ tokens?|\s*\([0-9,]+\))",
        r"(?i)context window exceeded",
        r"(?i)input token count.*exceeds the maximum",
        r"(?i)maximum prompt length is [0-9]+",
        r"(?i)reduce the length of the messages",
        r"(?i)maximum context length is [0-9]+ tokens",
        r"(?i)exceeds (?:the )?maximum allowed input length of [0-9,]+ tokens?",
        r"(?i)input \([0-9]+ tokens\) is longer than the model'?s context length \([0-9]+ tokens\)",
        r"(?i)exceeds the limit of [0-9]+",
        r"(?i)exceeds the available context size",
        r"(?i)greater than the context length",
        r"(?i)context window exceeds limit",
        r"(?i)exceeded model token limit",
        r"(?i)too large for model with [0-9]+ maximum context length",
        r"(?i)prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?",
        r"(?i)model_context_window_exceeded",
        r"(?i)prompt too long; exceeded (?:max )?context length",
        r"(?i)range of input length should be",
        r"(?i)context[_ ]length[_ ]exceeded",
        r"(?i)too many tokens",
        r"(?i)token limit exceeded",
        r"(?i)^(?:[a-z0-9_ -]+ API error \(413\):|413(?:\s|:|$))",
    ]).expect("constant Pi overflow patterns")
});
static NON_OVERFLOW: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)^(Throttling error|Service unavailable):",
        r"(?i)rate limit",
        r"(?i)too many requests",
    ])
    .expect("constant Pi non-overflow patterns")
});
static CEREBRAS_BODYLESS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)")
        .expect("constant Cerebras overflow pattern")
});

pub(super) fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(error) = message.error_message.as_deref()
        && is_overflow_error(error, message.provider.as_str())
    {
        return true;
    }
    let Some(window) = context_window.filter(|window| *window > 0) else {
        return false;
    };
    let input = message.usage.input.saturating_add(message.usage.cache_read);
    match message.stop_reason {
        StopReason::Stop => input > window,
        StopReason::Length if message.usage.output == 0 => input as f64 >= window as f64 * 0.99,
        _ => false,
    }
}

pub(super) fn is_overflow_error(error: &str, provider: &str) -> bool {
    !NON_OVERFLOW.is_match(error)
        && (OVERFLOW.is_match(error)
            || (provider == "cerebras" && CEREBRAS_BODYLESS.is_match(error)))
}

pub(super) fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_current_typescript_overflow_oracle() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/compaction-current-oracle.json"
        ))
        .unwrap();
        for case in fixture["overflow"].as_array().unwrap() {
            let message: AssistantMessage =
                serde_json::from_value(case["message"].clone()).unwrap();
            assert_eq!(
                is_context_overflow(&message, case["contextWindow"].as_u64()),
                case["isContextOverflow"].as_bool().unwrap(),
                "{case}"
            );
            assert_eq!(
                is_recoverable_length(&message, case["desiredMaxOutput"].as_u64().unwrap()),
                case["isRecoverableLength"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}
