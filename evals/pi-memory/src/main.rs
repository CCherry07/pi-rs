use std::path::PathBuf;
use std::time::Duration;

use pi_memory_eval::{
    EvalCorpus, EvalError, EvalReport, EvalRunner, NoRecallBackend, OracleBackend, RunnerConfig,
    SqliteProviderBackend,
};
use pi_plugin_memory_local::{FastEmbedModelStore, LocalMemoryProvider, SqliteRecallRanking};

const HELP: &str = "\
Deterministic retrieval evaluation for pi-memory

Usage:
  pi-memory-eval [options]

Options:
  --backend <name>                     sqlite, sqlite-bm25, sqlite-dense, sqlite-dense-raw-rrf,
                                       no-recall, or oracle
  --suite <name>                       Evaluation suite (default: smoke)
  --tier <name>                        Deprecated alias for --suite
  --fixtures <directory>               Corpus directory (default: bundled v1)
  --timeout-ms <milliseconds>          Per-query timeout (default: 50)
  --database <path>                    SQLite path (default: temporary database)
  --embedding-cache <directory>        Installed model cache (required by sqlite-dense)
  --report <path>                      Write pretty JSON instead of printing it
  -h, --help                           Show this help
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Sqlite,
    SqliteBm25,
    SqliteDense,
    SqliteDenseRawRrf,
    NoRecall,
    Oracle,
}

impl BackendKind {
    fn parse(value: &str) -> Result<Self, EvalError> {
        match value {
            "sqlite" => Ok(Self::Sqlite),
            "sqlite-bm25" => Ok(Self::SqliteBm25),
            "sqlite-dense" => Ok(Self::SqliteDense),
            "sqlite-dense-raw-rrf" => Ok(Self::SqliteDenseRawRrf),
            "no-recall" => Ok(Self::NoRecall),
            "oracle" => Ok(Self::Oracle),
            _ => Err(EvalError::Cli(format!(
                "unknown backend {value:?}; expected sqlite, sqlite-bm25, sqlite-dense, sqlite-dense-raw-rrf, no-recall, or oracle"
            ))),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::SqliteBm25 => "sqlite-bm25",
            Self::SqliteDense => "sqlite-dense",
            Self::SqliteDenseRawRrf => "sqlite-dense-raw-rrf",
            Self::NoRecall => "no-recall",
            Self::Oracle => "oracle",
        }
    }

    const fn uses_dense(self) -> bool {
        matches!(self, Self::SqliteDense | Self::SqliteDenseRawRrf)
    }

    const fn uses_sqlite(self) -> bool {
        matches!(
            self,
            Self::Sqlite | Self::SqliteBm25 | Self::SqliteDense | Self::SqliteDenseRawRrf
        )
    }
}

#[derive(Debug)]
struct Cli {
    backend: BackendKind,
    suite: String,
    fixtures: Option<PathBuf>,
    timeout_ms: u64,
    database: Option<PathBuf>,
    embedding_cache: Option<PathBuf>,
    report: Option<PathBuf>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            backend: BackendKind::Sqlite,
            suite: "smoke".to_string(),
            fixtures: None,
            timeout_ms: 50,
            database: None,
            embedding_cache: None,
            report: None,
        }
    }
}

impl Cli {
    fn parse() -> Result<Option<Self>, EvalError> {
        Self::parse_from(std::env::args().skip(1))
    }

    fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Option<Self>, EvalError> {
        let mut cli = Self::default();
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "-h" | "--help" => return Ok(None),
                "--backend" => {
                    cli.backend = BackendKind::parse(&next_value(&mut args, &argument)?)?
                }
                "--suite" | "--tier" => cli.suite = next_value(&mut args, &argument)?,
                "--fixtures" => cli.fixtures = Some(next_value(&mut args, &argument)?.into()),
                "--timeout-ms" => {
                    let value = next_value(&mut args, &argument)?;
                    cli.timeout_ms = value.parse().map_err(|_| {
                        EvalError::Cli(format!("--timeout-ms expects an integer, got {value:?}"))
                    })?;
                    if cli.timeout_ms == 0 {
                        return Err(EvalError::Cli(
                            "--timeout-ms must be greater than zero".to_string(),
                        ));
                    }
                }
                "--database" => cli.database = Some(next_value(&mut args, &argument)?.into()),
                "--embedding-cache" => {
                    cli.embedding_cache = Some(next_value(&mut args, &argument)?.into())
                }
                "--report" => cli.report = Some(next_value(&mut args, &argument)?.into()),
                _ => {
                    return Err(EvalError::Cli(format!(
                        "unknown argument {argument:?}; use --help"
                    )));
                }
            }
        }
        if cli.database.is_some() && !cli.backend.uses_sqlite() {
            return Err(EvalError::Cli(
                "--database is only valid with a SQLite backend".to_string(),
            ));
        }
        if cli.backend.uses_dense() && cli.embedding_cache.is_none() {
            return Err(EvalError::Cli(format!(
                "--embedding-cache is required with --backend {}",
                cli.backend.name()
            )));
        }
        if !cli.backend.uses_dense() && cli.embedding_cache.is_some() {
            return Err(EvalError::Cli(
                "--embedding-cache is only valid with a dense backend".to_string(),
            ));
        }
        Ok(Some(cli))
    }
}

