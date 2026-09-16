use std::hint::black_box;

use pi_agent::StreamAssembler;
use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{
    ContentMetadata, ModelId, ProviderId, ResponseMetadata, StopReason, StreamEvent, ToolCallId,
    Usage,
};

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(25, 100)?;
    let mut report = BenchmarkReport::new("stream_assembly");

    report.push(stream_case("text_256x64", 256, 64, false, config)?);
    report.push(stream_case("mixed_256x64", 256, 64, true, config)?);
    report.finish()
}

fn stream_case(
    name: &str,
    delta_count: usize,
    delta_bytes: usize,
    mixed_content: bool,
    config: BenchConfig,
) -> BenchResult<pi_bench::BenchmarkCase> {
    let events = stream_fixture(delta_count, delta_bytes, mixed_content);
    verify_stream(&events, mixed_content)?;
    let identity = format!("stream:v1:{delta_count}:{delta_bytes}:{mixed_content}");
    let parameters = parameter_map(&[
        ("delta_count", delta_count),
        ("delta_bytes", delta_bytes),
        ("event_count", events.len()),
    ]);

    measure(name, config, fixture_hash(identity), parameters, || {
        verify_stream(black_box(&events), mixed_content)
    })
}

fn stream_fixture(delta_count: usize, delta_bytes: usize, mixed_content: bool) -> Vec<StreamEvent> {
    let mut events = vec![StreamEvent::Start {
        metadata: ResponseMetadata::new(
            ProviderId::new("scripted"),
            ModelId::new("bench"),
            "scripted",
            1,
        ),
    }];
    let mut content_index = 0;

    if mixed_content {
        events.push(StreamEvent::ThinkingStart { content_index });
        events.push(StreamEvent::ThinkingDelta {
            content_index,
            delta: "considering the benchmark fixture".repeat(8),
        });
        events.push(StreamEvent::ThinkingEnd {
            content_index,
            thinking_signature: Some("opaque-benchmark-signature".to_string()),
        });
        content_index += 1;
    }

    events.push(StreamEvent::TextStart { content_index });
    let delta = "x".repeat(delta_bytes);
    for _ in 0..delta_count {
        events.push(StreamEvent::TextDelta {
            content_index,
            delta: delta.clone(),
        });
    }
    events.push(StreamEvent::TextEnd {
        content_index,
        text_signature: None,
    });

    if mixed_content {
        content_index += 1;
        events.push(StreamEvent::ToolCallStart {
            content_index,
            id: ToolCallId::new("bench-call"),
            name: "read".to_string(),
        });
        events.push(StreamEvent::ContentMetadata {
            content_index,
            metadata: ContentMetadata::ToolCall {
                namespace: Some("bench".to_string()),
            },
        });
        for arguments_delta in ["{\"path\":", "\"src/", "lib.rs\"", "}"] {
            events.push(StreamEvent::ToolCallDelta {
                content_index,
                arguments_delta: arguments_delta.to_string(),
            });
        }
        events.push(StreamEvent::ToolCallEnd {
            content_index,
            thought_signature: None,
        });
    }

    events.push(StreamEvent::Done {
        reason: if mixed_content {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        },
        usage: Usage::default(),
    });
    events
}

fn verify_stream(events: &[StreamEvent], mixed_content: bool) -> Result<(), String> {
    let mut assembler = StreamAssembler::new();
    for event in events.iter().cloned() {
        assembler.push(event).map_err(|error| error.to_string())?;
    }
    let message = assembler.finish().map_err(|error| error.to_string())?;
    let expected_blocks = if mixed_content { 3 } else { 1 };
    if message.content.len() != expected_blocks {
        return Err(format!(
            "assembled {} blocks, expected {expected_blocks}",
            message.content.len()
        ));
    }
    if mixed_content
        && message
            .tool_calls()
            .first()
            .and_then(|call| call.arguments.get("path"))
            .and_then(serde_json::Value::as_str)
            != Some("src/lib.rs")
    {
        return Err("assembled tool call arguments changed".to_string());
    }
    black_box(message);
    Ok(())
}
