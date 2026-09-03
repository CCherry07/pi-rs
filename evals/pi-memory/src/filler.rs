use pi_plugin_memory_local::{
    MemoryEvidence, MemoryKind, MemoryMutation, MemoryOrigin, MemoryRecord, MemoryScope,
};

use crate::EvalSession;

const SUBSYSTEMS: &[&str] = &[
    "telemetry",
    "documentation",
    "package-index",
    "preview-ui",
    "migration-lab",
    "release-dashboard",
    "analytics-export",
    "edge-proxy",
    "fixture-builder",
    "compatibility-suite",
    "incident-console",
    "benchmark-runner",
    "schema-browser",
    "audit-reporter",
    "example-generator",
    "sandbox-runner",
    "artifact-viewer",
    "deployment-mirror",
    "developer-portal",
    "trace-inspector",
    "archive-reader",
    "workflow-designer",
    "status-widget",
    "test-fixture",
];

const CRATES: &[&str] = &[
    "pi-telemetry",
    "pi-md",
    "pi-provider",
    "pi-rpc",
    "pi-resources",
    "pi-shell",
    "pi-test-support",
    "pi-tool-support",
];

const REGIONS: &[&str] = &[
    "us-west-2",
    "ap-southeast-1",
    "eu-central-1",
    "us-east-2",
    "eu-west-2",
];

const OTHER_PROJECTS: &[&str] = &[
    "apollo", "beacon", "comet", "delta", "ember", "fjord", "glacier", "harbor",
];

pub(crate) fn generate_filler_sessions(
    seed: u64,
    haystack: &str,
    count: usize,
) -> Vec<EvalSession> {
    let mut random = StableRandom::new(seed ^ stable_text_hash(haystack));
    (0..count)
        .map(|index| {
            let session_id = format!("generated-{haystack}-{index:03}");
            let subsystem = choose(SUBSYSTEMS, random.next());
            let crate_name = choose(CRATES, random.next());
            let region = choose(REGIONS, random.next());
            let project_topic = (random.next() % 12) as usize;
            let user_topic = (random.next() % 6) as usize;
            let other_project = choose(OTHER_PROJECTS, random.next());
            let port = 3_100 + random.next() % 700;
            let project = project_filler(project_topic, subsystem, crate_name, region, port);
            let user = user_filler(user_topic, subsystem);
            let other = other_project_filler(other_project, subsystem, region);
            EvalSession {
                id: session_id.clone(),
                mutations: vec![
                    remember(
                        &session_id,
                        index,
                        0,
                        MemoryScope::Project {
                            root: "/workspace/atlas".to_string(),
                        },
                        project.0,
                        project.1,
                        seed,
                    ),
                    remember(
                        &session_id,
                        index,
                        1,
                        MemoryScope::User,
                        user.0,
                        user.1,
                        seed,
                    ),
                    remember(
                        &session_id,
                        index,
                        2,
                        MemoryScope::Project {
                            root: format!("/workspace/{other_project}"),
                        },
                        other.0,
                        other.1,
                        seed,
                    ),
                ],
            }
        })
        .collect()
}

fn remember(
    session_id: &str,
    session_index: usize,
    record_index: usize,
    scope: MemoryScope,
    kind: MemoryKind,
    text: String,
    seed: u64,
) -> MemoryMutation {
    let record_id = format!("{session_id}-record-{record_index}");
    MemoryMutation::Remember {
        mutation_id: format!("{session_id}-mutation-{record_index}"),
        record: MemoryRecord {
            id: record_id,
            scope,
            kind,
            text,
            origin: MemoryOrigin {
                session_id: session_id.to_string(),
                entry_id: Some(format!("entry-{record_index}")),
                tool_call_id: None,
            },
            evidence: MemoryEvidence {
                note: format!("Deterministic evaluation filler generated from seed {seed}."),
            },
            recorded_at_ms: 10_000 + session_index as i64 * 10 + record_index as i64,
            supersedes: None,
        },
    }
}

