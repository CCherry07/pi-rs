use pi_core::{Message, Usage, UsageCost};

use crate::{LaneRecordEntry, SessionDocument, SessionEntry, UsageAttribution};

/// Returns usage billed by one durable session entry.
///
/// Pi attributes assistant and tool-result messages directly, while branch
/// summaries and compactions carry the usage of their isolated summary calls.
pub fn session_entry_usage(entry: &SessionEntry) -> Option<&Usage> {
    match entry {
        SessionEntry::Message(entry) => match entry.message.as_standard() {
            Some(Message::Assistant(message)) => Some(&message.usage),
            Some(Message::ToolResult(message)) => message.usage.as_ref(),
            Some(Message::User(_) | Message::Custom(_)) | None => None,
        },
        SessionEntry::Compaction(entry) => entry.usage.as_ref(),
        SessionEntry::BranchSummary(entry) => entry.usage.as_ref(),
        SessionEntry::CustomMessage(_)
        | SessionEntry::ModelChange(_)
        | SessionEntry::ThinkingLevelChange(_)
        | SessionEntry::ActiveToolsChange(_)
        | SessionEntry::Custom(_) => None,
    }
}

/// Aggregates Pi-compatible billed usage across all supplied session entries.
///
/// Callers choose the scope. Product session totals pass every tree entry so
/// compacted and abandoned history remains part of the amount billed.
pub fn aggregate_session_usage<'a>(entries: impl IntoIterator<Item = &'a SessionEntry>) -> Usage {
    let mut total = Usage::default();
    let mut saw_cache_write_1h = false;
    let mut saw_reasoning = false;
    for usage in entries.into_iter().filter_map(session_entry_usage) {
        total.input = total.input.saturating_add(usage.input);
        total.output = total.output.saturating_add(usage.output);
        total.cache_read = total.cache_read.saturating_add(usage.cache_read);
        total.cache_write = total.cache_write.saturating_add(usage.cache_write);
        if let Some(cache_write_1h) = usage.cache_write_1h {
            saw_cache_write_1h = true;
            total.cache_write_1h = Some(
                total
                    .cache_write_1h
                    .unwrap_or(0)
                    .saturating_add(cache_write_1h),
            );
        }
        if let Some(reasoning) = usage.reasoning {
            saw_reasoning = true;
            total.reasoning = Some(total.reasoning.unwrap_or(0).saturating_add(reasoning));
        }
        total.cost = UsageCost {
            input: total.cost.input + usage.cost.input,
            output: total.cost.output + usage.cost.output,
            cache_read: total.cost.cache_read + usage.cost.cache_read,
            cache_write: total.cost.cache_write + usage.cost.cache_write,
            total: total.cost.total + usage.cost.total,
        };
    }
    total.total_tokens = total
        .input
        .saturating_add(total.output)
        .saturating_add(total.cache_read)
        .saturating_add(total.cache_write);
    if !saw_cache_write_1h {
        total.cache_write_1h = None;
    }
    if !saw_reasoning {
        total.reasoning = None;
    }
    total
}

/// Aggregates all product-visible usage for a complete session document.
///
/// Tree entries carry the usage of normal assistant, tool, and summarization
/// work. Explicit adjustment records account for detached provider work such
/// as a background review without adding that private work to the tree.
pub fn aggregate_document_usage(document: &SessionDocument) -> Usage {
    let base = aggregate_session_usage(document.entries.iter().map(|record| &record.entry));
    let mut input = i128::from(base.input);
    let mut output = i128::from(base.output);
    let mut cache_read = i128::from(base.cache_read);
    let mut cache_write = i128::from(base.cache_write);
    let mut cache_write_1h = base.cache_write_1h.map(i128::from);
    let mut reasoning = base.reasoning.map(i128::from);
    let mut cost = base.cost;

    for adjustment in document.records.iter().filter_map(|record| {
        let LaneRecordEntry::Usage(record) = &record.record else {
            return None;
        };
        matches!(record.attribution, UsageAttribution::Adjustment { .. }).then_some(&record.usage)
    }) {
        input = input.saturating_add(i128::from(adjustment.input));
        output = output.saturating_add(i128::from(adjustment.output));
        cache_read = cache_read.saturating_add(i128::from(adjustment.cache_read));
        cache_write = cache_write.saturating_add(i128::from(adjustment.cache_write));
        if let Some(value) = adjustment.cache_write_1h {
            let current = cache_write_1h.get_or_insert(0);
            *current = current.saturating_add(i128::from(value));
        }
        if let Some(value) = adjustment.reasoning {
            let current = reasoning.get_or_insert(0);
            *current = current.saturating_add(i128::from(value));
        }
        cost.input += adjustment.cost.input;
        cost.output += adjustment.cost.output;
        cost.cache_read += adjustment.cost.cache_read;
        cost.cache_write += adjustment.cost.cache_write;
        cost.total += adjustment.cost.total;
    }

    let input = nonnegative_u64(input);
    let output = nonnegative_u64(output);
    let cache_read = nonnegative_u64(cache_read);
    let cache_write = nonnegative_u64(cache_write);
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: cache_write_1h.map(nonnegative_u64),
        reasoning: reasoning.map(nonnegative_u64),
        total_tokens: input
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
        cost: UsageCost {
            input: cost.input.max(0.0),
            output: cost.output.max(0.0),
            cache_read: cost.cache_read.max(0.0),
            cache_write: cost.cache_write.max(0.0),
            total: cost.total.max(0.0),
        },
    }
}

