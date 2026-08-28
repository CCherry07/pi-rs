use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use futures::StreamExt;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

pub const HOST_TARGET: &str = env!("PI_PLUGIN_HOST_TARGET");
const INTENT_SCHEMA: u32 = 1;
const LOCK_SCHEMA: u32 = 1;
const RELEASE_SCHEMA: u32 = 1;
const REGISTRY_SCHEMA: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PluginManagerError {
    #[error("cannot access plugin package state at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid plugin package data at {location}: {message}")]
    InvalidData { location: String, message: String },
    #[error(
        "plugin source {location} is unsupported; use a local path, HTTP URL, github:owner/repo@tag, or registry:id@version"
    )]
    UnsupportedSource { location: String },
    #[error("plugin source {location} requires a registry URL")]
    RegistryRequired { location: String },
    #[error("plugin {id} has no version satisfying {requirement}")]
    VersionNotFound { id: String, requirement: String },
    #[error("plugin {id} has no artifact for host target {target}")]
    MissingArtifact { id: String, target: String },
    #[error("plugin artifact hash mismatch for {location}: expected {expected}, got {actual}")]
    HashMismatch {
        location: String,
        expected: String,
        actual: String,
    },
    #[error("plugin download failed for {url}: {message}")]
    Download { url: String, message: String },
    #[error("plugin download from {url} exceeds the {limit}-byte limit")]
    DownloadTooLarge { url: String, limit: u64 },
    #[error("plugin {id} is not installed in {scope} scope")]
    NotInstalled { id: String, scope: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    Global,
    Project,
}

impl InstallScope {
    fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginManagerOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub target: String,
    pub registry: Option<String>,
}

impl PluginManagerOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            target: HOST_TARGET.to_string(),
            registry: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginIntent {
    pub id: String,
    pub source: String,
    #[serde(default = "any_version")]
    pub version: String,
    #[serde(default = "empty_options")]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginIntentFile {
    pub schema: u32,
    #[serde(default)]
    pub plugins: Vec<PluginIntent>,
}

