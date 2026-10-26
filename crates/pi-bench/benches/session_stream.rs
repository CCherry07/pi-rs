use std::hint::black_box;
use std::sync::Arc;

use pi_agent::AgentOptions;
use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{
    Message, ModelId, ProviderId, ResponseMetadata, StopReason, StreamEvent, Usage, UserMessage,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, CompactionSettings, SessionEntry, SessionHeader,
    SessionStore,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tempfile::TempDir;

const PAYLOAD_BYTES: usize = 128;
const DELTA_BYTES: usize = 16;

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(3, 12)?;
    let executor = tokio::runtime::Runtime::new()?;
    let mut report = BenchmarkReport::new("session_stream");
    for history_messages in [64, 1_024, 4_096] {
        for delta_count in [16, 256] {
            // Every timed run starts from the same history. Construction, journal
            // seeding and resume happen outside the timed Agent -> Session path.
            let fixtures = (0..config.warmup_iterations + config.sample_iterations)
                .map(|_| executor.block_on(Fixture::new(history_messages, delta_count)))
                .collect::<BenchResult<Vec<_>>>()?;
            let mut runs = fixtures.iter();
            report.push(measure(
                format!("stream_{delta_count}_deltas_after_{history_messages}_messages"),
                config,
                fixture_hash(format!("session-stream:v1:{history_messages}:{delta_count}:{PAYLOAD_BYTES}:{DELTA_BYTES}")),
                parameter_map(&[
                    ("history_messages", history_messages),
                    ("delta_count", delta_count),
                    ("payload_bytes", PAYLOAD_BYTES),
                    ("delta_bytes", DELTA_BYTES),
                ]),
                || {
                    let fixture = runs.next().expect("one fixture per measurement");
                    let pi_session::PromptOutcome::Agent(outcome) = executor.block_on(fixture.session.prompt("continue"))
                        .map_err(|error| error.to_string())?
                    else {
                        return Err("expected a streaming agent run".to_string());
                    };
                    if outcome.final_context.messages.len() != history_messages + 2 {
                        return Err("streaming changed the expected conversation length".to_string());
                    }
                    black_box(fixture.session.snapshot());
                    Ok(())
                },
            )?);
        }
    }
    report.finish()
}

struct Fixture {
    _directory: TempDir,
    session: Arc<AgentSession>,
}

impl Fixture {
    async fn new(history_messages: usize, delta_count: usize) -> BenchResult<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("session.jsonl");
        let log =
            SessionStore::create(&path, SessionHeader::new("bench-stream", directory.path()))?;
        let payload = "x".repeat(PAYLOAD_BYTES);
        log.append_batch((0..history_messages).map(|index| {
            SessionEntry::message(Message::User(UserMessage::text(
                format!("history-{index}:{payload}"),
                index as i64,
            )))
        }))?;
        let mut events = vec![
            StreamEvent::Start {
                metadata: ResponseMetadata::new(
                    ProviderId::new("scripted"),
                    ModelId::new("test"),
                    "scripted",
                    1,
                ),
            },
            StreamEvent::TextStart { content_index: 0 },
        ];
        events.extend((0..delta_count).map(|_| StreamEvent::TextDelta {
            content_index: 0,
            delta: "y".repeat(DELTA_BYTES),
        }));
        events.extend([
            StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage::default(),
            },
        ]);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Events(
                events,
            )]))
            .agent_options(AgentOptions {
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .build()?;
        let session = AgentSession::open(
            runtime,
            path,
            AgentSessionOptions::default().compaction(CompactionSettings {
                enabled: false,
                ..CompactionSettings::default()
            }),
        )
        .await?;
        Ok(Self {
            _directory: directory,
            session,
        })
    }
}