fn project_filler(
    topic: usize,
    subsystem: &str,
    crate_name: &str,
    region: &str,
    port: u64,
) -> (MemoryKind, String) {
    match topic {
        0 => (
            MemoryKind::Instruction,
            format!(
                "Atlas {subsystem} test command runs cargo test -p {crate_name} inside the workspace; it is limited to that subsystem."
            ),
        ),
        1 => (
            MemoryKind::Summary,
            format!(
                "Atlas {subsystem} release dashboard tracks workspace tests, signed artifacts, staging, production, and approval status; it does not define deployment steps."
            ),
        ),
        2 => (
            MemoryKind::Fact,
            format!(
                "Atlas {subsystem} deployment mirror is hosted in {region}; this region applies only to {subsystem}."
            ),
        ),
        3 => (
            MemoryKind::Summary,
            format!(
                "Atlas {subsystem} cache compatibility suite compares Redis 6 with Valkey 8; it does not select the primary cache."
            ),
        ),
        4 => (
            MemoryKind::Fact,
            format!(
                "Atlas {subsystem} examples call local memory recall while testing semantic retrieval."
            ),
        ),
        5 => (
            MemoryKind::Summary,
            format!(
                "Atlas {subsystem} rollback drill restores a sample artifact and runs smoke tests without changing production."
            ),
        ),
        6 => (
            MemoryKind::Instruction,
            format!(
                "Atlas {subsystem} macOS guide uses BSD sed -E and notes that GNU sed -r is unavailable."
            ),
        ),
        7 => (
            MemoryKind::Fact,
            format!(
                "Atlas {subsystem} SQLite FTS5 fixture demonstrates MATCH queries containing punctuation and raw syntax."
            ),
        ),
        8 => (
            MemoryKind::Instruction,
            format!(
                "Atlas {subsystem} parallel test fixture reserves port {port}; it cannot use the product's ephemeral listener."
            ),
        ),
        9 => (
            MemoryKind::Fact,
            format!(
                "Atlas {subsystem} UI has a deploy button, but this is not an agent tool registration."
            ),
        ),
        10 => (
            MemoryKind::Fact,
            format!(
                "Atlas {subsystem} export pipeline writes analytics to PostgreSQL; semantic memory storage is unrelated."
            ),
        ),
        _ => (
            MemoryKind::Summary,
            format!(
                "Atlas {subsystem} migration report lists MemoryRecord rows with supersedes fields but does not define the journal correction procedure."
            ),
        ),
    }
}

fn user_filler(topic: usize, subsystem: &str) -> (MemoryKind, String) {
    match topic {
        0 => (
            MemoryKind::Preference,
            format!(
                "For {subsystem} interoperability examples only, use Python; this does not change the global code-example language."
            ),
        ),
        1 => (
            MemoryKind::Instruction,
            format!(
                "Security audit answers for {subsystem} require detailed appendices; normal response style remains unchanged."
            ),
        ),
        2 => (
            MemoryKind::Instruction,
            format!(
                "Keep {subsystem} chat titles concise; this rule applies to titles rather than answer style."
            ),
        ),
        3 => (
            MemoryKind::Preference,
            format!(
                "Legacy {subsystem} documentation preserves Java examples verbatim; it is not a preference for new code examples."
            ),
        ),
        4 => (
            MemoryKind::Instruction,
            format!(
                "The {subsystem} changelog needs a detailed explanation of migrations; ordinary answers remain concise."
            ),
        ),
        _ => (
            MemoryKind::Preference,
            format!(
                "The {subsystem} generated fixture uses two-space indentation; user-facing code formatting is unaffected."
            ),
        ),
    }
}

fn other_project_filler(project: &str, subsystem: &str, region: &str) -> (MemoryKind, String) {
    (
        MemoryKind::Summary,
        format!(
            "{project} {subsystem} release tests use PostgreSQL, Redis, and a deployment mirror in {region}."
        ),
    )
}

fn choose(values: &'static [&'static str], random: u64) -> &'static str {
    values[(random as usize) % values.len()]
}

fn stable_text_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

struct StableRandom {
    state: u64,
}

impl StableRandom {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_seeded_and_stable() {
        let first = generate_filler_sessions(24_052_026, "small", 2);
        let second = generate_filler_sessions(24_052_026, "small", 2);
        assert_eq!(first, second);
        assert_eq!(first[0].id, "generated-small-000");
        assert_ne!(generate_filler_sessions(24_052_027, "small", 2), first);
    }
}
