use std::hint::black_box;

use pi_bench::{BenchConfig, BenchResult, BenchmarkReport, fixture_hash, measure, parameter_map};
use pi_core::{Message, UserMessage};
use pi_session::{SessionEntry, SessionHeader, SessionLog};
use tempfile::TempDir;

fn main() -> BenchResult<()> {
    let config = BenchConfig::from_env(5, 50)?;
    let mut report = BenchmarkReport::new("session_commit");

    for initial_entries in [64, 1_024, 4_096, 16_384] {
        let mut fixture = DeferredCommitFixture::create(initial_entries, 128)?;
        let parameters = parameter_map(&[
            ("initial_entries", initial_entries.to_string()),
            ("payload_bytes", fixture.payload_bytes.to_string()),
            ("persistence", "deferred".to_string()),
        ]);
        report.push(measure(
            format!("append_after_{initial_entries}_entries"),
            config,
            fixture_hash(format!(
                "session-commit:v1:{initial_entries}:{}",
                fixture.payload_bytes
            )),
            parameters,
            || fixture.append(),
        )?);
        fixture.verify(config.warmup_iterations + config.sample_iterations)?;
        report.push(measure(
            format!("owned_snapshot_after_{initial_entries}_entries"),
            config,
            fixture_hash(format!(
                "session-snapshot:v1:{initial_entries}:{}",
                fixture.payload_bytes
            )),
            parameter_map(&[
                ("initial_entries", initial_entries.to_string()),
                ("payload_bytes", fixture.payload_bytes.to_string()),
                ("persistence", "deferred".to_string()),
            ]),
            || fixture.snapshot(),
        )?);
        report.push(measure(
            format!("shared_snapshot_after_{initial_entries}_entries"),
            config,
            fixture_hash(format!(
                "session-shared-snapshot:v1:{initial_entries}:{}",
                fixture.payload_bytes
            )),
            parameter_map(&[
                ("initial_entries", initial_entries.to_string()),
                ("payload_bytes", fixture.payload_bytes.to_string()),
                ("snapshot", "shared-warm".to_string()),
            ]),
            || fixture.shared_snapshot(),
        )?);
        let mut refresh_fixture = DeferredCommitFixture::create(initial_entries, 128)?;
        report.push(measure(
            format!("refresh_shared_snapshot_after_{initial_entries}_entries"),
            config,
            fixture_hash(format!(
                "session-refresh-shared-snapshot:v1:{initial_entries}:{}",
                refresh_fixture.payload_bytes
            )),
            parameter_map(&[
                ("initial_entries", initial_entries.to_string()),
                ("payload_bytes", refresh_fixture.payload_bytes.to_string()),
                ("snapshot", "shared-after-mutation".to_string()),
            ]),
            || refresh_fixture.append_and_shared_snapshot(),
        )?);
    }

    report.finish()
}

struct DeferredCommitFixture {
    _directory: TempDir,
    log: SessionLog,
    initial_entries: usize,
    appended_entries: usize,
    payload: String,
    payload_bytes: usize,
}

impl DeferredCommitFixture {
    fn create(initial_entries: usize, payload_bytes: usize) -> BenchResult<Self> {
        let directory = tempfile::tempdir()?;
        let log = SessionLog::create_deferred(
            directory.path().join("session.jsonl"),
            SessionHeader::new("bench-commit", directory.path()),
        )?;
        let payload = "x".repeat(payload_bytes);
        log.append_batch((0..initial_entries).map(|index| {
            SessionEntry::message(Message::User(UserMessage::text(
                format!("seed-{index}:{payload}"),
                i64::try_from(index).unwrap_or(i64::MAX),
            )))
        }))?;
        Ok(Self {
            _directory: directory,
            log,
            initial_entries,
            appended_entries: 0,
            payload,
            payload_bytes,
        })
    }

    fn append(&mut self) -> Result<(), String> {
        let index = self.appended_entries;
        let id = self
            .log
            .append_message(Message::User(UserMessage::text(
                format!("append-{index}:{}", self.payload),
                i64::try_from(self.initial_entries + index).unwrap_or(i64::MAX),
            )))
            .map_err(|error| error.to_string())?;
        self.appended_entries += 1;
        black_box(id);
        Ok(())
    }

    fn verify(&self, expected_appends: usize) -> Result<(), String> {
        if self.appended_entries != expected_appends {
            return Err(format!(
                "appended {} entries, expected {expected_appends}",
                self.appended_entries
            ));
        }
        let expected_messages = u64::try_from(self.initial_entries + expected_appends)
            .map_err(|error| error.to_string())?;
        let actual_messages = self.log.stats().message_count;
        if actual_messages != expected_messages {
            return Err(format!(
                "session contains {actual_messages} messages, expected {expected_messages}"
            ));
        }
        Ok(())
    }

    fn snapshot(&self) -> Result<(), String> {
        black_box(self.log.load().map_err(|error| error.to_string())?);
        Ok(())
    }

    fn shared_snapshot(&self) -> Result<(), String> {
        black_box(
            self.log
                .shared_document()
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    fn append_and_shared_snapshot(&mut self) -> Result<(), String> {
        self.append()?;
        self.shared_snapshot()
    }
}
