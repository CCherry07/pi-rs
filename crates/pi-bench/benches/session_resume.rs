use std::hint::black_box;
use std::path::PathBuf;

use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{
    AssistantMessage, ContentBlock, Message, ModelId, ProviderId, StopReason, TextContent,
    ToolCall, ToolCallId, ToolResultMessage, Usage, UserMessage,
};
use pi_session::{
    EntryOrder, LaneRecordEntry, MAIN_LANE, NewLaneRecord, OperationIntent, OperationOutcome,
    RecordQuery, SessionEntry, SessionHeader, SessionLog, SessionMutation,
};
use tempfile::TempDir;

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(3, 20)?;
    let mut report = BenchmarkReport::new("session_resume");

    for (name, turns, payload_bytes) in [
        ("small", 64, 256),
        ("medium", 1_024, 512),
        ("large", 4_096, 512),
    ] {
        let fixture = SessionFixture::create(name, turns, payload_bytes)?;
        fixture.verify()?;
        let parameters = parameter_map(&[
            ("turns", turns.to_string()),
            ("entries", fixture.expected_entries.to_string()),
            ("payload_bytes", payload_bytes.to_string()),
            ("file_bytes", fixture.file_bytes.to_string()),
        ]);
        let identity = format!("session:v1:{turns}:{payload_bytes}");
        report.push(measure(
            format!("resume_{name}"),
            config,
            fixture_hash(identity),
            parameters.clone(),
            || fixture.verify(),
        )?);
        if name == "large" {
            fixture.measure_phases(&mut report, config, &parameters)?;
        }
    }

    let mixed = SessionFixture::create_mixed("mixed-large", 2_048, 256)?;
    mixed.verify()?;
    let mixed_parameters = parameter_map(&[
        ("turns", "2048".to_string()),
        ("entries", mixed.expected_entries.to_string()),
        ("records", mixed.expected_records.to_string()),
        ("payload_bytes", "256".to_string()),
        ("file_bytes", mixed.file_bytes.to_string()),
    ]);
    report.push(measure(
        "resume_mixed_large",
        config,
        fixture_hash("session-mixed:v1:2048:256"),
        mixed_parameters.clone(),
        || mixed.verify(),
    )?);
    mixed.measure_settled_recovery_scan(
        &mut report,
        config,
        &mixed_parameters,
        "resume_mixed_large_settled_recovery_scan",
    )?;

    report.finish()
}

struct SessionFixture {
    _directory: TempDir,
    path: PathBuf,
    expected_entries: usize,
    expected_records: usize,
    file_bytes: u64,
}

