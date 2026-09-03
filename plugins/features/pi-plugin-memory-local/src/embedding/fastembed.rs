#![forbid(unsafe_code)]

//! Pinned local embedding-model assets and the FastEmbed memory adapter.
//!
//! Model installation is provider-managed. Offline initialization and ordinary
//! embedding never access the network; automatic initialization and the
//! explicit management command may acquire the pinned assets before an
//! adapter is published.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use fs2::FileExt;
use hf_hub::api::sync::ApiBuilder;
use hf_hub::{Cache, Repo, RepoType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{EmbeddingDescriptor, EmbeddingError, EmbeddingPurpose, MemoryEmbedder};

const MODEL_ID: &str = "intfloat/multilingual-e5-small";
const MODEL_REVISION: &str = "0e60b8d9d2166d80387f86e3b48ec9ced55f4d15";
const EMBEDDING_REVISION: &str = "0e60b8d9d2166d80387f86e3b48ec9ced55f4d15+pi-fastembed-v1";
const DIMENSIONS: usize = 384;
const MAX_LENGTH: usize = 512;
const EMBEDDING_BATCH_SIZE: usize = 32;
const MARKER_SCHEMA_VERSION: u32 = 1;
const MARKER_FILE: &str = "pi-memory-multilingual-e5-small.json";
const INSTALL_LOCK_FILE: &str = ".pi-memory-model-install.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelAsset {
    path: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const MODEL_ASSETS: &[ModelAsset] = &[
    ModelAsset {
        path: "onnx/model.onnx",
        bytes: 470_268_510,
        sha256: "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    },
    ModelAsset {
        path: "tokenizer.json",
        bytes: 17_082_730,
        sha256: "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    },
    ModelAsset {
        path: "config.json",
        bytes: 655,
        sha256: "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    },
    ModelAsset {
        path: "special_tokens_map.json",
        bytes: 167,
        sha256: "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    },
    ModelAsset {
        path: "tokenizer_config.json",
        bytes: 443,
        sha256: "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastEmbedModelStatus {
    pub descriptor: EmbeddingDescriptor,
    pub cache_dir: PathBuf,
    pub expected_download_bytes: u64,
    pub state: FastEmbedModelState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastEmbedModelState {
    Missing,
    Ready { installed_at_ms: i64 },
    Invalid { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastEmbedInstallReceipt {
    pub descriptor: EmbeddingDescriptor,
    pub cache_dir: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub reused: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FastEmbedModelError {
    #[error("cannot {operation} embedding model path {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot download embedding model asset {asset}: {message}")]
    Download { asset: String, message: String },
    #[error("embedding model asset {path} has {actual_bytes} bytes; expected {expected_bytes}")]
    SizeMismatch {
        path: PathBuf,
        expected_bytes: u64,
        actual_bytes: u64,
    },
    #[error("embedding model asset {path} failed SHA-256 verification")]
    ChecksumMismatch { path: PathBuf },
    #[error("embedding model installation marker is invalid: {0}")]
    InvalidMarker(String),
    #[error("embedding model is not installed")]
    NotInstalled,
    #[error("cannot initialize embedding model: {0}")]
    Initialize(String),
    #[error("embedding worker failed: {0}")]
    Worker(String),
}

#[derive(Clone)]
pub struct FastEmbedModelStore {
    cache_dir: PathBuf,
    source: Arc<dyn AssetSource>,
    verifier: Arc<dyn AssetChecksumVerifier>,
}

impl std::fmt::Debug for FastEmbedModelStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FastEmbedModelStore")
            .field("cache_dir", &self.cache_dir)
            .field("descriptor", &descriptor())
            .finish_non_exhaustive()
    }
}

impl FastEmbedModelStore {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            source: Arc::new(HuggingFaceSource),
            verifier: Arc::new(Sha256Verifier),
        }
    }

    pub fn status(&self) -> FastEmbedModelStatus {
        let state = match self.read_valid_marker() {
            Ok(Some(marker)) => match self.resolve_asset_paths() {
                Ok(_) => FastEmbedModelState::Ready {
                    installed_at_ms: marker.installed_at_ms,
                },
                Err(error) => FastEmbedModelState::Invalid {
                    message: error.to_string(),
                },
            },
            Ok(None) => FastEmbedModelState::Missing,
            Err(error) => FastEmbedModelState::Invalid {
                message: error.to_string(),
            },
        };
        FastEmbedModelStatus {
            descriptor: descriptor(),
            cache_dir: self.cache_dir.clone(),
            expected_download_bytes: expected_download_bytes(),
            state,
        }
    }

    /// Download and verify every pinned asset, then atomically publish the
    /// install marker. A ready installation is reused without network access.
    pub async fn install(&self) -> Result<FastEmbedInstallReceipt, FastEmbedModelError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.install_blocking())
            .await
            .map_err(|error| FastEmbedModelError::Worker(error.to_string()))?
    }

    /// Open the local model only when a complete verified installation exists.
    /// This method never downloads assets.
    pub fn embedder_if_ready(
        &self,
    ) -> Result<Option<Arc<dyn MemoryEmbedder>>, FastEmbedModelError> {
        match self.status().state {
            FastEmbedModelState::Missing => Ok(None),
            FastEmbedModelState::Invalid { message } => {
                Err(FastEmbedModelError::InvalidMarker(message))
            }
            FastEmbedModelState::Ready { .. } => {
                let paths = self.resolve_asset_paths()?;
                let embedder = FastEmbedMemoryEmbedder::load(paths)?;
                Ok(Some(Arc::new(embedder)))
            }
        }
    }

    fn install_blocking(&self) -> Result<FastEmbedInstallReceipt, FastEmbedModelError> {
        create_dir_all(&self.cache_dir)?;
        let lock_path = self.cache_dir.join(INSTALL_LOCK_FILE);
        let lock = open_lock_file(&lock_path)?;
        lock.lock_exclusive()
            .map_err(|source| FastEmbedModelError::Io {
                operation: "lock",
                path: lock_path.clone(),
                source,
            })?;

        let result = self.install_while_locked();
        let unlock_result = FileExt::unlock(&lock).map_err(|source| FastEmbedModelError::Io {
            operation: "unlock",
            path: lock_path,
            source,
        });
        match (result, unlock_result) {
            (Ok(receipt), Ok(())) => Ok(receipt),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn install_while_locked(&self) -> Result<FastEmbedInstallReceipt, FastEmbedModelError> {
        let marker_ready = matches!(self.status().state, FastEmbedModelState::Ready { .. });
        let mut paths = self.verified_cached_asset_paths();
        if paths.len() == MODEL_ASSETS.len() {
            if !marker_ready {
                self.clear_marker()?;
                self.write_marker(&InstallMarker::expected(now_ms()))?;
            }
            return Ok(self.receipt(true));
        }
        self.clear_marker()?;

        let missing = MODEL_ASSETS
            .iter()
            .copied()
            .filter(|asset| !paths.contains_key(asset.path))
            .collect::<Vec<_>>();
        let downloaded = self
            .source
            .fetch(&self.cache_dir, MODEL_ID, MODEL_REVISION, &missing)?;
        if downloaded.len() != missing.len() {
            return Err(FastEmbedModelError::Download {
                asset: MODEL_ID.to_string(),
                message: format!(
                    "source returned {} paths for {} assets",
                    downloaded.len(),
                    missing.len()
                ),
            });
        }
        paths.extend(
            missing
                .iter()
                .zip(downloaded)
                .map(|(asset, path)| (asset.path, path)),
        );
        for asset in MODEL_ASSETS {
            let path = paths
                .get(asset.path)
                .ok_or_else(|| FastEmbedModelError::Download {
                    asset: asset.path.to_string(),
                    message: "source did not provide a local path".to_string(),
                })?;
            verify_asset_size(asset, path)?;
            self.verifier.verify(asset, path)?;
        }
        self.write_marker(&InstallMarker::expected(now_ms()))?;
        Ok(self.receipt(false))
    }

    fn receipt(&self, reused: bool) -> FastEmbedInstallReceipt {
        FastEmbedInstallReceipt {
            descriptor: descriptor(),
            cache_dir: self.cache_dir.clone(),
            files: MODEL_ASSETS.len(),
            bytes: expected_download_bytes(),
            reused,
        }
    }

    fn marker_path(&self) -> PathBuf {
        self.cache_dir.join(MARKER_FILE)
    }

    fn clear_marker(&self) -> Result<(), FastEmbedModelError> {
        let path = self.marker_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(FastEmbedModelError::Io {
                operation: "remove stale",
                path,
                source,
            }),
        }
    }

    fn read_valid_marker(&self) -> Result<Option<InstallMarker>, FastEmbedModelError> {
        let path = self.marker_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(FastEmbedModelError::Io {
                    operation: "read",
                    path,
                    source,
                });
            }
        };
        let marker: InstallMarker = serde_json::from_slice(&bytes)
            .map_err(|error| FastEmbedModelError::InvalidMarker(error.to_string()))?;
        marker.validate()?;
        Ok(Some(marker))
    }

    fn write_marker(&self, marker: &InstallMarker) -> Result<(), FastEmbedModelError> {
        let marker_path = self.marker_path();
        let mut temporary = tempfile::NamedTempFile::new_in(&self.cache_dir).map_err(|source| {
            FastEmbedModelError::Io {
                operation: "create temporary marker in",
                path: self.cache_dir.clone(),
                source,
            }
        })?;
        serde_json::to_writer_pretty(&mut temporary, marker)
            .map_err(|error| FastEmbedModelError::InvalidMarker(error.to_string()))?;
        temporary
            .write_all(b"\n")
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| FastEmbedModelError::Io {
                operation: "write",
                path: marker_path.clone(),
                source,
            })?;
        temporary
            .persist(&marker_path)
            .map_err(|error| FastEmbedModelError::Io {
                operation: "persist",
                path: marker_path,
                source: error.error,
            })?;
        sync_directory(&self.cache_dir)?;
        Ok(())
    }

    fn resolve_asset_paths(&self) -> Result<ModelPaths, FastEmbedModelError> {
        let cache = Cache::new(self.cache_dir.clone());
        let repo = cache.repo(pinned_repo());
        let mut paths = HashMap::new();
        for asset in MODEL_ASSETS {
            let path = repo.get(asset.path).ok_or_else(|| {
                FastEmbedModelError::InvalidMarker(format!(
                    "verified asset {} is missing from {}",
                    asset.path,
                    self.cache_dir.display()
                ))
            })?;
            verify_asset_size(asset, &path)?;
            paths.insert(asset.path, path);
        }
        ModelPaths::from_map(paths)
    }

    fn verified_cached_asset_paths(&self) -> HashMap<&'static str, PathBuf> {
        let cache = Cache::new(self.cache_dir.clone());
        let repo = cache.repo(pinned_repo());
        MODEL_ASSETS
            .iter()
            .filter_map(|asset| {
                let path = repo.get(asset.path)?;
                verify_asset_size(asset, &path).ok()?;
                self.verifier.verify(asset, &path).ok()?;
                Some((asset.path, path))
            })
            .collect()
    }

    #[cfg(test)]
    fn with_source(
        cache_dir: impl Into<PathBuf>,
        source: Arc<dyn AssetSource>,
        verifier: Arc<dyn AssetChecksumVerifier>,
    ) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            source,
            verifier,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    schema_version: u32,
    model: String,
    model_revision: String,
    embedding_revision: String,
    dimensions: usize,
    assets: Vec<InstallMarkerAsset>,
    installed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct InstallMarkerAsset {
    path: String,
    bytes: u64,
    sha256: String,
}

impl InstallMarker {
    fn expected(installed_at_ms: i64) -> Self {
        Self {
            schema_version: MARKER_SCHEMA_VERSION,
            model: MODEL_ID.to_string(),
            model_revision: MODEL_REVISION.to_string(),
            embedding_revision: EMBEDDING_REVISION.to_string(),
            dimensions: DIMENSIONS,
            assets: MODEL_ASSETS
                .iter()
                .map(|asset| InstallMarkerAsset {
                    path: asset.path.to_string(),
                    bytes: asset.bytes,
                    sha256: asset.sha256.to_string(),
                })
                .collect(),
            installed_at_ms,
        }
    }

    fn validate(&self) -> Result<(), FastEmbedModelError> {
        let expected = Self::expected(self.installed_at_ms);
        if self != &expected {
            return Err(FastEmbedModelError::InvalidMarker(format!(
                "{} does not describe the pinned {} revision",
                MARKER_FILE, MODEL_ID
            )));
        }
        Ok(())
    }
}

trait AssetSource: Send + Sync {
    fn fetch(
        &self,
        cache_dir: &Path,
        model: &str,
        revision: &str,
        assets: &[ModelAsset],
    ) -> Result<Vec<PathBuf>, FastEmbedModelError>;
}

trait AssetChecksumVerifier: Send + Sync {
    fn verify(&self, asset: &ModelAsset, path: &Path) -> Result<(), FastEmbedModelError>;
}

#[derive(Debug)]
struct Sha256Verifier;

impl AssetChecksumVerifier for Sha256Verifier {
    fn verify(&self, asset: &ModelAsset, path: &Path) -> Result<(), FastEmbedModelError> {
        if hash_file(path)? != asset.sha256 {
            return Err(FastEmbedModelError::ChecksumMismatch {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
struct HuggingFaceSource;

impl AssetSource for HuggingFaceSource {
    fn fetch(
        &self,
        cache_dir: &Path,
        model: &str,
        revision: &str,
        assets: &[ModelAsset],
    ) -> Result<Vec<PathBuf>, FastEmbedModelError> {
        let mut builder = ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .with_progress(false);
        if let Ok(endpoint) = std::env::var("HF_ENDPOINT") {
            builder = builder.with_endpoint(endpoint);
        }
        let api = builder
            .build()
            .map_err(|error| FastEmbedModelError::Download {
                asset: model.to_string(),
                message: error.to_string(),
            })?;
        let repo = api.repo(Repo::with_revision(
            model.to_string(),
            RepoType::Model,
            revision.to_string(),
        ));
        assets
            .iter()
            .map(|asset| {
                repo.download(asset.path)
                    .map_err(|error| FastEmbedModelError::Download {
                        asset: asset.path.to_string(),
                        message: error.to_string(),
                    })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct ModelPaths {
    onnx: PathBuf,
    tokenizer: PathBuf,
    config: PathBuf,
    special_tokens: PathBuf,
    tokenizer_config: PathBuf,
}

impl ModelPaths {
    fn from_map(mut paths: HashMap<&'static str, PathBuf>) -> Result<Self, FastEmbedModelError> {
        let mut take = |name| {
            paths.remove(name).ok_or_else(|| {
                FastEmbedModelError::InvalidMarker(format!("model asset {name} is missing"))
            })
        };
        Ok(Self {
            onnx: take("onnx/model.onnx")?,
            tokenizer: take("tokenizer.json")?,
            config: take("config.json")?,
            special_tokens: take("special_tokens_map.json")?,
            tokenizer_config: take("tokenizer_config.json")?,
        })
    }
}

type SharedTextEmbedding = Arc<Mutex<TextEmbedding>>;
type SharedModelCache = Mutex<HashMap<String, Weak<Mutex<TextEmbedding>>>>;

fn shared_models() -> &'static SharedModelCache {
    static MODELS: OnceLock<SharedModelCache> = OnceLock::new();
    MODELS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct FastEmbedMemoryEmbedder {
    descriptor: EmbeddingDescriptor,
    model: SharedTextEmbedding,
}

impl FastEmbedMemoryEmbedder {
    fn load(paths: ModelPaths) -> Result<Self, FastEmbedModelError> {
        let descriptor = descriptor();
        let cache_key = format!(
            "{}@{}:{}",
            descriptor.model, descriptor.revision, descriptor.dimensions
        );
        if let Some(model) = shared_models()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cache_key)
            .and_then(Weak::upgrade)
        {
            return Ok(Self { descriptor, model });
        }

        let model = Arc::new(Mutex::new(load_text_embedding(&paths)?));
        let mut models = shared_models()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let model = models
            .get(&cache_key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                models.insert(cache_key, Arc::downgrade(&model));
                model
            });
        Ok(Self { descriptor, model })
    }
}

impl std::fmt::Debug for FastEmbedMemoryEmbedder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FastEmbedMemoryEmbedder")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl MemoryEmbedder for FastEmbedMemoryEmbedder {
    fn descriptor(&self) -> &EmbeddingDescriptor {
        &self.descriptor
    }

    async fn embed(
        &self,
        purpose: EmbeddingPurpose,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let inputs = prefix_inputs(purpose, texts);
        let model = Arc::clone(&self.model);
        tokio::task::spawn_blocking(move || {
            model
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .embed(inputs, Some(EMBEDDING_BATCH_SIZE))
                .map_err(|error| EmbeddingError::Provider(error.to_string()))
        })
        .await
        .map_err(|error| EmbeddingError::Provider(format!("embedding worker failed: {error}")))?
    }
}

fn load_text_embedding(paths: &ModelPaths) -> Result<TextEmbedding, FastEmbedModelError> {
    let onnx_file = read_verified_file(model_asset("onnx/model.onnx"), &paths.onnx)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read_verified_file(model_asset("tokenizer.json"), &paths.tokenizer)?,
        config_file: read_verified_file(model_asset("config.json"), &paths.config)?,
        special_tokens_map_file: read_verified_file(
            model_asset("special_tokens_map.json"),
            &paths.special_tokens,
        )?,
        tokenizer_config_file: read_verified_file(
            model_asset("tokenizer_config.json"),
            &paths.tokenizer_config,
        )?,
    };
    let model =
        UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files).with_pooling(Pooling::Mean);
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(4);
    TextEmbedding::try_new_from_user_defined(
        model,
        InitOptionsUserDefined::new()
            .with_max_length(MAX_LENGTH)
            .with_intra_threads(threads),
    )
    .map_err(|error| FastEmbedModelError::Initialize(error.to_string()))
}

fn prefix_inputs(purpose: EmbeddingPurpose, texts: Vec<String>) -> Vec<String> {
    let prefix = match purpose {
        EmbeddingPurpose::Query => "query: ",
        EmbeddingPurpose::Document => "passage: ",
    };
    texts
        .into_iter()
        .map(|text| format!("{prefix}{text}"))
        .collect()
}

fn descriptor() -> EmbeddingDescriptor {
    EmbeddingDescriptor {
        model: MODEL_ID.to_string(),
        revision: EMBEDDING_REVISION.to_string(),
        dimensions: DIMENSIONS,
    }
}

fn pinned_repo() -> Repo {
    Repo::with_revision(
        MODEL_ID.to_string(),
        RepoType::Model,
        MODEL_REVISION.to_string(),
    )
}

fn expected_download_bytes() -> u64 {
    MODEL_ASSETS.iter().map(|asset| asset.bytes).sum()
}

fn model_asset(path: &str) -> &ModelAsset {
    MODEL_ASSETS
        .iter()
        .find(|asset| asset.path == path)
        .expect("built-in model asset")
}

fn verify_asset_size(asset: &ModelAsset, path: &Path) -> Result<(), FastEmbedModelError> {
    let metadata = std::fs::metadata(path).map_err(|source| FastEmbedModelError::Io {
        operation: "inspect",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() != asset.bytes {
        return Err(FastEmbedModelError::SizeMismatch {
            path: path.to_path_buf(),
            expected_bytes: asset.bytes,
            actual_bytes: metadata.len(),
        });
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, FastEmbedModelError> {
    let file = File::open(path).map_err(|source| FastEmbedModelError::Io {
        operation: "open",
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| FastEmbedModelError::Io {
                operation: "read",
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn read_file(path: &Path) -> Result<Vec<u8>, FastEmbedModelError> {
    std::fs::read(path).map_err(|source| FastEmbedModelError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })
}

fn read_verified_file(asset: &ModelAsset, path: &Path) -> Result<Vec<u8>, FastEmbedModelError> {
    let bytes = read_file(path)?;
    let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_bytes != asset.bytes {
        return Err(FastEmbedModelError::SizeMismatch {
            path: path.to_path_buf(),
            expected_bytes: asset.bytes,
            actual_bytes,
        });
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != asset.sha256 {
        return Err(FastEmbedModelError::ChecksumMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

fn create_dir_all(path: &Path) -> Result<(), FastEmbedModelError> {
    std::fs::create_dir_all(path).map_err(|source| FastEmbedModelError::Io {
        operation: "create directory",
        path: path.to_path_buf(),
        source,
    })
}

fn open_lock_file(path: &Path) -> Result<File, FastEmbedModelError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|source| FastEmbedModelError::Io {
            operation: "open",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), FastEmbedModelError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| FastEmbedModelError::Io {
            operation: "sync directory",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), FastEmbedModelError> {
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Debug)]
    struct SparseCacheSource {
        calls: Arc<AtomicUsize>,
        requested: Arc<Mutex<Vec<String>>>,
        wrong_size: bool,
    }

    impl AssetSource for SparseCacheSource {
        fn fetch(
            &self,
            cache_dir: &Path,
            model: &str,
            revision: &str,
            assets: &[ModelAsset],
        ) -> Result<Vec<PathBuf>, FastEmbedModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requested
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(assets.iter().map(|asset| asset.path.to_string()));
            let cache = Cache::new(cache_dir.to_path_buf());
            let repo = cache.repo(Repo::with_revision(
                model.to_string(),
                RepoType::Model,
                revision.to_string(),
            ));
            repo.create_ref(revision)
                .map_err(|source| FastEmbedModelError::Io {
                    operation: "create test ref",
                    path: cache_dir.to_path_buf(),
                    source,
                })?;
            let snapshot = repo.pointer_path(revision);
            assets
                .iter()
                .enumerate()
                .map(|(index, asset)| {
                    let destination = snapshot.join(asset.path);
                    create_dir_all(destination.parent().expect("asset parent"))?;
                    let file =
                        File::create(&destination).map_err(|source| FastEmbedModelError::Io {
                            operation: "create",
                            path: destination.clone(),
                            source,
                        })?;
                    let bytes = if self.wrong_size && index == 0 {
                        asset.bytes - 1
                    } else {
                        asset.bytes
                    };
                    file.set_len(bytes)
                        .map_err(|source| FastEmbedModelError::Io {
                            operation: "resize",
                            path: destination.clone(),
                            source,
                        })?;
                    Ok(destination)
                })
                .collect()
        }
    }

    #[derive(Debug)]
    struct AcceptChecksums;

    impl AssetChecksumVerifier for AcceptChecksums {
        fn verify(&self, _asset: &ModelAsset, _path: &Path) -> Result<(), FastEmbedModelError> {
            Ok(())
        }
    }

    #[test]
    fn e5_prefixes_are_owned_by_the_adapter() {
        assert_eq!(
            prefix_inputs(EmbeddingPurpose::Query, vec!["你好".to_string()]),
            ["query: 你好"]
        );
        assert_eq!(
            prefix_inputs(EmbeddingPurpose::Document, vec!["deploy".to_string()]),
            ["passage: deploy"]
        );
    }

    #[test]
    fn missing_or_malformed_markers_never_look_ready() {
        let directory = tempfile::tempdir().unwrap();
        let store = FastEmbedModelStore::new(directory.path());
        assert_eq!(store.status().state, FastEmbedModelState::Missing);

        std::fs::write(store.marker_path(), b"not json").unwrap();
        assert!(matches!(
            store.status().state,
            FastEmbedModelState::Invalid { .. }
        ));
        assert!(store.embedder_if_ready().is_err());
    }

    #[test]
    fn marker_validation_binds_the_embedding_space() {
        let mut marker = InstallMarker::expected(42);
        assert!(marker.validate().is_ok());
        marker.embedding_revision.push_str("-changed");
        assert!(marker.validate().is_err());
    }

    #[test]
    fn checksum_verification_rejects_wrong_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("asset");
        std::fs::write(&path, b"wrong").unwrap();
        let asset = ModelAsset {
            path: "asset",
            bytes: 5,
            sha256: "8810ad581e59f2bc3928b261707a71308f7e1399d91f6a281c18d980a7e6b5a",
        };
        assert!(matches!(
            Sha256Verifier.verify(&asset, &path),
            Err(FastEmbedModelError::ChecksumMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn install_is_explicit_atomic_and_reuses_verified_assets() {
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SparseCacheSource {
            calls: Arc::clone(&calls),
            requested: Arc::new(Mutex::new(Vec::new())),
            wrong_size: false,
        });
        let store =
            FastEmbedModelStore::with_source(directory.path(), source, Arc::new(AcceptChecksums));
        assert_eq!(store.status().state, FastEmbedModelState::Missing);

        let installed = store.install().await.unwrap();
        assert!(!installed.reused);
        assert!(matches!(
            store.status().state,
            FastEmbedModelState::Ready { .. }
        ));
        store.clear_marker().unwrap();
        assert_eq!(store.status().state, FastEmbedModelState::Missing);
        let reused = store.install().await.unwrap();
        assert!(reused.reused);
        assert!(matches!(
            store.status().state,
            FastEmbedModelState::Ready { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn interrupted_install_reuses_each_verified_cached_asset() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::new(directory.path().to_path_buf());
        let repo = cache.repo(pinned_repo());
        repo.create_ref(MODEL_REVISION).unwrap();
        let cached_asset = MODEL_ASSETS[0];
        let cached_path = repo.pointer_path(MODEL_REVISION).join(cached_asset.path);
        create_dir_all(cached_path.parent().unwrap()).unwrap();
        File::create(&cached_path)
            .unwrap()
            .set_len(cached_asset.bytes)
            .unwrap();

        let requested = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(SparseCacheSource {
            calls: Arc::new(AtomicUsize::new(0)),
            requested: Arc::clone(&requested),
            wrong_size: false,
        });
        let store =
            FastEmbedModelStore::with_source(directory.path(), source, Arc::new(AcceptChecksums));

        let receipt = store.install().await.unwrap();

        assert!(!receipt.reused);
        assert_eq!(
            *requested
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            MODEL_ASSETS[1..]
                .iter()
                .map(|asset| asset.path.to_string())
                .collect::<Vec<_>>()
        );
        assert!(matches!(
            store.status().state,
            FastEmbedModelState::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn failed_verification_does_not_publish_a_ready_marker() {
        let directory = tempfile::tempdir().unwrap();
        let store = FastEmbedModelStore::with_source(
            directory.path(),
            Arc::new(SparseCacheSource {
                calls: Arc::new(AtomicUsize::new(0)),
                requested: Arc::new(Mutex::new(Vec::new())),
                wrong_size: true,
            }),
            Arc::new(AcceptChecksums),
        );

        assert!(matches!(
            store.install().await,
            Err(FastEmbedModelError::SizeMismatch { .. })
        ));
        assert!(!store.marker_path().exists());
        assert_eq!(store.status().state, FastEmbedModelState::Missing);
    }
}
