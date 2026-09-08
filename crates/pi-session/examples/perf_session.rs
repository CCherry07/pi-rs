//! Deterministic performance worker used by scripts/bench-perf.
//! This measures public owned-snapshot APIs, not a structure-only storage API.
use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::time::Instant;

use pi_core::{Message, UserMessage};
use pi_session::{
    BranchQuery, EntryQuery, ForkOptions, InMemorySession, InMemorySessionRepo, MAIN_LANE,
    ProvisionedEntry, SessionCreateOptions, SessionEntry, SessionMetadata,
};
use serde_json::json;

struct Config {
    mode: String,
    size: usize,
    samples: usize,
    warmup: usize,
    batch: usize,
    payload: usize,
}

impl Config {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() != 6 {
            return Err("expected: MODE SIZE SAMPLES WARMUP BATCH PAYLOAD_BYTES".into());
        }
        let config = Self {
            mode: args[0].clone(),
            size: args[1].parse()?,
            samples: args[2].parse()?,
            warmup: args[3].parse()?,
            batch: args[4].parse()?,
            payload: args[5].parse()?,
        };
        if config.size == 0 || config.samples == 0 || config.batch == 0 || config.payload < 32 {
            return Err("size, samples and batch must be positive; payload must be >= 32".into());
        }
        Ok(config)
    }
}

fn entry_id(index: usize) -> String {
    format!("benchmark-entry-{index:08}")
}

fn storage() -> InMemorySession {
    InMemorySession::new(SessionMetadata {
        id: "benchmark-session-00000000".into(),
        created_at: 1_700_000_000_000,
        parent_session_id: None,
    })
}

fn seed(session: &InMemorySession, config: &Config) -> Result<(), Box<dyn Error>> {
    for index in 0..config.size {
        let id = entry_id(index);
        let prefix = format!("{id}:");
        let text = format!("{prefix}{}", "x".repeat(config.payload - prefix.len()));
        session.append_entry(
            ProvisionedEntry {
                id,
                entry: SessionEntry::message(Message::User(UserMessage::text(
                    text,
                    1_650_000_000_000,
                ))),
            },
            MAIN_LANE,
        )?;
    }
    Ok(())
}

fn emit(name: &str, batch: usize, samples: Vec<f64>) {
    println!(
        "{}",
        json!({"name": name, "batch": batch, "samples": samples, "unit": "ms"})
    );
}

fn measure(
    config: &Config,
    name: &str,
    batch: usize,
    expected: usize,
    mut run: impl FnMut() -> usize,
) {
    assert_eq!(run(), expected, "{name}: incorrect fixture/result");
    for _ in 0..config.warmup {
        for _ in 0..batch {
            black_box(run());
        }
    }
    let mut samples = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let start = Instant::now();
        for _ in 0..batch {
            black_box(run());
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.0 / batch as f64);
    }
    emit(name, batch, samples);
}

fn read(config: &Config) -> Result<(), Box<dyn Error>> {
    let session = storage();
    seed(&session, config)?;
    let lookup_count = config.size.min(100);
    let ids: Vec<_> = (0..lookup_count)
        .map(|index| entry_id(index * (config.size - 1) / (lookup_count - 1).max(1)))
        .collect();
    measure(config, "get100", config.batch, lookup_count, || {
        ids.iter()
            .filter(|id| session.get_entry(id).is_some())
            .count()
    });
    let latest = EntryQuery {
        limit: Some(50),
        ..EntryQuery::default()
    };
    measure(
        config,
        "latest50",
        config.batch,
        config.size.min(50),
        || session.find_entries(&latest).unwrap().len(),
    );
    let branch = BranchQuery::default();
    measure(config, "full_branch", 1, config.size, || {
        session.find_entries_on_branch(&branch).unwrap().len()
    });
    Ok(())
}

fn fork(config: &Config) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..config.warmup + config.samples {
        let repo = InMemorySessionRepo::new();
        let source = repo.create(SessionCreateOptions {
            id: Some("benchmark-session-00000000".into()),
            ..SessionCreateOptions::default()
        })?;
        seed(source.storage(), config)?;
        let metadata = source.metadata()?;
        let options = ForkOptions::default();
        let create = SessionCreateOptions {
            id: Some("benchmark-session-00000001".into()),
            ..SessionCreateOptions::default()
        };
        let start = Instant::now();
        let fork = repo.fork(&metadata, &options, create)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(fork.get_stats().message_count, config.size as u64);
        assert_eq!(fork.get_leaf_id()?, Some(entry_id(config.size - 1)));
        assert_eq!(fork.metadata()?.parent_session_id, Some(metadata.id));
        if iteration >= config.warmup {
            samples.push(elapsed);
        }
    }
    emit("fork", 1, samples);
    Ok(())
}

fn catalog(config: &Config) -> Result<(), Box<dyn Error>> {
    {
        let repo = InMemorySessionRepo::new();
        for index in 0..config.size {
            repo.create(SessionCreateOptions {
                id: Some(format!("benchmark-session-{index:08}")),
                ..SessionCreateOptions::default()
            })?;
        }
        measure(config, "list_sessions", 1, config.size, || {
            repo.list().len()
        });
    }
    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..config.warmup + config.samples {
        let repo = InMemorySessionRepo::new();
        let options = SessionCreateOptions {
            id: Some("benchmark-session-00000000".into()),
            ..SessionCreateOptions::default()
        };
        let start = Instant::now();
        let session = repo.create(options)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(session.metadata()?.id, "benchmark-session-00000000");
        if iteration >= config.warmup {
            samples.push(elapsed);
        }
    }
    emit("create_session", 1, samples);
    Ok(())
}

fn checkpoint(phase: &str) -> Result<(), Box<dyn Error>> {
    println!("{}", json!({"phase": phase}));
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Err("memory measurement requires checkpoint acknowledgements on stdin".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let config = Config::parse()?;
    match config.mode.as_str() {
        "read" => read(&config),
        "fork" => fork(&config),
        "catalog" => catalog(&config),
        "memory" => {
            let session = storage();
            checkpoint("baseline")?;
            seed(&session, &config)?;
            assert_eq!(session.stats().message_count, config.size as u64);
            checkpoint("loaded")?;
            black_box(session);
            Ok(())
        }
        _ => Err("mode must be read, fork, catalog or memory".into()),
    }
}