impl SessionFixture {
    fn create(name: &str, turns: usize, payload_bytes: usize) -> BenchResult<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(format!("{name}.jsonl"));
        let log = SessionLog::create(
            &path,
            SessionHeader::new(format!("bench-{name}"), directory.path()),
        )?;
        let payload = "x".repeat(payload_bytes);
        let mut entries = Vec::with_capacity(turns * 3);
        for turn in 0..turns {
            let timestamp = i64::try_from(turn).unwrap_or(i64::MAX);
            let call_id = ToolCallId::new(format!("call-{turn}"));
            entries.push(SessionEntry::message(Message::User(UserMessage::text(
                format!("request-{turn}:{payload}"),
                timestamp,
            ))));
            entries.push(SessionEntry::message(Message::assistant(
                AssistantMessage {
                    content: vec![ContentBlock::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": format!("fixture/{turn}.rs")}),
                        thought_signature: None,
                        namespace: None,
                    })],
                    api: "scripted".to_string(),
                    provider: ProviderId::new("scripted"),
                    model: ModelId::new("bench"),
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    deferred: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp_ms: timestamp,
                },
            )));
            entries.push(SessionEntry::message(Message::tool_result(
                ToolResultMessage {
                    tool_call_id: call_id,
                    tool_name: "read".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new(format!(
                        "result-{turn}:{payload}"
                    )))],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: timestamp,
                },
            )));
        }
        let expected_entries = entries.len();
        log.append_batch(entries)?;
        drop(log);
        let file_bytes = std::fs::metadata(&path)?.len();
        Ok(Self {
            _directory: directory,
            path,
            expected_entries,
            expected_records: 0,
            file_bytes,
        })
    }

    fn create_mixed(name: &str, turns: usize, payload_bytes: usize) -> BenchResult<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join(format!("{name}.jsonl"));
        let log = SessionLog::create_deferred(
            &path,
            SessionHeader::new(format!("bench-{name}"), directory.path()),
        )?;
        let payload = "x".repeat(payload_bytes);
        for turn in 0..turns {
            let source_leaf_id = log.leaf_id();
            let run_id = format!("run-{turn}");
            log.append_record(NewLaneRecord {
                id: run_id.clone(),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::OperationStarted {
                    source_leaf_id,
                    intent: OperationIntent::Run {
                        original_prompt: Vec::new(),
                        initial_messages: Vec::new(),
                        system_prompt_override: None,
                        resume_data: None,
                    },
                },
            })?;

            let timestamp = i64::try_from(turn).unwrap_or(i64::MAX);
            let call_id = ToolCallId::new(format!("mixed-call-{turn}"));
            log.append_batch([
                SessionEntry::message(Message::User(UserMessage::text(
                    format!("request-{turn}:{payload}"),
                    timestamp,
                ))),
                SessionEntry::message(Message::assistant(AssistantMessage {
                    content: vec![ContentBlock::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": format!("fixture/{turn}.rs")}),
                        thought_signature: None,
                        namespace: None,
                    })],
                    api: "scripted".to_string(),
                    provider: ProviderId::new("scripted"),
                    model: ModelId::new("bench"),
                    response_model: None,
                    response_id: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    deferred: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp_ms: timestamp,
                })),
                SessionEntry::message(Message::tool_result(ToolResultMessage {
                    tool_call_id: call_id,
                    tool_name: "read".to_string(),
                    content: vec![ContentBlock::Text(TextContent::new(format!(
                        "result-{turn}:{payload}"
                    )))],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: false,
                    timestamp_ms: timestamp,
                })),
            ])?;
            log.append_record(NewLaneRecord {
                id: format!("finish-{turn}"),
                lane: MAIN_LANE.to_string(),
                record: LaneRecordEntry::OperationFinished {
                    run_id,
                    outcome: OperationOutcome::Completed,
                    error: None,
                },
            })?;
        }
        log.materialize()?;
        drop(log);
        let file_bytes = std::fs::metadata(&path)?.len();
        Ok(Self {
            _directory: directory,
            path,
            expected_entries: turns * 3,
            expected_records: turns * 2,
            file_bytes,
        })
    }

    fn verify(&self) -> Result<(), String> {
        let (_, document) = SessionLog::open(&self.path).map_err(|error| error.to_string())?;
        let branch = document.branch().map_err(|error| error.to_string())?;
        if branch.len() != self.expected_entries {
            return Err(format!(
                "restored {} entries, expected {}",
                branch.len(),
                self.expected_entries
            ));
        }
        if document.records.len() != self.expected_records {
            return Err(format!(
                "restored {} records, expected {}",
                document.records.len(),
                self.expected_records
            ));
        }
        let provider_messages = document
            .context()
            .map_err(|error| error.to_string())?
            .provider_messages();
        if provider_messages.len() != self.expected_entries {
            return Err(format!(
                "projected {} provider messages, expected {}",
                provider_messages.len(),
                self.expected_entries
            ));
        }
        if !matches!(provider_messages.last(), Some(Message::ToolResult(_))) {
            return Err("provider projection ended with a dangling tool call".to_string());
        }
        black_box(provider_messages);
        Ok(())
    }

    fn measure_phases(
        &self,
        report: &mut BenchmarkReport,
        config: BenchConfig,
        parameters: &std::collections::BTreeMap<String, String>,
    ) -> BenchResult<()> {
        let fixture_identity = fixture_hash(format!(
            "session:v1:{}:{}",
            self.expected_entries, self.file_bytes
        ));
        report.push(measure(
            "resume_large_open_handle",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                let log = SessionLog::open_handle(&self.path).map_err(|error| error.to_string())?;
                black_box(log);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_generic_typed_decode",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                let bytes = std::fs::read(&self.path).map_err(|error| error.to_string())?;
                let mutations = bytes
                    .split(|byte| *byte == b'\n')
                    .skip(1)
                    .filter(|line| !line.is_empty())
                    .map(serde_json::from_slice::<SessionMutation>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
                black_box(mutations);
                Ok::<(), String>(())
            },
        )?);

        let log = SessionLog::open_handle(&self.path).map_err(|error| error.to_string())?;
        self.measure_settled_recovery_scan(
            report,
            config,
            parameters,
            "resume_large_settled_recovery_scan",
        )?;
        report.push(measure(
            "resume_large_indexed_branch",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                black_box(log.branch_entries().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_indexed_context",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                black_box(log.context().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_full_document_branch",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                let document = log.load().map_err(|error| error.to_string())?;
                black_box(document.branch().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_full_document_context",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                let document = log.load().map_err(|error| error.to_string())?;
                black_box(document.context().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_document_snapshot",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                black_box(log.load().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        let document = log.load().map_err(|error| error.to_string())?;
        report.push(measure(
            "resume_large_branch",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                black_box(document.branch().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        report.push(measure(
            "resume_large_context",
            config,
            fixture_identity.clone(),
            parameters.clone(),
            || {
                black_box(document.context().map_err(|error| error.to_string())?);
                Ok::<(), String>(())
            },
        )?);
        let context = document.context().map_err(|error| error.to_string())?;
        report.push(measure(
            "resume_large_provider_projection",
            config,
            fixture_identity,
            parameters.clone(),
            || {
                black_box(context.provider_messages());
                Ok::<(), String>(())
            },
        )?);
        Ok(())
    }

    fn measure_settled_recovery_scan(
        &self,
        report: &mut BenchmarkReport,
        config: BenchConfig,
        parameters: &std::collections::BTreeMap<String, String>,
        name: &str,
    ) -> BenchResult<()> {
        let fixture_identity = fixture_hash(format!(
            "session-recovery:v1:{}:{}:{}",
            self.expected_entries, self.expected_records, self.file_bytes
        ));
        let log = SessionLog::open_handle(&self.path).map_err(|error| error.to_string())?;
        report.push(measure(
            name,
            config,
            fixture_identity,
            parameters.clone(),
            || {
                let open_operations = log
                    .find_open_operations(MAIN_LANE, None)
                    .map_err(|error| error.to_string())?;
                let records = log
                    .find_records(&RecordQuery {
                        order: EntryOrder::OldestFirst,
                        ..RecordQuery::default()
                    })
                    .map_err(|error| error.to_string())?;
                black_box((open_operations, records));
                Ok::<(), String>(())
            },
        )?);
        Ok(())
    }
}
