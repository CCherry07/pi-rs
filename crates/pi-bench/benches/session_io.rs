use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::AgentOptions;
use pi_bench::{
    BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure_with_setup, parameter_map,
};
use pi_core::{Message, ModelId, PluginId, ProviderId, UserMessage};
use pi_plugin::{AgentPluginContext, BoundaryResult, Plugin, PluginError, TurnEndEvent};
use pi_runtime::PiRuntime;
use pi_session::{
    ActiveToolsEntry, AgentSession, AgentSessionOptions, BranchQuery, CompactionSettings,
    EntryQuery, ExactSessionIdResolution, ForkOptions, ForkPosition, JsonlSessionRepo,
    ModelChangeEntry, QueueKind, SessionEntry, SessionHeader, SessionMutation, SessionRecord,
    SessionStore, ThinkingLevelEntry,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tempfile::TempDir;

const TIMESTAMP_MS: i64 = 1_793_001_600_123;
const PAYLOAD_BYTES: usize = 128;

fn main() -> BenchResult {
    let config = BenchConfig::from_env(3, 20)?;
    let executor = tokio::runtime::Runtime::new()?;
    let mut report = BenchmarkReport::new("session_io");
    let seed = Seed::new(4_096)?;

    for retained in [false, true] {
        for batch in [1, 4, 16] {
            report.push(measure_with_setup(
                format!("durable_append_{batch}_retained_{retained}"),
                config,
                identity(&format!("append:{batch}:{retained}"), 4_096),
                parameter_map(&[
                    ("history_messages", "4096".into()),
                    ("batch_entries", batch.to_string()),
                    ("writer_retained", retained.to_string()),
                    ("sync_data_per_sample", "1".into()),
                ]),
                || {
                    let fixture = seed.open()?;
                    if retained {
                        fixture.store.set_name(Some("warm writer".into()))?;
                    }
                    Ok((fixture, (0..batch).map(message_entry).collect::<Vec<_>>()))
                },
                |(fixture, entries)| -> BenchResult<_> {
                    Ok(fixture.store.append_batch(std::mem::take(entries))?)
                },
                |(fixture, _), ids| {
                    assert_eq!(ids.len(), batch);
                    assert_eq!(fixture.store.stats().message_count, 4_096 + batch as u64);
                    assert_eq!(
                        SessionStore::read(&fixture.path)?.stats.message_count,
                        4_096 + batch as u64
                    );
                    Ok(())
                },
            )?);
        }
    }

    for history in [4_096, 16_384] {
        let seed = Seed::new(history)?;
        for kept in [10, history] {
            report.push(measure_with_setup(
                format!("fork_{kept}_from_{history}"),
                config,
                identity(&format!("fork:{kept}"), history),
                parameter_map(&[("history_messages", history), ("kept_messages", kept)]),
                || seed.open(),
                |fixture| -> BenchResult<_> {
                    Ok(fixture.store.fork(
                        fixture.directory.path().join("fork.jsonl"),
                        SessionHeader::new("fork", fixture.directory.path()),
                        &ForkOptions::Branch {
                            entry_id: Some(format!("message-{}", kept - 1)),
                            position: Some(ForkPosition::At),
                        },
                    )?)
                },
                |fixture, fork| {
                    assert_eq!(fork.stats().message_count, kept as u64);
                    let disk = SessionStore::read(fork.path())?;
                    assert_eq!(fork.load()?.log, disk.log);
                    assert_eq!(fixture.store.stats().message_count, history as u64);
                    Ok(())
                },
            )?);
        }
        let fixture = seed.open()?;
        let query = BranchQuery {
            entries: EntryQuery {
                limit: Some(50),
                ..Default::default()
            },
            ..Default::default()
        };
        report.push(measure_with_setup(
            format!("branch_newest_50_of_{history}"),
            config,
            identity("branch:50", history),
            parameter_map(&[("history_messages", history), ("limit", 50)]),
            || Ok(()),
            |_| -> BenchResult<_> { Ok(black_box(fixture.store.find_entries_on_branch(&query)?)) },
            |_, records| {
                assert_eq!(records.len(), 50);
                assert_eq!(records[0].id, format!("message-{}", history - 1));
                assert_eq!(records[49].id, format!("message-{}", history - 50));
                Ok(())
            },
        )?);
    }

    for mode in ["first_writer", "next_turn", "open_and_prompt"] {
        report.push(measure_with_setup(
            format!("prompt_4096_hooks_4_{mode}"),
            config,
            identity(mode, 4_096),
            parameter_map(&[
                ("history_messages", "4096"),
                ("native_turn_end_hooks", "4"),
                ("mode", mode),
                ("provider", "scripted; no network"),
            ]),
            || {
                let fixture = RuntimeFixture::new(&executor, &seed, mode != "open_and_prompt")?;
                if mode == "next_turn" {
                    executor.block_on(fixture.session()?.prompt("warmup turn"))?;
                }
                Ok(fixture)
            },
            |fixture| -> BenchResult<_> {
                fixture.open()?;
                Ok(executor.block_on(fixture.session()?.prompt("continue"))?)
            },
            |fixture, _| {
                let messages = if mode == "next_turn" { 4_100 } else { 4_098 };
                assert_eq!(fixture.session()?.snapshot().agent.messages.len(), messages);
                assert_eq!(
                    SessionStore::read(&fixture.storage.path)?
                        .stats
                        .message_count,
                    messages as u64
                );
                Ok(())
            },
        )?);
    }

    for count in [4, 16] {
        report.push(measure_with_setup(
            format!("clear_queue_{count}"),
            config,
            identity(&format!("clear-queue:{count}"), 4_096),
            parameter_map(&[("history_messages", 4_096), ("queued_messages", count)]),
            || {
                let fixture = RuntimeFixture::new(&executor, &seed, true)?;
                for index in 0..count {
                    fixture
                        .session()?
                        .enqueue_message(message(index), QueueKind::NextRun)?;
                }
                Ok(fixture)
            },
            |fixture| -> BenchResult<_> { Ok(fixture.session()?.clear_queue()?) },
            |fixture, cleared| {
                assert_eq!(cleared.follow_up.len(), count);
                assert!(fixture.session()?.snapshot().queue.follow_up.is_empty());
                let restored = executor.block_on(AgentSession::open(
                    runtime(fixture.storage.directory.path())?,
                    &fixture.storage.path,
                    options(),
                ))?;
                assert!(restored.snapshot().queue.follow_up.is_empty());
                executor.block_on(restored.shutdown());
                Ok(())
            },
        )?);
    }

    for count in [10, 1_000] {
        let directory = tempfile::tempdir()?;
        let repo = JsonlSessionRepo::new(directory.path().join("sessions"));
        let target = repo.allocate_new(directory.path())?;
        let parent = target.path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        for index in 0..count {
            let header = SessionHeader::new(format!("known-{index}"), directory.path());
            let mut file = File::create(parent.join(format!("renamed-{index}.jsonl")))?;
            serde_json::to_writer(&mut file, &header)?;
            file.write_all(b"\n")?;
        }
        report.push(measure_with_setup(
            format!("allocate_new_with_{count}_sessions"),
            config,
            identity(&format!("allocate:{count}"), 0),
            parameter_map(&[("session_files", count)]),
            || Ok(()),
            |_| -> BenchResult<_> { Ok(black_box(repo.allocate_new(directory.path())?)) },
            |_, target| {
                assert!(!target.path.exists());
                assert_eq!(target.path.parent(), Some(parent));
                Ok(())
            },
        )?);
        for id in [format!("known-{}", count - 1), "missing".into()] {
            report.push(measure_with_setup(
                format!("resolve_{id}_with_{count}_sessions"),
                config,
                identity(&format!("lookup:{count}:{id}"), 0),
                parameter_map(&[("session_files", count.to_string()), ("id", id.clone())]),
                || Ok(()),
                |_| -> BenchResult<_> {
                    Ok(black_box(repo.resolve_exact_id(directory.path(), &id)?))
                },
                |_, found| {
                    assert_eq!(
                        matches!(found, ExactSessionIdResolution::Existing(_)),
                        id != "missing"
                    );
                    Ok(())
                },
            )?);
        }
    }
    report.finish()
}

fn identity(operation: &str, history: usize) -> String {
    fixture_hash(format!(
        "session-io:v1:{operation}:{history}:{PAYLOAD_BYTES}:{TIMESTAMP_MS}"
    ))
}

fn message(index: usize) -> Message {
    Message::User(UserMessage::text(
        format!("history-{index}:{}", "x".repeat(PAYLOAD_BYTES)),
        index as i64,
    ))
}

fn message_entry(index: usize) -> SessionEntry {
    SessionEntry::message(message(index))
}

struct Seed(Vec<u8>);

impl Seed {
    fn new(history: usize) -> BenchResult<Self> {
        let mut bytes = Vec::new();
        let configuration = [
            SessionEntry::ModelChange(ModelChangeEntry {
                provider: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
            }),
            SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: "off".into(),
            }),
            SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                active_tool_names: Vec::new(),
            }),
        ];
        let entries = configuration
            .into_iter()
            .enumerate()
            .map(|(i, entry)| (format!("config-{i}"), entry))
            .chain((0..history).map(|i| (format!("message-{i}"), message_entry(i))));
        let mut parent_id = None;
        for (index, (id, entry)) in entries.enumerate() {
            let mutation = SessionMutation::Entry {
                lane: Some("main".into()),
                record: SessionRecord {
                    id: id.clone(),
                    seq: index as u64 + 1,
                    parent_id,
                    timestamp_ms: TIMESTAMP_MS,
                    entry,
                },
            };
            serde_json::to_writer(&mut bytes, &mutation)?;
            bytes.push(b'\n');
            parent_id = Some(id);
        }
        Ok(Self(bytes))
    }

    fn open(&self) -> BenchResult<StoreFixture> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("session.jsonl");
        let mut file = File::create(&path)?;
        let mut header = SessionHeader::new("session-io", directory.path());
        header.created_at = TIMESTAMP_MS;
        serde_json::to_writer(&mut file, &header)?;
        file.write_all(b"\n")?;
        file.write_all(&self.0)?;
        file.sync_all()?;
        drop(file);
        let store = SessionStore::open_handle(&path)?;
        Ok(StoreFixture {
            store,
            path,
            directory,
        })
    }
}

