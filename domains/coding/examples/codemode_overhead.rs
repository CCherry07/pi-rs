//! Run with: cargo run --release -p pi-coding --example codemode_overhead
//!
//! Measures the real scheduler and JS worker with deterministic in-process model
//! responses. It does not include network/model inference or session disk I/O.
//! Every codemode execution creates a new VM; warm samples only reuse the process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pi_agent::AgentOptions;
use pi_core::{
    ContentBlock, Message, PluginId, ToolCall, ToolCallId, ToolExecutionMode, ToolResult, ToolSpec,
};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_codemode::CodemodePlugin;
use pi_runtime::PiRuntime;
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde::Serialize;
use serde_json::{Value, json};

const WARM_SAMPLES: usize = 20;

#[derive(Clone, Copy)]
struct SyntheticTool {
    delay_ms: u64,
    payload_bytes: usize,
}

#[async_trait]
impl Tool for SyntheticTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            output_schema: None,
            constrained_sampling: None,
            name: "synthetic".into(),
            label: "Synthetic read".into(),
            description: "Return a fixed-size payload after an optional delay.".into(),
            parameters: json!({"type":"object","properties":{"index":{"type":"integer"}},"required":["index"]}),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        Ok(ToolResult::text(format!(
            "item-{}:{}",
            input["index"],
            "x".repeat(self.payload_bytes)
        )))
    }
}

#[pi_plugin::plugin]
impl Plugin for SyntheticTool {
    fn id(&self) -> PluginId {
        PluginId::new("synthetic-benchmark")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(*self))
    }
}

#[derive(Clone, Copy)]
enum Mode {
    DirectBatch,
    DirectTurns,
    CodemodeParallel,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::DirectBatch => "direct_parallel_batch",
            Self::DirectTurns => "direct_separate_turns",
            Self::CodemodeParallel => "codemode_parallel_filtered",
        }
    }

    fn turns(self, calls: usize) -> Vec<ScriptedTurn> {
        let calls: Vec<_> = (0..calls)
            .map(|index| {
                ToolCall::new(format!("call-{index}"), "synthetic", json!({"index":index}))
            })
            .collect();
        let mut turns = match self {
            Self::DirectBatch => vec![ScriptedTurn::ToolCalls(calls)],
            Self::DirectTurns => calls
                .into_iter()
                .map(|call| ScriptedTurn::ToolCalls(vec![call]))
                .collect(),
            Self::CodemodeParallel => vec![ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "outer",
                "codemode",
                json!({"code":format!(
                    "const values = await Promise.all(Array.from({{length:{}}}, (_, index) => tools.synthetic({{index}}))); text(values.map(value => value.slice(0, 6)));", calls.len()
                )}),
            )])],
        };
        turns.push(ScriptedTurn::Text("done".into()));
        turns
    }
}

#[derive(Serialize)]
struct Measurement {
    mode: &'static str,
    tool_calls: usize,
    synthetic_delay_ms: u64,
    payload_bytes_per_tool: usize,
    first_run_ms: f64,
    warm_p50_ms: f64,
    warm_p95_ms: f64,
    provider_requests_per_run: usize,
    model_visible_tool_text_bytes: usize,
}

async fn measure(
    mode: Mode,
    calls: usize,
    tool: SyntheticTool,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let provider =
        ScriptedProviderPlugin::scripted((0..=WARM_SAMPLES).flat_map(|_| mode.turns(calls)));
    let recorded = provider.provider();
    let runtime = PiRuntime::builder()
        .plugin(CodemodePlugin)
        .plugin(tool)
        .provider_plugin(provider)
        .agent_options(AgentOptions {
            cwd: std::env::current_dir()?,
            active_tools: vec!["synthetic".into(), "codemode".into()],
            max_parallel_tools: 8,
            ..Default::default()
        })
        .build()?;
    let mut timings = Vec::new();
    let mut output_bytes = None;
    for _ in 0..=WARM_SAMPLES {
        // Keep input history identical and exclude reset cost from the measurement.
        runtime.agent().replace_context_messages(Vec::new())?;
        let start = Instant::now();
        let outcome = runtime.prompt("Run benchmark.").await?;
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
        let mut text_bytes = 0;
        for message in outcome.new_messages {
            if let Message::ToolResult(result) = message {
                if result.is_error {
                    return Err(format!("benchmark tool failed: {:?}", result.content).into());
                }
                text_bytes += result
                    .content
                    .iter()
                    .map(|content| match content {
                        ContentBlock::Text(text) => text.text.len(),
                        _ => 0,
                    })
                    .sum::<usize>();
            }
        }
        // Runtime headings may include timings; report the first measured result.
        output_bytes.get_or_insert(text_bytes);
    }
    let first_run_ms = timings.remove(0);
    timings.sort_by(f64::total_cmp);
    Ok(Measurement {
        mode: mode.name(),
        tool_calls: calls,
        synthetic_delay_ms: tool.delay_ms,
        payload_bytes_per_tool: tool.payload_bytes,
        first_run_ms,
        warm_p50_ms: timings[WARM_SAMPLES / 2],
        warm_p95_ms: timings[(WARM_SAMPLES * 95 / 100) - 1],
        provider_requests_per_run: recorded.requests().len() / (WARM_SAMPLES + 1),
        model_visible_tool_text_bytes: output_bytes.unwrap_or_default(),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut measurements = Vec::new();
    for (calls, delay_ms, payload_bytes) in [(1, 0, 0), (8, 0, 4096), (8, 10, 4096)] {
        for mode in [Mode::DirectBatch, Mode::DirectTurns, Mode::CodemodeParallel] {
            measurements.push(
                measure(
                    mode,
                    calls,
                    SyntheticTool {
                        delay_ms,
                        payload_bytes,
                    },
                )
                .await?,
            );
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scope":"Real scheduler and fresh JS VM; deterministic in-process provider; no model/network or session disk I/O",
            "sample_note":"First run is the first sample of each case, not a fresh process; every JS sample uses a fresh VM",
            "build_mode":if cfg!(debug_assertions) { "debug" } else { "optimized" },
            "architecture":std::env::consts::ARCH,
            "os":std::env::consts::OS,
            "warm_samples":WARM_SAMPLES,
            "max_parallel_tools":8,
            "measurements":measurements,
        }))?
    );
    Ok(())
}