fn nonnegative_u64(value: i128) -> u64 {
    if value <= 0 {
        0
    } else {
        u64::try_from(value).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{
        AssistantMessage, ContentBlock, ModelId, ProviderId, StopReason, TextContent, ToolCallId,
        ToolResultMessage,
    };

    use super::*;
    use crate::{
        AgentMessage, BranchSummaryEntry, CompactionEntry, LaneRecord, SessionDocument,
        SessionHeader, SessionStats, SessionUsage, UsageAttribution, UsageRecord,
    };

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) -> Usage {
        Usage {
            input,
            output,
            cache_read,
            cache_write,
            total_tokens: input + output + cache_read + cache_write,
            cost: UsageCost {
                total: cost,
                ..UsageCost::default()
            },
            ..Usage::default()
        }
    }

    #[test]
    fn totals_include_tool_results_compactions_and_branch_summaries() {
        let assistant = SessionEntry::message(Message::assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new("answer"))],
            api: "test".to_string(),
            provider: ProviderId::new("test"),
            model: ModelId::new("model"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(10, 2, 3, 4, 0.1),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        }));
        let tool_result = SessionEntry::message(Message::tool_result(ToolResultMessage {
            tool_call_id: ToolCallId::new("call"),
            tool_name: "metered".to_string(),
            content: vec![ContentBlock::Text(TextContent::new("result"))],
            details: None,
            usage: Some(usage(20, 5, 6, 7, 0.2)),
            added_tool_names: None,
            is_error: false,
            timestamp_ms: 2,
        }));
        let compaction = SessionEntry::Compaction(CompactionEntry {
            summary: "summary".to_string(),
            retained_tail: Vec::<AgentMessage>::new(),
            tokens_before: 100,
            details: None,
            usage: Some(usage(30, 8, 9, 10, 0.3)),
        });
        let branch = SessionEntry::BranchSummary(BranchSummaryEntry {
            from_id: "root".to_string(),
            summary: "branch".to_string(),
            details: None,
            usage: Some(usage(40, 11, 12, 13, 0.4)),
        });

        let total = aggregate_session_usage([&assistant, &tool_result, &compaction, &branch]);

        assert_eq!(total.input, 100);
        assert_eq!(total.output, 26);
        assert_eq!(total.cache_read, 30);
        assert_eq!(total.cache_write, 34);
        assert_eq!(total.total_tokens, 190);
        assert!((total.cost.total - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn document_totals_add_only_explicit_usage_adjustments() {
        let assistant = SessionEntry::message(Message::assistant(AssistantMessage {
            content: vec![ContentBlock::Text(TextContent::new("answer"))],
            api: "test".to_string(),
            provider: ProviderId::new("test"),
            model: ModelId::new("model"),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: usage(10, 2, 3, 4, 1.0),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 1,
        }));
        let adjustment = |id: &str, input: i64, output: i64, cost: f64| LaneRecord {
            id: id.to_string(),
            seq: 2,
            lane: crate::MAIN_LANE.to_string(),
            timestamp_ms: 2,
            record: LaneRecordEntry::Usage(UsageRecord {
                usage: SessionUsage {
                    input,
                    output,
                    total_tokens: input.saturating_add(output),
                    cost: UsageCost {
                        total: cost,
                        ..UsageCost::default()
                    },
                    ..SessionUsage::default()
                },
                attribution: UsageAttribution::Adjustment {
                    run_id: None,
                    entry_id: None,
                    details: None,
                },
            }),
        };
        let document = SessionDocument {
            header: SessionHeader::new("usage", "."),
            entries: vec![crate::SessionRecord {
                id: "assistant".to_string(),
                seq: 1,
                parent_id: None,
                timestamp_ms: 1,
                entry: assistant,
            }],
            records: vec![
                adjustment("background-review", 5, 1, 0.5),
                adjustment("credit", -2, 0, -0.1),
            ],
            lanes: Vec::new(),
            log: Vec::new(),
            name: None,
            labels: std::collections::HashMap::new(),
            stats: SessionStats::default(),
        };

        let total = aggregate_document_usage(&document);

        assert_eq!(total.input, 13);
        assert_eq!(total.output, 3);
        assert_eq!(total.cache_read, 3);
        assert_eq!(total.cache_write, 4);
        assert_eq!(total.total_tokens, 23);
        assert!((total.cost.total - 1.4).abs() < f64::EPSILON);
    }
}
