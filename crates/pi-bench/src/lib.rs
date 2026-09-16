#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Display;
use std::hint::black_box;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub const REPORT_SCHEMA: &str = "pi.bench.v1";

pub type BenchResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchConfig {
    pub warmup_iterations: usize,
    pub sample_iterations: usize,
}

impl BenchConfig {
    pub fn from_env(default_warmup: usize, default_samples: usize) -> BenchResult<Self> {
        Ok(Self {
            warmup_iterations: parse_count("PI_BENCH_WARMUP", default_warmup)?,
            sample_iterations: parse_count("PI_BENCH_SAMPLES", default_samples)?,
        })
    }
}

fn parse_count(name: &str, default: usize) -> BenchResult<usize> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .map_err(|error| format!("{name} must be a positive integer: {error}"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(format!("could not read {name}: {error}").into()),
    };
    if value == 0 {
        return Err(format!("{name} must be greater than zero").into());
    }
    Ok(value)
}

#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub schema: &'static str,
    pub suite: String,
    pub generated_at_unix_ms: u128,
    pub environment: BenchmarkEnvironment,
    pub cases: Vec<BenchmarkCase>,
}

#[derive(Debug, Serialize)]
pub struct BenchmarkEnvironment {
    pub git_revision: Option<String>,
    pub git_dirty: Option<bool>,
    pub rustc: Option<String>,
    pub target: String,
    pub profile: &'static str,
}

#[derive(Debug, Serialize)]
pub struct BenchmarkCase {
    pub name: String,
    pub unit: &'static str,
    pub warmup_iterations: usize,
    pub sample_iterations: usize,
    pub fixture_hash: String,
    pub parameters: BTreeMap<String, String>,
    pub stats: DurationStats,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DurationStats {
    pub min_ns: u64,
    pub mean_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl BenchmarkReport {
    pub fn new(suite: impl Into<String>) -> Self {
        Self {
            schema: REPORT_SCHEMA,
            suite: suite.into(),
            generated_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            environment: BenchmarkEnvironment::capture(),
            cases: Vec::new(),
        }
    }

    pub fn push(&mut self, case: BenchmarkCase) {
        self.cases.push(case);
    }

    pub fn finish(self) -> BenchResult<()> {
        let encoded = serde_json::to_string_pretty(&self)?;
        println!("{encoded}");
        if let Some(output_dir) = std::env::var_os("PI_BENCH_OUTPUT") {
            let output_dir = PathBuf::from(output_dir);
            std::fs::create_dir_all(&output_dir)?;
            let path = output_dir.join(format!("{}.json", sanitize_name(&self.suite)));
            std::fs::write(&path, format!("{encoded}\n"))?;
            eprintln!("wrote benchmark report to {}", path.display());
        }
        Ok(())
    }
}

impl BenchmarkEnvironment {
    fn capture() -> Self {
        Self {
            git_revision: command_stdout("git", &["rev-parse", "HEAD"]),
            git_dirty: git_dirty(),
            rustc: command_stdout("rustc", &["--version"]),
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    }
}

fn command_stdout(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn git_dirty() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

pub fn measure<E>(
    name: impl Into<String>,
    config: BenchConfig,
    fixture_hash: impl Into<String>,
    parameters: BTreeMap<String, String>,
    mut operation: impl FnMut() -> Result<(), E>,
) -> BenchResult<BenchmarkCase>
where
    E: Display,
{
    for _ in 0..config.warmup_iterations {
        black_box(operation()).map_err(|error| format!("benchmark warmup failed: {error}"))?;
    }

    let mut samples = Vec::with_capacity(config.sample_iterations);
    for _ in 0..config.sample_iterations {
        let started = Instant::now();
        black_box(operation()).map_err(|error| format!("benchmark sample failed: {error}"))?;
        samples.push(duration_ns(started.elapsed().as_nanos()));
    }

    Ok(BenchmarkCase {
        name: name.into(),
        unit: "ns",
        warmup_iterations: config.warmup_iterations,
        sample_iterations: config.sample_iterations,
        fixture_hash: fixture_hash.into(),
        parameters,
        stats: duration_stats(&mut samples),
    })
}

fn duration_ns(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn duration_stats(samples: &mut [u64]) -> DurationStats {
    samples.sort_unstable();
    let sum = samples.iter().map(|value| u128::from(*value)).sum::<u128>();
    DurationStats {
        min_ns: samples[0],
        mean_ns: duration_ns(sum / samples.len() as u128),
        p50_ns: percentile(samples, 50),
        p95_ns: percentile(samples, 95),
        p99_ns: percentile(samples, 99),
        max_ns: samples[samples.len() - 1],
    }
}

fn percentile(sorted_samples: &[u64], percentile: usize) -> u64 {
    let rank = percentile
        .saturating_mul(sorted_samples.len())
        .div_ceil(100);
    sorted_samples[rank.saturating_sub(1).min(sorted_samples.len() - 1)]
}

pub fn fixture_hash(bytes: impl AsRef<[u8]>) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bytes.as_ref().iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}

pub fn parameter_map(values: &[(&str, impl Display)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.to_string()))
        .collect()
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_are_stable() {
        let mut samples = (1..=100).collect::<Vec<_>>();
        assert_eq!(
            duration_stats(&mut samples),
            DurationStats {
                min_ns: 1,
                mean_ns: 50,
                p50_ns: 50,
                p95_ns: 95,
                p99_ns: 99,
                max_ns: 100,
            }
        );
    }

    #[test]
    fn fixture_hash_is_deterministic() {
        assert_eq!(fixture_hash("pi"), fixture_hash("pi"));
        assert_ne!(fixture_hash("pi"), fixture_hash("Pi"));
    }

    #[test]
    fn report_schema_serializes_with_required_identity() {
        let report = BenchmarkReport::new("smoke");
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema"], REPORT_SCHEMA);
        assert_eq!(value["suite"], "smoke");
        assert!(value["environment"]["target"].is_string());
    }
}
