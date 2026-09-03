use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("cannot read evaluation data {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write evaluation report {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}{line_suffix}: {source}", line_suffix = line.map_or(String::new(), |line| format!(" at line {line}")))]
    Json {
        path: PathBuf,
        line: Option<usize>,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid evaluation corpus: {0}")]
    InvalidCorpus(String),
    #[error("invalid command line: {0}")]
    Cli(String),
    #[error("memory provider setup failed: {0}")]
    Provider(String),
}

impl EvalError {
    pub(crate) fn read(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Read {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidCorpus(message.into())
    }
}