struct StoreFixture {
    store: SessionStore,
    path: PathBuf,
    directory: TempDir,
}

struct RuntimeFixture<'a> {
    executor: &'a tokio::runtime::Runtime,
    session: Option<Arc<AgentSession>>,
    runtime: Option<PiRuntime>,
    storage: StoreFixture,
}

impl<'a> RuntimeFixture<'a> {
    fn new(executor: &'a tokio::runtime::Runtime, seed: &Seed, opened: bool) -> BenchResult<Self> {
        let storage = seed.open()?;
        let runtime = Some(runtime(storage.directory.path())?);
        let mut fixture = Self {
            executor,
            storage,
            runtime,
            session: None,
        };
        if opened {
            fixture.open()?;
        }
        Ok(fixture)
    }

    fn open(&mut self) -> BenchResult {
        if let Some(runtime) = self.runtime.take() {
            self.session = Some(self.executor.block_on(AgentSession::open(
                runtime,
                &self.storage.path,
                options(),
            ))?);
        }
        Ok(())
    }

    fn session(&self) -> BenchResult<&AgentSession> {
        self.session
            .as_deref()
            .ok_or_else(|| "session not opened".into())
    }
}

impl Drop for RuntimeFixture<'_> {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            self.executor.block_on(session.shutdown());
        }
    }
}

fn options() -> AgentSessionOptions {
    AgentSessionOptions::default().compaction(CompactionSettings {
        enabled: false,
        ..Default::default()
    })
}

fn runtime(cwd: &Path) -> BenchResult<PiRuntime> {
    let mut builder = PiRuntime::builder()
        .agent_options(AgentOptions {
            cwd: cwd.into(),
            ..Default::default()
        })
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("warmup".into()),
            ScriptedTurn::Text("complete".into()),
        ]));
    for index in 0..4 {
        builder = builder.plugin(BoundaryHook(format!("hook-{index}")));
    }
    Ok(builder.build()?)
}

struct BoundaryHook(String);

#[pi_plugin::plugin]
impl Plugin for BoundaryHook {
    fn id(&self) -> PluginId {
        PluginId::new(&self.0)
    }

    async fn turn_end(
        &self,
        _: AgentPluginContext,
        event: TurnEndEvent,
    ) -> Result<BoundaryResult, PluginError> {
        black_box(
            event
                .boundary
                .as_ref()
                .map(|boundary| boundary.context.context_entries.len()),
        );
        Ok(BoundaryResult::default())
    }
}