impl Default for PluginIntentFile {
    fn default() -> Self {
        Self {
            schema: INTENT_SCHEMA,
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LockedPlugin {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub source: String,
    pub target: String,
    pub sha256: String,
    pub artifact: String,
    #[serde(default = "empty_options")]
    pub options: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginLockFile {
    pub schema: u32,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_sha256: Option<String>,
    #[serde(default)]
    pub plugins: Vec<LockedPlugin>,
}

impl PluginLockFile {
    fn empty(target: impl Into<String>) -> Self {
        Self {
            schema: LOCK_SCHEMA,
            target: target.into(),
            intent_sha256: None,
            plugins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileStatus {
    Unchanged,
    Repaired,
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub source: String,
    pub target: String,
    pub sha256: String,
}

impl From<&LockedPlugin> for InstalledPlugin {
    fn from(plugin: &LockedPlugin) -> Self {
        Self {
            id: plugin.id.clone(),
            version: plugin.version.clone(),
            kind: plugin.kind.clone(),
            source: plugin.source.clone(),
            target: plugin.target.clone(),
            sha256: plugin.sha256.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PluginManager {
    options: PluginManagerOptions,
    client: reqwest::Client,
}

#[must_use = "prepared plugin state must be committed or rolled back"]
pub struct PreparedPluginReconcile {
    manager: PluginManager,
    paths: ScopePaths,
    previous_lock: Option<PluginLockFile>,
    current_lock: PluginLockFile,
    status: ReconcileStatus,
    finished: bool,
    _guard: Option<StateGuard>,
}

impl PreparedPluginReconcile {
    pub fn status(&self) -> ReconcileStatus {
        self.status
    }

    pub fn installed(&self) -> Vec<InstalledPlugin> {
        self.current_lock
            .plugins
            .iter()
            .map(InstalledPlugin::from)
            .collect()
    }

    pub fn commit(mut self) {
        self.finished = true;
    }

    pub fn rollback(mut self) -> Result<(), PluginManagerError> {
        self.rollback_in_place()
    }

    fn rollback_in_place(&mut self) -> Result<(), PluginManagerError> {
        if self.finished || self.status != ReconcileStatus::Updated {
            self.finished = true;
            return Ok(());
        }
        if let Some(previous_lock) = &self.previous_lock {
            write_json_atomic(&self.paths.lock, previous_lock)?;
            self.manager.activate(&self.paths, previous_lock)?;
        } else {
            remove_file_if_exists(&self.paths.lock)?;
            remove_dir_if_exists(&self.paths.active)?;
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for PreparedPluginReconcile {
    fn drop(&mut self) {
        let _ = self.rollback_in_place();
    }
}

impl PluginManager {
    pub fn new(options: PluginManagerOptions) -> Result<Self, PluginManagerError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("pi-rs/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| PluginManagerError::InvalidData {
                location: "HTTP client".to_string(),
                message: error.to_string(),
            })?;
        Ok(Self { options, client })
    }

    pub async fn install(
        &self,
        source: impl Into<String>,
        version: Option<&str>,
        scope: InstallScope,
    ) -> Result<InstalledPlugin, PluginManagerError> {
        let source = source.into();
        let (source, requirement) = normalize_install_source(&source, version)?;
        let source = canonical_local_source(&source, &self.options.cwd)?;
        let paths = self.scope_paths(scope);
        reject_discovered_source(&source, &paths.active)?;
        let _guard = StateGuard::acquire(&paths.guard)?;
        let mut intent = read_intent(&paths.intent)?;
        let previous_intent = intent.clone();
        let previous_lock = read_lock(&paths.lock, &self.options.target)?;
        let preview = self.resolve_source(&source, &requirement).await?;
        let options = intent
            .plugins
            .iter()
            .find(|plugin| plugin.id == preview.id)
            .map(|plugin| plugin.options.clone())
            .unwrap_or_else(empty_options);
        let desired = PluginIntent {
            id: preview.id.clone(),
            source: source.clone(),
            version: requirement.to_string(),
            options,
        };
        if let Some(existing) = intent
            .plugins
            .iter_mut()
            .find(|plugin| plugin.id == desired.id)
        {
            *existing = desired;
        } else {
            intent.plugins.push(desired);
        }
        let mut preferred = locked_versions(&previous_lock)?;
        preferred.remove(&preview.id);
        let candidates = self.resolve_intent(&intent, &preferred).await?;
        let lock = self.install_candidates(&paths, &intent, candidates)?;
        self.commit_state(&paths, &previous_intent, &previous_lock, &intent, &lock)?;
        lock.plugins
            .iter()
            .find(|plugin| plugin.id == preview.id)
            .map(InstalledPlugin::from)
            .ok_or_else(|| PluginManagerError::InvalidData {
                location: source,
                message: "resolved root disappeared from the lock file".to_string(),
            })
    }

    pub fn list(&self, scope: InstallScope) -> Result<Vec<InstalledPlugin>, PluginManagerError> {
        let paths = self.scope_paths(scope);
        let lock = read_lock(&paths.lock, &self.options.target)?;
        validate_lock_target(&lock, &self.options.target, &paths.lock)?;
        Ok(lock.plugins.iter().map(InstalledPlugin::from).collect())
    }

    pub async fn sync(
        &self,
        scope: InstallScope,
    ) -> Result<Vec<InstalledPlugin>, PluginManagerError> {
        let prepared = self.prepare_reconcile_inner(scope, true).await?;
        let installed = prepared.installed();
        prepared.commit();
        Ok(installed)
    }

    pub async fn prepare_reconcile(
        &self,
        scope: InstallScope,
    ) -> Result<PreparedPluginReconcile, PluginManagerError> {
        self.prepare_reconcile_inner(scope, false).await
    }

    pub fn remove(
        &self,
        id: &str,
        scope: InstallScope,
    ) -> Result<InstalledPlugin, PluginManagerError> {
        let paths = self.scope_paths(scope);
        let _guard = StateGuard::acquire(&paths.guard)?;
        let mut intent = read_intent(&paths.intent)?;
        let lock = read_lock(&paths.lock, &self.options.target)?;
        validate_lock_target(&lock, &self.options.target, &paths.lock)?;
        let previous_intent = intent.clone();
        let removed = lock
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .map(InstalledPlugin::from)
            .ok_or_else(|| PluginManagerError::NotInstalled {
                id: id.to_string(),
                scope: scope.name(),
            })?;
        let previous_len = intent.plugins.len();
        intent.plugins.retain(|plugin| plugin.id != id);
        if intent.plugins.len() == previous_len {
            return Err(PluginManagerError::NotInstalled {
                id: id.to_string(),
                scope: scope.name(),
            });
        }
        let next_lock = PluginLockFile {
            schema: LOCK_SCHEMA,
            target: self.options.target.clone(),
            intent_sha256: Some(intent_digest(&intent)?),
            plugins: lock
                .plugins
                .iter()
                .filter(|plugin| plugin.id != id)
                .cloned()
                .collect(),
        };
        self.commit_state(&paths, &previous_intent, &lock, &intent, &next_lock)?;
        Ok(removed)
    }

    async fn prepare_reconcile_inner(
        &self,
        scope: InstallScope,
        force: bool,
    ) -> Result<PreparedPluginReconcile, PluginManagerError> {
        let paths = self.scope_paths(scope);
        if !paths.intent.exists() && !paths.lock.exists() && !paths.active.exists() {
            return Ok(PreparedPluginReconcile {
                manager: self.clone(),
                paths,
                previous_lock: None,
                current_lock: PluginLockFile::empty(&self.options.target),
                status: ReconcileStatus::Unchanged,
                finished: false,
                _guard: None,
            });
        }
        let guard = StateGuard::acquire(&paths.guard)?;
        let intent = read_intent(&paths.intent)?;
        let previous_lock_existed = paths.lock.exists();
        let previous_lock = read_lock(&paths.lock, &self.options.target)?;
        validate_lock_target(&previous_lock, &self.options.target, &paths.lock)?;
        let digest = intent_digest(&intent)?;
        let intent_current = previous_lock.intent_sha256.as_deref() == Some(digest.as_str());
        let store_current = if intent_current {
            ensure_store_entries(&paths, &previous_lock)?
        } else {
            false
        };
        let local_sources_current = if intent_current {
            self.local_roots_current(&intent, &previous_lock)?
        } else {
            false
        };
        if !force && intent_current && store_current && local_sources_current {
            let status = if activation_current(&paths, &previous_lock)? {
                ReconcileStatus::Unchanged
            } else {
                self.activate(&paths, &previous_lock)?;
                ReconcileStatus::Repaired
            };
            return Ok(PreparedPluginReconcile {
                manager: self.clone(),
                paths,
                previous_lock: previous_lock_existed.then_some(previous_lock.clone()),
                current_lock: previous_lock,
                status,
                finished: false,
                _guard: Some(guard),
            });
        }
        let preferred = locked_versions(&previous_lock)?;
        let candidates = self.resolve_intent(&intent, &preferred).await?;
        let lock = self.install_candidates(&paths, &intent, candidates)?;
        write_json_atomic(&paths.lock, &lock)?;
        if let Err(error) = self.activate(&paths, &lock) {
            if previous_lock_existed {
                let _ = write_json_atomic(&paths.lock, &previous_lock);
                let _ = self.activate(&paths, &previous_lock);
            } else {
                let _ = remove_file_if_exists(&paths.lock);
                let _ = remove_dir_if_exists(&paths.active);
            }
            return Err(error);
        }
        Ok(PreparedPluginReconcile {
            manager: self.clone(),
            paths,
            previous_lock: previous_lock_existed.then_some(previous_lock),
            current_lock: lock,
            status: ReconcileStatus::Updated,
            finished: false,
            _guard: Some(guard),
        })
    }

    fn local_roots_current(
        &self,
        intent: &PluginIntentFile,
        lock: &PluginLockFile,
    ) -> Result<bool, PluginManagerError> {
        let locked = lock
            .plugins
            .iter()
            .map(|plugin| (plugin.id.as_str(), plugin))
            .collect::<HashMap<_, _>>();
        for plugin in &intent.plugins {
            let Some(path) = local_source_path(&plugin.source, &self.options.cwd) else {
                continue;
            };
            if !path.exists() {
                return Ok(false);
            }
            let requirement = parse_requirement(&plugin.version, &plugin.id)?;
            let candidate =
                resolve_local_package(&path, &plugin.source, &requirement, &self.options.target)?;
            let Some(locked) = locked.get(candidate.id.as_str()) else {
                return Ok(false);
            };
            let effective_options = merge_options(&candidate.options, &plugin.options);
            if locked.version != candidate.version.to_string()
                || locked.sha256 != candidate.sha256
                || locked.options != effective_options
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn resolve_intent(
        &self,
        intent: &PluginIntentFile,
        preferred: &HashMap<String, Version>,
    ) -> Result<Vec<ResolvedCandidate>, PluginManagerError> {
        let mut resolved = Vec::with_capacity(intent.plugins.len());
        for plugin in &intent.plugins {
            let requirement = parse_requirement(&plugin.version, &plugin.id)?;
            let preferred_requirement = preferred
                .get(&plugin.id)
                .filter(|version| requirement.matches(version))
                .map(exact_requirement);
            let candidate = match self
                .resolve_source(
                    &plugin.source,
                    preferred_requirement.as_ref().unwrap_or(&requirement),
                )
                .await
            {
                Err(PluginManagerError::VersionNotFound { .. })
                    if preferred_requirement.is_some()
                        && local_source_path(&plugin.source, &self.options.cwd).is_some() =>
                {
                    self.resolve_source(&plugin.source, &requirement).await?
                }
                result => result?,
            };
            if plugin.id != candidate.id {
                return Err(PluginManagerError::InvalidData {
                    location: plugin.source.clone(),
                    message: format!(
                        "requested plugin {}, but the source declares {}",
                        plugin.id, candidate.id
                    ),
                });
            }
            resolved.push(candidate);
        }
        Ok(resolved)
    }

    async fn resolve_source(
        &self,
        source: &str,
        requirement: &VersionReq,
    ) -> Result<ResolvedCandidate, PluginManagerError> {
        if let Some(id) = source.strip_prefix("registry:") {
            return self.resolve_registry(id, requirement).await;
        }
        if let Some(spec) = source.strip_prefix("github:") {
            let (repository, tag) =
                spec.rsplit_once('@')
                    .ok_or_else(|| PluginManagerError::InvalidData {
                        location: source.to_string(),
                        message: "GitHub sources must use github:owner/repository@tag".to_string(),
                    })?;
            let url = format!(
                "https://github.com/{repository}/releases/download/{tag}/pi-plugin-release.json"
            );
            return self.resolve_release_url(&url, requirement).await;
        }
        if source.starts_with("https://") || source.starts_with("http://") {
            return self.resolve_release_url(source, requirement).await;
        }
        let path = source.strip_prefix("path:").unwrap_or(source);
        let path = {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                self.options.cwd.join(path)
            }
        };
        if path.exists() {
            return resolve_local_package(&path, source, requirement, &self.options.target);
        }
        Err(PluginManagerError::UnsupportedSource {
            location: source.to_string(),
        })
    }

    async fn resolve_registry(
        &self,
        id: &str,
        requirement: &VersionReq,
    ) -> Result<ResolvedCandidate, PluginManagerError> {
        validate_plugin_id(id, &format!("registry:{id}"))?;
        let registry =
            self.options
                .registry
                .as_ref()
                .ok_or_else(|| PluginManagerError::RegistryRequired {
                    location: format!("registry:{id}"),
                })?;
        let bytes = self.fetch(registry, MAX_MANIFEST_BYTES).await?;
        let index: RegistryIndex = parse_json(&bytes, registry)?;
        if index.schema != REGISTRY_SCHEMA {
            return Err(invalid_data(
                registry,
                format!("unsupported registry schema {}", index.schema),
            ));
        }
        let releases =
            index
                .plugins
                .get(id)
                .ok_or_else(|| PluginManagerError::VersionNotFound {
                    id: id.to_string(),
                    requirement: requirement.to_string(),
                })?;
        let parsed_releases = releases
            .iter()
            .map(|release| {
                Version::parse(&release.version)
                    .map(|version| (version, release))
                    .map_err(|error| {
                        invalid_data(
                            registry,
                            format!("invalid version for plugin {id}: {error}"),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let selected = parsed_releases
            .iter()
            .filter(|(version, _)| requirement.matches(version))
            .max_by(|(left, _), (right, _)| left.cmp(right))
            .ok_or_else(|| PluginManagerError::VersionNotFound {
                id: id.to_string(),
                requirement: requirement.to_string(),
            })?;
        let manifest = resolve_url(registry, &selected.1.manifest)?;
        let candidate = self.resolve_release_url(&manifest, requirement).await?;
        if candidate.id != id || candidate.version != selected.0 {
            return Err(invalid_data(
                manifest,
                format!(
                    "registry selected {id}@{}, but the release declares {}@{}",
                    selected.0, candidate.id, candidate.version
                ),
            ));
        }
        Ok(candidate)
    }

    async fn resolve_release_url(
        &self,
        manifest_url: &str,
        requirement: &VersionReq,
    ) -> Result<ResolvedCandidate, PluginManagerError> {
        let bytes = self.fetch(manifest_url, MAX_MANIFEST_BYTES).await?;
        let release: ReleaseManifest = parse_json(&bytes, manifest_url)?;
        if release.schema != RELEASE_SCHEMA {
            return Err(invalid_data(
                manifest_url,
                format!("unsupported release schema {}", release.schema),
            ));
        }
        validate_identity(&release.id, &release.kind, manifest_url)?;
        let version = parse_version(&release.version, &release.id)?;
        if !requirement.matches(&version) {
            return Err(PluginManagerError::VersionNotFound {
                id: release.id,
                requirement: requirement.to_string(),
            });
        }
        validate_options(&release.options, &release.id)?;
        let artifact = release
            .artifacts
            .iter()
            .find(|artifact| artifact.target == self.options.target)
            .ok_or_else(|| PluginManagerError::MissingArtifact {
                id: release.id.clone(),
                target: self.options.target.clone(),
            })?;
        validate_sha256(&artifact.sha256, manifest_url)?;
        let artifact_url = resolve_url(manifest_url, &artifact.url)?;
        let artifact_bytes = self.fetch(&artifact_url, MAX_ARTIFACT_BYTES).await?;
        verify_hash(&artifact_bytes, &artifact.sha256, &artifact_url)?;
        let artifact_name = artifact
            .file_name
            .clone()
            .or_else(|| {
                Url::parse(&artifact_url).ok().and_then(|url| {
                    url.path_segments()
                        .and_then(Iterator::last)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                })
            })
            .ok_or_else(|| invalid_data(&artifact_url, "artifact has no file name"))?;
        validate_file_name(&artifact_name, &artifact_url)?;
        Ok(ResolvedCandidate {
            id: release.id,
            version,
            kind: release.kind,
            source: manifest_url.to_string(),
            target: artifact.target.clone(),
            sha256: artifact.sha256.to_ascii_lowercase(),
            artifact_name,
            artifact_bytes,
            options: release.options,
        })
    }

    async fn fetch(&self, url: &str, limit: u64) -> Result<Vec<u8>, PluginManagerError> {
        let parsed = Url::parse(url).map_err(|error| invalid_data(url, error.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(PluginManagerError::UnsupportedSource {
                location: url.to_string(),
            });
        }
        if parsed.scheme() == "http"
            && !parsed.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            })
        {
            return Err(invalid_data(
                url,
                "remote plugin metadata and artifacts require HTTPS",
            ));
        }
        let response = self
            .client
            .get(parsed)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| PluginManagerError::Download {
                url: url.to_string(),
                message: error.to_string(),
            })?;
        if response
            .content_length()
            .is_some_and(|content_length| content_length > limit)
        {
            return Err(PluginManagerError::DownloadTooLarge {
                url: url.to_string(),
                limit,
            });
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| PluginManagerError::Download {
                url: url.to_string(),
                message: error.to_string(),
            })?;
            let next_len = bytes.len().saturating_add(chunk.len());
            if u64::try_from(next_len).unwrap_or(u64::MAX) > limit {
                return Err(PluginManagerError::DownloadTooLarge {
                    url: url.to_string(),
                    limit,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn install_candidates(
        &self,
        paths: &ScopePaths,
        intent: &PluginIntentFile,
        candidates: Vec<ResolvedCandidate>,
    ) -> Result<PluginLockFile, PluginManagerError> {
        let root_options = intent
            .plugins
            .iter()
            .map(|plugin| (plugin.id.as_str(), &plugin.options))
            .collect::<HashMap<_, _>>();
        let mut lock = PluginLockFile::empty(&self.options.target);
        lock.intent_sha256 = Some(intent_digest(intent)?);
        for candidate in candidates {
            let store = paths.store.join("sha256").join(&candidate.sha256);
            materialize_store(&store, &candidate)?;
            let options = root_options.get(candidate.id.as_str()).map_or_else(
                || candidate.options.clone(),
                |overrides| merge_options(&candidate.options, overrides),
            );
            validate_options(&options, &candidate.id)?;
            lock.plugins.push(LockedPlugin {
                id: candidate.id,
                version: candidate.version.to_string(),
                kind: candidate.kind,
                source: candidate.source,
                target: candidate.target,
                sha256: candidate.sha256,
                artifact: candidate.artifact_name,
                options,
            });
        }
        Ok(lock)
    }

    fn activate(
        &self,
        paths: &ScopePaths,
        lock: &PluginLockFile,
    ) -> Result<(), PluginManagerError> {
        let plugin_root = paths.active.parent().ok_or_else(|| {
            invalid_data(
                paths.active.display().to_string(),
                "active plugin directory has no parent",
            )
        })?;
        create_dir_all(plugin_root)?;
        let nonce = next_nonce();
        let stage = plugin_root.join(format!(".installed-stage-{nonce}"));
        let backup = plugin_root.join(format!(".installed-backup-{nonce}"));
        remove_dir_if_exists(&stage)?;
        create_dir_all(&stage)?;
        for (index, plugin) in lock.plugins.iter().enumerate() {
            validate_file_name(&plugin.artifact, &plugin.source)?;
            let stored_artifact = paths.store.join("sha256").join(&plugin.sha256);
            if !stored_artifact.is_file() {
                return Err(PluginManagerError::Io {
                    path: stored_artifact,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "content-addressed artifact is missing",
                    ),
                });
            }
            let package = stage.join(format!("{index:04}-{}", safe_id(&plugin.id)));
            create_dir_all(&package)?;
            link_or_copy(&stored_artifact, &package.join(&plugin.artifact))?;
            write_loader_manifest(&package.join("pi-plugin.toml"), plugin)?;
        }
        if paths.active.exists() {
            fs::rename(&paths.active, &backup).map_err(|source| PluginManagerError::Io {
                path: paths.active.clone(),
                source,
            })?;
        }
        if let Err(source) = fs::rename(&stage, &paths.active) {
            if backup.exists() {
                let _ = fs::rename(&backup, &paths.active);
            }
            return Err(PluginManagerError::Io {
                path: paths.active.clone(),
                source,
            });
        }
        remove_dir_if_exists(&backup)?;
        Ok(())
    }

    fn commit_state(
        &self,
        paths: &ScopePaths,
        previous_intent: &PluginIntentFile,
        previous_lock: &PluginLockFile,
        intent: &PluginIntentFile,
        lock: &PluginLockFile,
    ) -> Result<(), PluginManagerError> {
        write_json_atomic(&paths.intent, intent)?;
        if let Err(error) = write_json_atomic(&paths.lock, lock) {
            let _ = write_json_atomic(&paths.intent, previous_intent);
            return Err(error);
        }
        if let Err(error) = self.activate(paths, lock) {
            let _ = write_json_atomic(&paths.intent, previous_intent);
            let _ = write_json_atomic(&paths.lock, previous_lock);
            return Err(error);
        }
        Ok(())
    }

    fn scope_paths(&self, scope: InstallScope) -> ScopePaths {
        let base = match scope {
            InstallScope::Global => self.options.agent_dir.clone(),
            InstallScope::Project => self.options.cwd.join(".pi"),
        };
        ScopePaths {
            intent: base.join("plugins.json"),
            lock: base.join("plugins.lock"),
            guard: base.join("plugins.lock.guard"),
            store: base.join("plugins/store"),
            active: base.join("plugins/installed"),
        }
    }
}

#[derive(Debug)]
struct ScopePaths {
    intent: PathBuf,
    lock: PathBuf,
    guard: PathBuf,
    store: PathBuf,
    active: PathBuf,
}

struct StateGuard {
    file: fs::File,
}

impl StateGuard {
    fn acquire(path: &Path) -> Result<Self, PluginManagerError> {
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| PluginManagerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        file.lock_exclusive()
            .map_err(|source| PluginManagerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self { file })
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
struct ResolvedCandidate {
    id: String,
    version: Version,
    kind: String,
    source: String,
    target: String,
    sha256: String,
    artifact_name: String,
    artifact_bytes: Vec<u8>,
    options: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseManifest {
    schema: u32,
    id: String,
    version: String,
    kind: String,
    #[serde(default = "empty_options")]
    options: Value,
    artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseArtifact {
    target: String,
    url: String,
    sha256: String,
    #[serde(default)]
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryIndex {
    schema: u32,
    plugins: BTreeMap<String, Vec<RegistryRelease>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRelease {
    version: String,
    manifest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalManifest {
    schema: u32,
    plugin: LocalManifestPlugin,
    #[serde(default)]
    options: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalManifestPlugin {
    id: String,
    version: String,
    kind: String,
    artifact: PathBuf,
}

#[derive(Serialize)]
struct LoaderManifest<'a> {
    schema: u32,
    plugin: LoaderManifestPlugin<'a>,
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    options: toml::Table,
}

#[derive(Serialize)]
struct LoaderManifestPlugin<'a> {
    id: &'a str,
    version: &'a str,
    kind: &'a str,
    artifact: &'a str,
}

fn normalize_install_source(
    source: &str,
    version: Option<&str>,
) -> Result<(String, VersionReq), PluginManagerError> {
    if let Some(registry) = source.strip_prefix("registry:") {
        let (id, embedded) = registry
            .rsplit_once('@')
            .filter(|(id, requirement)| !id.is_empty() && !requirement.is_empty())
            .map_or((registry, None), |(id, requirement)| {
                (id, Some(requirement))
            });
        let requirement = version.or(embedded).unwrap_or("*");
        return Ok((
            format!("registry:{id}"),
            parse_requirement(requirement, id)?,
        ));
    }
    let requirement = version.unwrap_or("*");
    Ok((source.to_string(), parse_requirement(requirement, source)?))
}

fn canonical_local_source(source: &str, cwd: &Path) -> Result<String, PluginManagerError> {
    if source.starts_with("registry:")
        || source.starts_with("github:")
        || source.starts_with("https://")
        || source.starts_with("http://")
    {
        return Ok(source.to_string());
    }
    let path = source.strip_prefix("path:").unwrap_or(source);
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    if !path.exists() {
        return Ok(source.to_string());
    }
    path.canonicalize()
        .map(|path| format!("path:{}", path.display()))
        .map_err(|source| PluginManagerError::Io { path, source })
}

fn local_source_path(source: &str, cwd: &Path) -> Option<PathBuf> {
    if source.starts_with("registry:")
        || source.starts_with("github:")
        || source.starts_with("https://")
        || source.starts_with("http://")
    {
        return None;
    }
    let path = PathBuf::from(source.strip_prefix("path:").unwrap_or(source));
    Some(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn reject_discovered_source(source: &str, active: &Path) -> Result<(), PluginManagerError> {
    let Some(path) = source.strip_prefix("path:") else {
        return Ok(());
    };
    let discovery_root = active.parent().ok_or_else(|| {
        invalid_data(
            active.display().to_string(),
            "plugin activation directory has no discovery root",
        )
    })?;
    let normalized_root = discovery_root
        .canonicalize()
        .unwrap_or_else(|_| discovery_root.to_path_buf());
    if Path::new(path).starts_with(&normalized_root) {
        return Err(invalid_data(
            source,
            format!(
                "the source is already inside loader discovery root {}; install from a package outside that directory or use it directly",
                normalized_root.display()
            ),
        ));
    }
    Ok(())
}

fn resolve_local_package(
    path: &Path,
    source_label: &str,
    requirement: &VersionReq,
    target: &str,
) -> Result<ResolvedCandidate, PluginManagerError> {
    let manifest_path = if path.is_dir() {
        path.join("pi-plugin.toml")
    } else {
        path.to_path_buf()
    };
    let contents = fs::read_to_string(&manifest_path).map_err(|source| PluginManagerError::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest: LocalManifest = toml::from_str(&contents)
        .map_err(|error| invalid_data(manifest_path.display().to_string(), error.to_string()))?;
    if manifest.schema != RELEASE_SCHEMA {
        return Err(invalid_data(
            manifest_path.display().to_string(),
            format!("unsupported plugin manifest schema {}", manifest.schema),
        ));
    }
    validate_identity(
        &manifest.plugin.id,
        &manifest.plugin.kind,
        &manifest_path.display().to_string(),
    )?;
    let version = parse_version(&manifest.plugin.version, &manifest.plugin.id)?;
    if !requirement.matches(&version) {
        return Err(PluginManagerError::VersionNotFound {
            id: manifest.plugin.id,
            requirement: requirement.to_string(),
        });
    }
    if manifest.plugin.artifact.is_absolute()
        || manifest
            .plugin
            .artifact
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_data(
            manifest_path.display().to_string(),
            "artifact must be a relative path inside the package",
        ));
    }
    let package_dir = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|source| PluginManagerError::Io {
            path: manifest_path.clone(),
            source,
        })?;
    let artifact = package_dir.join(&manifest.plugin.artifact);
    let artifact = artifact
        .canonicalize()
        .map_err(|source| PluginManagerError::Io {
            path: artifact,
            source,
        })?;
    if !artifact.starts_with(&package_dir) {
        return Err(invalid_data(
            manifest_path.display().to_string(),
            "artifact escapes the package directory",
        ));
    }
    let artifact_name = artifact
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_data(artifact.display().to_string(), "artifact has no UTF-8 name"))?
        .to_string();
    validate_file_name(&artifact_name, &artifact.display().to_string())?;
    let artifact_bytes = fs::read(&artifact).map_err(|source| PluginManagerError::Io {
        path: artifact,
        source,
    })?;
    let sha256 = sha256_hex(&artifact_bytes);
    let options = serde_json::to_value(toml::Value::Table(manifest.options))
        .map_err(|error| invalid_data(source_label, error.to_string()))?;
    validate_options(&options, &manifest.plugin.id)?;
    Ok(ResolvedCandidate {
        id: manifest.plugin.id,
        version,
        kind: manifest.plugin.kind,
        source: source_label.to_string(),
        target: target.to_string(),
        sha256,
        artifact_name,
        artifact_bytes,
        options,
    })
}

fn materialize_store(
    store: &Path,
    candidate: &ResolvedCandidate,
) -> Result<(), PluginManagerError> {
    materialize_blob(store, &candidate.sha256, &candidate.artifact_bytes)
}

fn materialize_blob(
    store: &Path,
    expected_sha256: &str,
    bytes: &[u8],
) -> Result<(), PluginManagerError> {
    if store.is_file() {
        let stored = fs::read(store).map_err(|source| PluginManagerError::Io {
            path: store.to_path_buf(),
            source,
        })?;
        verify_hash(&stored, expected_sha256, &store.display().to_string())?;
        return Ok(());
    }
    if store.exists() {
        return Err(invalid_data(
            store.display().to_string(),
            "content-addressed store entry must be a file",
        ));
    }
    let parent = store.parent().ok_or_else(|| {
        invalid_data(
            store.display().to_string(),
            "content-addressed store has no parent",
        )
    })?;
    create_dir_all(parent)?;
    let stage = parent.join(format!(".{expected_sha256}-stage-{}", next_nonce()));
    remove_file_if_exists(&stage)?;
    fs::write(&stage, bytes).map_err(|source| PluginManagerError::Io {
        path: stage.clone(),
        source,
    })?;
    match fs::rename(&stage, store) {
        Ok(()) => Ok(()),
        Err(_) if store.is_file() => {
            remove_file_if_exists(&stage)?;
            let bytes = fs::read(store).map_err(|source| PluginManagerError::Io {
                path: store.to_path_buf(),
                source,
            })?;
            verify_hash(&bytes, expected_sha256, &store.display().to_string())
        }
        Err(source) => Err(PluginManagerError::Io {
            path: store.to_path_buf(),
            source,
        }),
    }
}

fn ensure_store_entries(
    paths: &ScopePaths,
    lock: &PluginLockFile,
) -> Result<bool, PluginManagerError> {
    for (index, plugin) in lock.plugins.iter().enumerate() {
        let store = paths.store.join("sha256").join(&plugin.sha256);
        if store.is_file() {
            continue;
        }
        if store.exists() {
            return Ok(false);
        }
        let installed = paths
            .active
            .join(format!("{index:04}-{}", safe_id(&plugin.id)))
            .join(&plugin.artifact);
        if !installed.is_file() {
            return Ok(false);
        }
        let bytes = fs::read(&installed).map_err(|source| PluginManagerError::Io {
            path: installed,
            source,
        })?;
        if sha256_hex(&bytes) != plugin.sha256 {
            return Ok(false);
        }
        materialize_blob(&store, &plugin.sha256, &bytes)?;
    }
    Ok(true)
}

fn write_loader_manifest(path: &Path, plugin: &LockedPlugin) -> Result<(), PluginManagerError> {
    let contents = loader_manifest_contents(plugin, path)?;
    fs::write(path, contents).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn loader_manifest_contents(
    plugin: &LockedPlugin,
    location: &Path,
) -> Result<String, PluginManagerError> {
    let options = options_to_toml(&plugin.options, &plugin.id)?;
    let manifest = LoaderManifest {
        schema: RELEASE_SCHEMA,
        plugin: LoaderManifestPlugin {
            id: &plugin.id,
            version: &plugin.version,
            kind: &plugin.kind,
            artifact: &plugin.artifact,
        },
        options,
    };
    toml::to_string_pretty(&manifest)
        .map_err(|error| invalid_data(location.display().to_string(), error.to_string()))
}

fn activation_current(
    paths: &ScopePaths,
    lock: &PluginLockFile,
) -> Result<bool, PluginManagerError> {
    if !paths.active.exists() {
        return Ok(lock.plugins.is_empty());
    }
    let mut visible_directories = 0_usize;
    for entry in fs::read_dir(&paths.active).map_err(|source| PluginManagerError::Io {
        path: paths.active.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| PluginManagerError::Io {
            path: paths.active.clone(),
            source,
        })?;
        if entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            visible_directories += 1;
        }
    }
    if visible_directories != lock.plugins.len() {
        return Ok(false);
    }
    for (index, plugin) in lock.plugins.iter().enumerate() {
        let package = paths
            .active
            .join(format!("{index:04}-{}", safe_id(&plugin.id)));
        let manifest_path = package.join("pi-plugin.toml");
        let artifact_path = package.join(&plugin.artifact);
        if !artifact_path.is_file() || !manifest_path.is_file() {
            return Ok(false);
        }
        let actual =
            fs::read_to_string(&manifest_path).map_err(|source| PluginManagerError::Io {
                path: manifest_path.clone(),
                source,
            })?;
        if actual != loader_manifest_contents(plugin, &manifest_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn options_to_toml(options: &Value, id: &str) -> Result<toml::Table, PluginManagerError> {
    validate_options(options, id)?;
    let value = toml::Value::try_from(options.clone())
        .map_err(|error| invalid_data(id, format!("options are not TOML-compatible: {error}")))?;
    value
        .try_into()
        .map_err(|error| invalid_data(id, format!("options must be an object: {error}")))
}

fn read_intent(path: &Path) -> Result<PluginIntentFile, PluginManagerError> {
    if !path.exists() {
        return Ok(PluginIntentFile::default());
    }
    let bytes = fs::read(path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let intent: PluginIntentFile = parse_json(&bytes, &path.display().to_string())?;
    if intent.schema != INTENT_SCHEMA {
        return Err(invalid_data(
            path.display().to_string(),
            format!("unsupported plugins.json schema {}", intent.schema),
        ));
    }
    let mut ids = HashSet::new();
    for plugin in &intent.plugins {
        validate_plugin_id(&plugin.id, &path.display().to_string())?;
        if !ids.insert(plugin.id.as_str()) {
            return Err(invalid_data(
                path.display().to_string(),
                format!("duplicate root plugin id {}", plugin.id),
            ));
        }
        if plugin.source.trim().is_empty() {
            return Err(invalid_data(
                path.display().to_string(),
                format!("plugin {} has an empty source", plugin.id),
            ));
        }
        parse_requirement(&plugin.version, &plugin.id)?;
        validate_options(&plugin.options, &plugin.id)?;
    }
    Ok(intent)
}

fn read_lock(path: &Path, target: &str) -> Result<PluginLockFile, PluginManagerError> {
    if !path.exists() {
        return Ok(PluginLockFile::empty(target));
    }
    let bytes = fs::read(path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let lock: PluginLockFile = parse_json(&bytes, &path.display().to_string())?;
    if lock.schema != LOCK_SCHEMA {
        return Err(invalid_data(
            path.display().to_string(),
            format!("unsupported plugins.lock schema {}", lock.schema),
        ));
    }
    validate_lock(&lock, path)?;
    Ok(lock)
}

fn validate_lock(lock: &PluginLockFile, path: &Path) -> Result<(), PluginManagerError> {
    let location = path.display().to_string();
    if let Some(digest) = &lock.intent_sha256 {
        validate_sha256(digest, &location)?;
    }
    let mut preceding = HashSet::new();
    for plugin in &lock.plugins {
        validate_identity(&plugin.id, &plugin.kind, &location)?;
        if !preceding.insert(plugin.id.as_str()) {
            return Err(invalid_data(
                &location,
                format!("duplicate locked plugin id {}", plugin.id),
            ));
        }
        parse_version(&plugin.version, &plugin.id)?;
        validate_sha256(&plugin.sha256, &location)?;
        validate_file_name(&plugin.artifact, &location)?;
        validate_options(&plugin.options, &plugin.id)?;
        if plugin.target != lock.target {
            return Err(invalid_data(
                &location,
                format!(
                    "plugin {} targets {}, but the lock targets {}",
                    plugin.id, plugin.target, lock.target
                ),
            ));
        }
    }
    Ok(())
}

fn validate_lock_target(
    lock: &PluginLockFile,
    target: &str,
    path: &Path,
) -> Result<(), PluginManagerError> {
    if !lock.plugins.is_empty() && lock.target != target {
        return Err(invalid_data(
            path.display().to_string(),
            format!(
                "plugins.lock targets {}, but this pi binary targets {target}",
                lock.target
            ),
        ));
    }
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), PluginManagerError> {
    let parent = path.parent().ok_or_else(|| {
        invalid_data(
            path.display().to_string(),
            "state file has no parent directory",
        )
    })?;
    create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugins"),
        next_nonce()
    ));
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| invalid_data(path.display().to_string(), error.to_string()))?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| PluginManagerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| PluginManagerError::Io {
            path: temporary.clone(),
            source,
        })?;
    fs::rename(&temporary, path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), PluginManagerError> {
    fs::create_dir_all(path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_dir_if_exists(path: &Path) -> Result<(), PluginManagerError> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_dir_all(path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_file_if_exists(path: &Path) -> Result<(), PluginManagerError> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).map_err(|source| PluginManagerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn link_or_copy(source: &Path, target: &Path) -> Result<(), PluginManagerError> {
    if fs::hard_link(source, target).is_ok() {
        return Ok(());
    }
    fs::copy(source, target)
        .map(|_| ())
        .map_err(|source_error| PluginManagerError::Io {
            path: target.to_path_buf(),
            source: source_error,
        })
}

fn resolve_url(base: &str, reference: &str) -> Result<String, PluginManagerError> {
    if let Ok(url) = Url::parse(reference) {
        return Ok(url.to_string());
    }
    Url::parse(base)
        .and_then(|base| base.join(reference))
        .map(String::from)
        .map_err(|error| invalid_data(base, error.to_string()))
}

fn parse_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    source: &str,
) -> Result<T, PluginManagerError> {
    serde_json::from_slice(bytes).map_err(|error| invalid_data(source, error.to_string()))
}

fn parse_version(version: &str, id: &str) -> Result<Version, PluginManagerError> {
    Version::parse(version).map_err(|error| invalid_data(id, error.to_string()))
}

fn parse_requirement(requirement: &str, id: &str) -> Result<VersionReq, PluginManagerError> {
    VersionReq::parse(requirement).map_err(|error| invalid_data(id, error.to_string()))
}

fn exact_requirement(version: &Version) -> VersionReq {
    VersionReq::parse(&format!("={version}")).expect("an exact semantic version is a requirement")
}

fn locked_versions(lock: &PluginLockFile) -> Result<HashMap<String, Version>, PluginManagerError> {
    lock.plugins
        .iter()
        .map(|plugin| {
            Ok((
                plugin.id.clone(),
                parse_version(&plugin.version, &plugin.id)?,
            ))
        })
        .collect()
}

fn intent_digest(intent: &PluginIntentFile) -> Result<String, PluginManagerError> {
    serde_json::to_vec(intent)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|error| invalid_data("plugins.json", error.to_string()))
}

fn validate_identity(id: &str, kind: &str, source: &str) -> Result<(), PluginManagerError> {
    validate_plugin_id(id, source)?;
    if !matches!(kind, "agent" | "provider" | "session") {
        return Err(invalid_data(
            source,
            "plugin kind must be agent, provider, or session",
        ));
    }
    Ok(())
}

fn validate_plugin_id(id: &str, source: &str) -> Result<(), PluginManagerError> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && id
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return Ok(());
    }
    Err(invalid_data(
        source,
        "plugin id must be lowercase kebab-case with alphanumeric ends",
    ))
}

fn validate_options(options: &Value, id: &str) -> Result<(), PluginManagerError> {
    if options.is_object() {
        Ok(())
    } else {
        Err(invalid_data(id, "plugin options must be a JSON object"))
    }
}

fn validate_file_name(name: &str, source: &str) -> Result<(), PluginManagerError> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || matches!(
            path.components().next(),
            Some(Component::ParentDir | Component::CurDir)
        )
    {
        return Err(invalid_data(
            source,
            "artifact file name must be a basename",
        ));
    }
    Ok(())
}

fn validate_sha256(hash: &str, source: &str) -> Result<(), PluginManagerError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(invalid_data(
            source,
            "sha256 must contain 64 hexadecimal characters",
        ))
    }
}

fn verify_hash(bytes: &[u8], expected: &str, source: &str) -> Result<(), PluginManagerError> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(PluginManagerError::HashMismatch {
            location: source.to_string(),
            expected: expected.to_ascii_lowercase(),
            actual,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn invalid_data(source: impl Into<String>, message: impl Into<String>) -> PluginManagerError {
    PluginManagerError::InvalidData {
        location: source.into(),
        message: message.into(),
    }
}

fn empty_options() -> Value {
    Value::Object(serde_json::Map::new())
}

fn merge_options(defaults: &Value, overrides: &Value) -> Value {
    let (Value::Object(defaults), Value::Object(overrides)) = (defaults, overrides) else {
        return overrides.clone();
    };
    let mut merged = defaults.clone();
    for (key, override_value) in overrides {
        let value = merged.get(key).map_or_else(
            || override_value.clone(),
            |default_value| merge_options(default_value, override_value),
        );
        merged.insert(key.clone(), value);
    }
    Value::Object(merged)
}

fn any_version() -> String {
    "*".to_string()
}

fn safe_id(id: &str) -> String {
    id.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'-' {
                char::from(byte)
            } else {
                '-'
            }
        })
        .collect()
}

fn next_nonce() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn write_local_package(root: &Path, id: &str, contents: &[u8]) -> PathBuf {
        let package = root.join(id);
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("plugin.dylib"), contents).unwrap();
        fs::write(
            package.join("pi-plugin.toml"),
            format!(
                r#"schema = 1

[plugin]
id = "{id}"
version = "1.2.3"
kind = "agent"
artifact = "plugin.dylib"
"#
            ),
        )
        .unwrap();
        package
    }

    #[tokio::test]
    async fn local_install_writes_intent_lock_cas_and_activation_view() {
        let root = tempfile::tempdir().unwrap();
        let package = write_local_package(root.path(), "frontend-check", b"native plugin");
        let mut options = PluginManagerOptions::new(root.path(), root.path().join("agent"));
        options.target = "aarch64-apple-darwin".to_string();
        let manager = PluginManager::new(options).unwrap();

        let installed = manager
            .install(package.display().to_string(), None, InstallScope::Project)
            .await
            .unwrap();

        assert_eq!(installed.id, "frontend-check");
        assert_eq!(installed.version, "1.2.3");
        let intent: PluginIntentFile =
            serde_json::from_slice(&fs::read(root.path().join(".pi/plugins.json")).unwrap())
                .unwrap();
        assert_eq!(intent.plugins[0].id, "frontend-check");
        let lock: PluginLockFile =
            serde_json::from_slice(&fs::read(root.path().join(".pi/plugins.lock")).unwrap())
                .unwrap();
        assert_eq!(lock.target, "aarch64-apple-darwin");
        assert!(
            root.path()
                .join(".pi/plugins/store/sha256")
                .join(&installed.sha256)
                .is_file()
        );
        assert!(
            root.path()
                .join(".pi/plugins/installed/0000-frontend-check/pi-plugin.toml")
                .is_file()
        );
    }

    #[tokio::test]
    async fn cas_identity_does_not_depend_on_the_publisher_file_name() {
        let root = tempfile::tempdir().unwrap();
        for (id, artifact) in [("first", "first.dylib"), ("second", "second.dylib")] {
            let package = root.path().join(id);
            fs::create_dir_all(&package).unwrap();
            fs::write(package.join(artifact), b"identical native bytes").unwrap();
            fs::write(
                package.join("pi-plugin.toml"),
                format!(
                    r#"schema = 1

[plugin]
id = "{id}"
version = "1.0.0"
kind = "agent"
artifact = "{artifact}"
"#
                ),
            )
            .unwrap();
        }
        let manager = PluginManager::new(PluginManagerOptions::new(
            root.path(),
            root.path().join("agent"),
        ))
        .unwrap();

        manager
            .install(
                root.path().join("first").display().to_string(),
                None,
                InstallScope::Global,
            )
            .await
            .unwrap();
        manager
            .install(
                root.path().join("second").display().to_string(),
                None,
                InstallScope::Global,
            )
            .await
            .unwrap();

        let active = root.path().join("agent/plugins/installed");
        assert!(active.join("0000-first/first.dylib").is_file());
        assert!(active.join("0001-second/second.dylib").is_file());
        let store = root.path().join("agent/plugins/store/sha256");
        assert_eq!(fs::read_dir(store).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn package_option_defaults_survive_empty_consumer_overrides() {
        let root = tempfile::tempdir().unwrap();
        let package = write_local_package(root.path(), "configured-plugin", b"native plugin");
        fs::write(
            package.join("pi-plugin.toml"),
            r#"schema = 1

[plugin]
id = "configured-plugin"
version = "1.2.3"
kind = "agent"
artifact = "plugin.dylib"

[options]
command = "frontend-check"

[options.nested]
default = true
overridden = false
"#,
        )
        .unwrap();
        let agent_dir = root.path().join("agent");
        let manager =
            PluginManager::new(PluginManagerOptions::new(root.path(), &agent_dir)).unwrap();

        manager
            .install(package.display().to_string(), None, InstallScope::Global)
            .await
            .unwrap();
        let activation = fs::read_to_string(
            agent_dir.join("plugins/installed/0000-configured-plugin/pi-plugin.toml"),
        )
        .unwrap();

        assert!(activation.contains("command = \"frontend-check\""));
        assert!(activation.contains("default = true"));
        assert!(activation.contains("overridden = false"));
    }

    #[test]
    fn consumer_options_recursively_override_package_defaults() {
        let defaults = serde_json::json!({
            "command": "frontend-check",
            "nested": {"preserved": true, "changed": "default"}
        });
        let overrides = serde_json::json!({
            "nested": {"changed": "consumer"}
        });

        assert_eq!(
            merge_options(&defaults, &overrides),
            serde_json::json!({
                "command": "frontend-check",
                "nested": {"preserved": true, "changed": "consumer"}
            })
        );
    }

    #[tokio::test]
    async fn prepared_reconcile_applies_options_rolls_back_and_repairs_activation() {
        let root = tempfile::tempdir().unwrap();
        let package = write_local_package(root.path(), "configured-plugin", b"native plugin");
        fs::write(
            package.join("pi-plugin.toml"),
            r#"schema = 1

[plugin]
id = "configured-plugin"
version = "1.2.3"
kind = "agent"
artifact = "plugin.dylib"

[options]
command = "frontend-check"
guidance = "default"
"#,
        )
        .unwrap();
        let agent_dir = root.path().join("agent");
        let manager =
            PluginManager::new(PluginManagerOptions::new(root.path(), &agent_dir)).unwrap();
        manager
            .install(package.display().to_string(), None, InstallScope::Global)
            .await
            .unwrap();
        let paths = manager.scope_paths(InstallScope::Global);
        let previous_lock = fs::read(&paths.lock).unwrap();
        let previous_activation =
            fs::read_to_string(paths.active.join("0000-configured-plugin/pi-plugin.toml")).unwrap();
        let mut intent = read_intent(&paths.intent).unwrap();
        intent.plugins[0].options = serde_json::json!({"guidance": "project"});
        write_json_atomic(&paths.intent, &intent).unwrap();

        {
            let prepared = manager
                .prepare_reconcile(InstallScope::Global)
                .await
                .unwrap();
            assert_eq!(prepared.status(), ReconcileStatus::Updated);
            let activation =
                fs::read_to_string(paths.active.join("0000-configured-plugin/pi-plugin.toml"))
                    .unwrap();
            assert!(activation.contains("command = \"frontend-check\""));
            assert!(activation.contains("guidance = \"project\""));
        }
        assert_eq!(fs::read(&paths.lock).unwrap(), previous_lock);
        assert_eq!(
            fs::read_to_string(paths.active.join("0000-configured-plugin/pi-plugin.toml")).unwrap(),
            previous_activation
        );

        let prepared = manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(prepared.status(), ReconcileStatus::Updated);
        prepared.commit();
        let prepared = manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(prepared.status(), ReconcileStatus::Unchanged);
        prepared.commit();

        let lock = read_lock(&paths.lock, &manager.options.target).unwrap();
        let store = paths.store.join("sha256").join(&lock.plugins[0].sha256);
        fs::remove_file(&store).unwrap();
        let prepared = manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(prepared.status(), ReconcileStatus::Unchanged);
        assert!(store.is_file());
        prepared.commit();

        fs::write(package.join("plugin.dylib"), b"rebuilt native plugin").unwrap();
        let prepared = manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(prepared.status(), ReconcileStatus::Updated);
        prepared.commit();

        fs::remove_dir_all(&paths.active).unwrap();
        let prepared = manager
            .prepare_reconcile(InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(prepared.status(), ReconcileStatus::Repaired);
        assert!(
            paths
                .active
                .join("0000-configured-plugin/pi-plugin.toml")
                .is_file()
        );
        prepared.commit();
    }

    #[tokio::test]
    async fn removing_a_plugin_preserves_the_remaining_declared_order() {
        let root = tempfile::tempdir().unwrap();
        let first = write_local_package(root.path(), "first", b"first artifact");
        let second = write_local_package(root.path(), "second", b"second artifact");
        let manager = PluginManager::new(PluginManagerOptions::new(
            root.path(),
            root.path().join("agent"),
        ))
        .unwrap();
        manager
            .install(first.display().to_string(), None, InstallScope::Global)
            .await
            .unwrap();
        manager
            .install(second.display().to_string(), None, InstallScope::Global)
            .await
            .unwrap();

        manager.remove("first", InstallScope::Global).unwrap();

        let remaining = manager.list(InstallScope::Global).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|plugin| plugin.id.as_str())
                .collect::<Vec<_>>(),
            ["second"]
        );
        assert!(
            root.path()
                .join("agent/plugins/installed/0000-second/plugin.dylib")
                .is_file()
        );
    }

    #[tokio::test]
    async fn package_manifest_rejects_runtime_plugin_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let package = write_local_package(root.path(), "dependent", b"native plugin");
        let mut manifest = fs::read_to_string(package.join("pi-plugin.toml")).unwrap();
        manifest.push_str("\n[dependencies]\nshared = \"^1\"\n");
        fs::write(package.join("pi-plugin.toml"), manifest).unwrap();
        let manager = PluginManager::new(PluginManagerOptions::new(
            root.path(),
            root.path().join("agent"),
        ))
        .unwrap();

        let error = manager
            .install(package.display().to_string(), None, InstallScope::Global)
            .await
            .unwrap_err();

        assert!(matches!(error, PluginManagerError::InvalidData { .. }));
    }

    #[tokio::test]
    async fn static_registry_selects_the_highest_version_and_host_artifact() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let frontend = b"frontend native artifact";
        let routes = HashMap::from([
            (
                "/index.json".to_string(),
                serde_json::to_vec(&serde_json::json!({
                    "schema": 1,
                    "plugins": {
                        "frontend-check": [
                            {"version": "1.0.0", "manifest": "frontend-1.0.json"},
                            {"version": "1.2.0", "manifest": "frontend-1.2.json"}
                        ]
                    }
                }))
                .unwrap(),
            ),
            (
                "/frontend-1.2.json".to_string(),
                serde_json::to_vec(&serde_json::json!({
                    "schema": 1,
                    "id": "frontend-check",
                    "version": "1.2.0",
                    "kind": "agent",
                    "artifacts": [{
                        "target": "test-target",
                        "url": "frontend.dylib",
                        "sha256": sha256_hex(frontend)
                    }]
                }))
                .unwrap(),
            ),
            ("/frontend.dylib".to_string(), frontend.to_vec()),
        ]);
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 8192];
                let size = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                let (status, body) = routes
                    .get(path)
                    .map_or(("404 Not Found", Vec::new()), |body| {
                        ("200 OK", body.clone())
                    });
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let mut options = PluginManagerOptions::new(root.path(), root.path().join("agent"));
        options.target = "test-target".to_string();
        options.registry = Some(format!("{base}/index.json"));
        let manager = PluginManager::new(options).unwrap();

        let installed = manager
            .install("registry:frontend-check@^1", None, InstallScope::Global)
            .await
            .unwrap();

        assert_eq!(installed.version, "1.2.0");
        assert_eq!(
            manager
                .list(InstallScope::Global)
                .unwrap()
                .iter()
                .map(|plugin| (plugin.id.as_str(), plugin.version.as_str()))
                .collect::<Vec<_>>(),
            [("frontend-check", "1.2.0")]
        );
        assert_eq!(manager.sync(InstallScope::Global).await.unwrap().len(), 1);
        server.abort();
    }

    #[test]
    fn artifact_hash_mismatch_is_rejected() {
        let expected = sha256_hex(b"expected artifact");

        let error =
            verify_hash(b"substituted artifact", &expected, "release artifact").unwrap_err();

        assert!(matches!(
            error,
            PluginManagerError::HashMismatch {
                location,
                expected: actual_expected,
                ..
            } if location == "release artifact" && actual_expected == expected
        ));
    }

    #[test]
    fn lock_artifact_paths_are_validated_before_use() {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("agent");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("plugins.lock"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "target": HOST_TARGET,
                "plugins": [{
                    "id": "unsafe-plugin",
                    "version": "1.0.0",
                    "kind": "agent",
                    "source": "fixture",
                    "target": HOST_TARGET,
                    "sha256": "../../outside",
                    "artifact": "plugin.dylib",
                    "options": {}
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let manager =
            PluginManager::new(PluginManagerOptions::new(root.path(), &agent_dir)).unwrap();

        let error = manager.list(InstallScope::Global).unwrap_err();

        assert!(matches!(error, PluginManagerError::InvalidData { .. }));
    }
}