#[tokio::main]
async fn main() {
    match Cli::parse() {
        Ok(None) => println!("{HELP}"),
        Ok(Some(cli)) => {
            if let Err(error) = run(cli).await {
                eprintln!("error: {error}");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}

async fn run(cli: Cli) -> Result<(), EvalError> {
    let corpus = match &cli.fixtures {
        Some(path) => EvalCorpus::load(path)?,
        None => EvalCorpus::bundled()?,
    };
    let suite = corpus.suite(&cli.suite)?;
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(cli.timeout_ms),
    });
    let report = match cli.backend {
        BackendKind::NoRecall => {
            runner
                .run(cli.backend.name(), &suite, &NoRecallBackend)
                .await
        }
        BackendKind::Oracle => {
            let backend = OracleBackend::new(&suite);
            runner.run(cli.backend.name(), &suite, &backend).await
        }
        BackendKind::Sqlite
        | BackendKind::SqliteBm25
        | BackendKind::SqliteDense
        | BackendKind::SqliteDenseRawRrf => run_sqlite(&cli, &suite, &runner).await?,
    };
    output_report(&report, cli.report.as_ref())
}

async fn run_sqlite(
    cli: &Cli,
    suite: &pi_memory_eval::EvalSuite,
    runner: &EvalRunner,
) -> Result<EvalReport, EvalError> {
    let temporary_directory = if cli.database.is_none() {
        Some(tempfile::tempdir().map_err(|error| {
            EvalError::Provider(format!("cannot create temporary directory: {error}"))
        })?)
    } else {
        None
    };
    let database = cli.database.clone().unwrap_or_else(|| {
        temporary_directory
            .as_ref()
            .expect("temporary directory exists")
            .path()
            .join("memory.sqlite3")
    });
    let provider = match cli.backend {
        BackendKind::Sqlite => {
            LocalMemoryProvider::open_with_ranking(&database, SqliteRecallRanking::Hybrid)
        }
        BackendKind::SqliteBm25 => {
            LocalMemoryProvider::open_with_ranking(&database, SqliteRecallRanking::Bm25)
        }
        BackendKind::SqliteDense | BackendKind::SqliteDenseRawRrf => {
            let cache = cli
                .embedding_cache
                .as_ref()
                .expect("validated dense embedding cache");
            let store = FastEmbedModelStore::new(cache);
            let embedder = store
                .embedder_if_ready()
                .map_err(|error| EvalError::Provider(error.to_string()))?
                .ok_or_else(|| {
                    EvalError::Provider(format!(
                        "embedding model is not installed in {}",
                        cache.display()
                    ))
                })?;
            let ranking = match cli.backend {
                BackendKind::SqliteDense => SqliteRecallRanking::SparseDenseRrf,
                BackendKind::SqliteDenseRawRrf => SqliteRecallRanking::SparseDenseRawRrf,
                _ => unreachable!("matched dense backend"),
            };
            LocalMemoryProvider::open_with_embedder_and_ranking(&database, embedder, ranking)
        }
        BackendKind::NoRecall | BackendKind::Oracle => unreachable!("validated SQLite backend"),
    };
    let provider = provider.map_err(|error| EvalError::Provider(error.to_string()))?;
    provider
        .apply(suite.mutations())
        .await
        .map_err(|error| EvalError::Provider(error.to_string()))?;
    let backend = SqliteProviderBackend::new(provider);
    Ok(runner.run(cli.backend.name(), suite, &backend).await)
}

fn output_report(report: &EvalReport, path: Option<&PathBuf>) -> Result<(), EvalError> {
    if let Some(path) = path {
        report.write_json(path)?;
        println!(
            "wrote {} cases to {} (recall@5 {:.3}, p95 {:.2} ms)",
            report.summary.total_cases,
            path.display(),
            report.summary.retrieval.recall_at_5,
            report.summary.latency_ms.p95,
        );
    } else {
        let json = serde_json::to_string_pretty(report).map_err(|source| EvalError::Json {
            path: PathBuf::from("<stdout>"),
            line: None,
            source,
        })?;
        println!("{json}");
    }
    Ok(())
}

fn next_value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, EvalError> {
    args.next()
        .ok_or_else(|| EvalError::Cli(format!("{option} requires a value")))
}

#[cfg(test)]
mod tests {
    use super::{BackendKind, Cli};

    fn parse(arguments: &[&str]) -> Result<Option<Cli>, pi_memory_eval::EvalError> {
        Cli::parse_from(arguments.iter().map(ToString::to_string))
    }

    #[test]
    fn dense_backend_requires_and_accepts_an_explicit_model_cache() {
        let missing = parse(&["--backend", "sqlite-dense"])
            .expect_err("dense backend without a cache must fail");
        assert!(
            missing
                .to_string()
                .contains("--embedding-cache is required")
        );

        let cli = parse(&[
            "--backend",
            "sqlite-dense",
            "--embedding-cache",
            "/tmp/model-cache",
            "--database",
            "/tmp/eval.sqlite3",
        ])
        .expect("valid dense arguments")
        .expect("run requested");
        assert_eq!(cli.backend, BackendKind::SqliteDense);
        assert_eq!(
            cli.embedding_cache.as_deref(),
            Some(std::path::Path::new("/tmp/model-cache"))
        );
    }

    #[test]
    fn raw_rrf_ablation_is_an_explicit_dense_backend() {
        let cli = parse(&[
            "--backend",
            "sqlite-dense-raw-rrf",
            "--embedding-cache",
            "/tmp/model-cache",
        ])
        .expect("valid raw RRF arguments")
        .expect("run requested");

        assert_eq!(cli.backend, BackendKind::SqliteDenseRawRrf);
        assert_eq!(cli.backend.name(), "sqlite-dense-raw-rrf");
    }

    #[test]
    fn model_cache_is_rejected_for_non_dense_backends() {
        let error = parse(&["--embedding-cache", "/tmp/model-cache"])
            .expect_err("lexical backend must not silently ignore the model cache");
        assert!(
            error
                .to_string()
                .contains("--embedding-cache is only valid")
        );
    }
}
