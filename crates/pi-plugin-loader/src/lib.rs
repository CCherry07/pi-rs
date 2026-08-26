#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::{HashMap, HashSet};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::{Library, Symbol};
use pi_core::{AgentPlugin, ProviderPlugin};
use pi_plugin_sdk::{
    AgentPluginCreateV2, BUILD_FINGERPRINT, NATIVE_PLUGIN_ABI_VERSION, NativePluginKind,
    PluginDescriptorFnV1, PluginLoadContext, PluginOptionsValue, PluginScope,
    ProviderPluginCreateV2, SessionPluginCreateV2,
};
use pi_runtime::PiRuntimeBuilder;
use pi_session::{SessionPlugin, SessionPlugins};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const DESCRIPTOR_SYMBOL: &[u8] = b"pi_plugin_descriptor_v1\0";
const AGENT_CONSTRUCTOR_SYMBOL: &[u8] = b"pi_agent_plugin_create_v2\0";
const PROVIDER_CONSTRUCTOR_SYMBOL: &[u8] = b"pi_provider_plugin_create_v2\0";
const SESSION_CONSTRUCTOR_SYMBOL: &[u8] = b"pi_session_plugin_create_v2\0";
const MANIFEST_FILE_NAME: &str = "pi-plugin.toml";

#[derive(Debug, thiserror::Error)]
pub enum NativePluginError {
    #[error("cannot access native plugin path {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse native plugin manifest {path}: {message}")]
    ManifestParse { path: PathBuf, message: String },
    #[error("unsupported native plugin manifest schema {schema} at {path}")]
    ManifestSchema { path: PathBuf, schema: u32 },
    #[error("native plugin artifact must be a relative path inside {package_dir}: {artifact}")]
    InvalidArtifactPath {
        package_dir: PathBuf,
        artifact: PathBuf,
    },
    #[error("failed to load native plugin {path}: {message}")]
    LibraryLoad { path: PathBuf, message: String },
    #[error("native plugin {path} does not export {symbol}: {message}")]
    MissingSymbol {
        path: PathBuf,
        symbol: &'static str,
        message: String,
    },
    #[error("native plugin {path} returned a null descriptor")]
    NullDescriptor { path: PathBuf },
    #[error("native plugin {path} uses ABI {actual}; host requires ABI {expected}")]
    AbiMismatch {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("native plugin {path} build fingerprint {actual} is incompatible with host {expected}")]
    BuildMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("native plugin {path} has invalid descriptor field {field}: {message}")]
    InvalidDescriptor {
        path: PathBuf,
        field: &'static str,
        message: String,
    },
    #[error("native plugin manifest {path} declares {field}={declared}, binary reports {actual}")]
    ManifestMismatch {
        path: PathBuf,
        field: &'static str,
        declared: String,
        actual: String,
    },
    #[error("duplicate native {kind} plugin id: {id}")]
    DuplicatePlugin { kind: &'static str, id: String },
    #[error("native plugin {id} failed to initialize: {message}")]
    Initialization { id: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePluginDescriptor {
    pub id: String,
    pub version: String,
    pub kind: NativePluginKind,
    pub artifact: PathBuf,
}

#[derive(Debug, Clone)]
pub struct NativePluginLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
    pub explicit_paths: Vec<PathBuf>,
}

impl NativePluginLoaderOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            project_trusted: false,
            explicit_paths: Vec::new(),
        }
    }
}

pub struct NativePluginLoader {
    options: NativePluginLoaderOptions,
}

impl NativePluginLoader {
    pub fn new(options: NativePluginLoaderOptions) -> Self {
        Self { options }
    }

    pub fn discover(&self) -> Result<NativePlugins, NativePluginError> {
        let mut sources = Vec::new();
        for manifest in discover_manifests(&self.options.agent_dir.join("plugins"))? {
            sources.push(PluginSource {
                path: manifest,
                scope: PluginScope::Global,
            });
        }
        if self.options.project_trusted {
            let root = self.options.cwd.join(".pi/plugins");
            for manifest in discover_manifests(&root)? {
                sources.push(PluginSource {
                    path: manifest,
                    scope: PluginScope::Project {
                        root: self.options.cwd.clone(),
                    },
                });
            }
        }
        sources.extend(
            self.options
                .explicit_paths
                .iter()
                .cloned()
                .map(|path| PluginSource {
                    path,
                    scope: PluginScope::ExplicitPath,
                }),
        );

        let mut plugins = NativePlugins::default();
        let mut identities = HashSet::new();
        for source in sources {
            let loaded = self.load_source(source)?;
            let descriptor = loaded.descriptor().clone();
            let identity = (descriptor.kind, descriptor.id.clone());
            if !identities.insert(identity) {
                return Err(NativePluginError::DuplicatePlugin {
                    kind: kind_name(descriptor.kind),
                    id: descriptor.id,
                });
            }
            match loaded {
                LoadedFactory::Agent(factory) => plugins.agent.push(factory),
                LoadedFactory::Provider(factory) => plugins.provider.push(factory),
                LoadedFactory::Session(factory) => plugins.session.push(factory),
            }
        }
        Ok(plugins)
    }

    fn load_source(&self, source: PluginSource) -> Result<LoadedFactory, NativePluginError> {
        let path = if source.path.is_dir() {
            source.path.join(MANIFEST_FILE_NAME)
        } else {
            source.path
        };
        let is_manifest = path
            .file_name()
            .is_some_and(|name| name == MANIFEST_FILE_NAME);
        let resolved = if is_manifest {
            resolve_manifest(&path)?
        } else {
            ResolvedPackage {
                artifact: canonicalize(&path)?,
                package_dir: canonicalize(path.parent().unwrap_or_else(|| Path::new(".")))?,
                options: PluginOptionsValue::Object(serde_json::Map::new()),
                expected: None,
            }
        };
        load_library(
            resolved,
            &self.options.cwd,
            &self.options.agent_dir,
            source.scope,
        )
    }
}

#[derive(Default)]
pub struct NativePlugins {
    agent: Vec<NativeAgentPluginFactory>,
    provider: Vec<NativeProviderPluginFactory>,
    session: Vec<NativeSessionPluginFactory>,
}

impl NativePlugins {
    pub fn is_empty(&self) -> bool {
        self.agent.is_empty() && self.provider.is_empty() && self.session.is_empty()
    }

    pub fn descriptors(&self) -> Vec<NativePluginDescriptor> {
        self.agent
            .iter()
            .map(NativeAgentPluginFactory::descriptor)
            .chain(
                self.provider
                    .iter()
                    .map(NativeProviderPluginFactory::descriptor),
            )
            .chain(
                self.session
                    .iter()
                    .map(NativeSessionPluginFactory::descriptor),
            )
            .cloned()
            .collect()
    }

    pub fn apply_runtime(&self, mut builder: PiRuntimeBuilder) -> PiRuntimeBuilder {
        for factory in &self.agent {
            let factory = factory.clone();
            builder = builder.try_agent_plugin_arc_factory(move || factory.create());
        }
        for factory in &self.provider {
            let factory = factory.clone();
            builder = builder.try_provider_plugin_arc_factory(move || factory.create());
        }
        builder
    }

    pub fn apply_session(&self, mut plugins: SessionPlugins) -> SessionPlugins {
        for factory in &self.session {
            let factory = factory.clone();
            plugins = plugins.try_plugin_arc_factory(move || factory.create());
        }
        plugins
    }

    pub fn agent_factories(&self) -> &[NativeAgentPluginFactory] {
        &self.agent
    }

    pub fn provider_factories(&self) -> &[NativeProviderPluginFactory] {
        &self.provider
    }

    pub fn session_factories(&self) -> &[NativeSessionPluginFactory] {
        &self.session
    }
}

struct PinnedLibrary {
    _library: ManuallyDrop<Library>,
}

struct FactoryCommon {
    _library: Arc<PinnedLibrary>,
    descriptor: NativePluginDescriptor,
    context: PluginLoadContext,
    options: PluginOptionsValue,
    next_generation: AtomicU64,
}

impl FactoryCommon {
    fn context(&self) -> PluginLoadContext {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.context.for_generation(generation)
    }
}

#[derive(Clone)]
pub struct NativeAgentPluginFactory {
    common: Arc<FactoryCommon>,
    create: AgentPluginCreateV2,
}

impl NativeAgentPluginFactory {
    pub fn descriptor(&self) -> &NativePluginDescriptor {
        &self.common.descriptor
    }

    pub fn create(&self) -> Result<Arc<dyn AgentPlugin>, NativePluginError> {
        (self.create)(&self.common.context(), &self.common.options).map_err(|error| {
            NativePluginError::Initialization {
                id: self.common.descriptor.id.clone(),
                message: error.to_string(),
            }
        })
    }
}

#[derive(Clone)]
pub struct NativeProviderPluginFactory {
    common: Arc<FactoryCommon>,
    create: ProviderPluginCreateV2,
}

impl NativeProviderPluginFactory {
    pub fn descriptor(&self) -> &NativePluginDescriptor {
        &self.common.descriptor
    }

    pub fn create(&self) -> Result<Arc<dyn ProviderPlugin>, NativePluginError> {
        (self.create)(&self.common.context(), &self.common.options).map_err(|error| {
            NativePluginError::Initialization {
                id: self.common.descriptor.id.clone(),
                message: error.to_string(),
            }
        })
    }
}

#[derive(Clone)]
pub struct NativeSessionPluginFactory {
    common: Arc<FactoryCommon>,
    create: SessionPluginCreateV2,
}

impl NativeSessionPluginFactory {
    pub fn descriptor(&self) -> &NativePluginDescriptor {
        &self.common.descriptor
    }

    pub fn create(&self) -> Result<Arc<dyn SessionPlugin>, NativePluginError> {
        (self.create)(&self.common.context(), &self.common.options).map_err(|error| {
            NativePluginError::Initialization {
                id: self.common.descriptor.id.clone(),
                message: error.to_string(),
            }
        })
    }
}

enum LoadedFactory {
    Agent(NativeAgentPluginFactory),
    Provider(NativeProviderPluginFactory),
    Session(NativeSessionPluginFactory),
}

impl LoadedFactory {
    fn descriptor(&self) -> &NativePluginDescriptor {
        match self {
            Self::Agent(factory) => factory.descriptor(),
            Self::Provider(factory) => factory.descriptor(),
            Self::Session(factory) => factory.descriptor(),
        }
    }
}

struct PluginSource {
    path: PathBuf,
    scope: PluginScope,
}

struct ResolvedPackage {
    artifact: PathBuf,
    package_dir: PathBuf,
    options: PluginOptionsValue,
    expected: Option<ExpectedDescriptor>,
}

struct ExpectedDescriptor {
    manifest_path: PathBuf,
    id: String,
    version: String,
    kind: NativePluginKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    plugin: ManifestPlugin,
    #[serde(default)]
    options: toml::Table,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPlugin {
    id: String,
    version: String,
    kind: String,
    artifact: PathBuf,
}

fn resolve_manifest(path: &Path) -> Result<ResolvedPackage, NativePluginError> {
    let contents = std::fs::read_to_string(path).map_err(|source| NativePluginError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let manifest: Manifest =
        toml::from_str(&contents).map_err(|error| NativePluginError::ManifestParse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if manifest.schema != 1 {
        return Err(NativePluginError::ManifestSchema {
            path: path.to_path_buf(),
            schema: manifest.schema,
        });
    }
    let kind =
        parse_kind(&manifest.plugin.kind).ok_or_else(|| NativePluginError::ManifestParse {
            path: path.to_path_buf(),
            message: format!(
                "unknown plugin kind {}; expected agent, provider, or session",
                manifest.plugin.kind
            ),
        })?;
    let package_dir = canonicalize(path.parent().unwrap_or_else(|| Path::new(".")))?;
    if manifest.plugin.artifact.is_absolute() {
        return Err(NativePluginError::InvalidArtifactPath {
            package_dir,
            artifact: manifest.plugin.artifact,
        });
    }
    let artifact = canonicalize(&package_dir.join(&manifest.plugin.artifact))?;
    if !artifact.starts_with(&package_dir) {
        return Err(NativePluginError::InvalidArtifactPath {
            package_dir,
            artifact,
        });
    }
    let options = serde_json::to_value(toml::Value::Table(manifest.options)).map_err(|error| {
        NativePluginError::ManifestParse {
            path: path.to_path_buf(),
            message: format!("options cannot be represented as JSON: {error}"),
        }
    })?;
    Ok(ResolvedPackage {
        artifact,
        package_dir,
        options,
        expected: Some(ExpectedDescriptor {
            manifest_path: path.to_path_buf(),
            id: manifest.plugin.id,
            version: manifest.plugin.version,
            kind,
        }),
    })
}

fn load_library(
    package: ResolvedPackage,
    cwd: &Path,
    agent_dir: &Path,
    scope: PluginScope,
) -> Result<LoadedFactory, NativePluginError> {
    let load_path = snapshot_artifact(&package.artifact, agent_dir)?;
    let pinned = pinned_library(&load_path, &package.artifact)?;
    let raw_descriptor = {
        // SAFETY: Symbol lookup is immediately copied while `pinned` is live.
        let descriptor: Symbol<'_, PluginDescriptorFnV1> = unsafe {
            pinned._library.get(DESCRIPTOR_SYMBOL).map_err(|error| {
                NativePluginError::MissingSymbol {
                    path: package.artifact.clone(),
                    symbol: "pi_plugin_descriptor_v1",
                    message: error.to_string(),
                }
            })?
        };
        // SAFETY: The C-layout descriptor function has no inputs. The returned
        // pointer is validated before dereference.
        let pointer = unsafe { descriptor() };
        if pointer.is_null() {
            return Err(NativePluginError::NullDescriptor {
                path: package.artifact,
            });
        }
        // SAFETY: Export macros return a pointer to an immutable static
        // descriptor which remains valid while the library is pinned.
        unsafe { *pointer }
    };
    verify_abi_version(&package.artifact, raw_descriptor.abi_version)?;
    let kind = NativePluginKind::from_u32(raw_descriptor.kind).ok_or_else(|| {
        NativePluginError::InvalidDescriptor {
            path: package.artifact.clone(),
            field: "kind",
            message: raw_descriptor.kind.to_string(),
        }
    })?;
    let id = descriptor_string(&package.artifact, "id", raw_descriptor.id)?;
    validate_plugin_id(&package.artifact, &id)?;
    let version = descriptor_string(&package.artifact, "version", raw_descriptor.version)?;
    let fingerprint = descriptor_string(
        &package.artifact,
        "build_fingerprint",
        raw_descriptor.build_fingerprint,
    )?;
    if fingerprint != BUILD_FINGERPRINT {
        return Err(NativePluginError::BuildMismatch {
            path: package.artifact,
            expected: BUILD_FINGERPRINT.to_string(),
            actual: fingerprint,
        });
    }
    if let Some(expected) = &package.expected {
        verify_manifest(expected, &id, &version, kind)?;
    }

    let descriptor = NativePluginDescriptor {
        id: id.clone(),
        version,
        kind,
        artifact: package.artifact.clone(),
    };
    let data_dir = agent_dir.join("plugin-data").join(&id);
    let cache_dir = agent_dir.join("cache/plugins/runtime").join(&id);
    create_dir_all(&data_dir)?;
    create_dir_all(&cache_dir)?;
    let context = PluginLoadContext::new(cwd, &package.package_dir, data_dir, cache_dir, scope, 0);
    let common = Arc::new(FactoryCommon {
        _library: Arc::clone(&pinned),
        descriptor,
        context,
        options: package.options,
        next_generation: AtomicU64::new(0),
    });

    match kind {
        NativePluginKind::Agent => {
            // SAFETY: The exact build fingerprint was verified before resolving
            // this Rust-ABI constructor, and the library remains pinned.
            let create = unsafe {
                load_symbol::<AgentPluginCreateV2>(
                    &pinned._library,
                    &package.artifact,
                    AGENT_CONSTRUCTOR_SYMBOL,
                    "pi_agent_plugin_create_v2",
                )?
            };
            Ok(LoadedFactory::Agent(NativeAgentPluginFactory {
                common,
                create,
            }))
        }
        NativePluginKind::Provider => {
            // SAFETY: See the agent constructor branch above.
            let create = unsafe {
                load_symbol::<ProviderPluginCreateV2>(
                    &pinned._library,
                    &package.artifact,
                    PROVIDER_CONSTRUCTOR_SYMBOL,
                    "pi_provider_plugin_create_v2",
                )?
            };
            Ok(LoadedFactory::Provider(NativeProviderPluginFactory {
                common,
                create,
            }))
        }
        NativePluginKind::Session => {
            // SAFETY: See the agent constructor branch above.
            let create = unsafe {
                load_symbol::<SessionPluginCreateV2>(
                    &pinned._library,
                    &package.artifact,
                    SESSION_CONSTRUCTOR_SYMBOL,
                    "pi_session_plugin_create_v2",
                )?
            };
            Ok(LoadedFactory::Session(NativeSessionPluginFactory {
                common,
                create,
            }))
        }
    }
}

fn verify_abi_version(path: &Path, actual: u32) -> Result<(), NativePluginError> {
    if actual == NATIVE_PLUGIN_ABI_VERSION {
        Ok(())
    } else {
        Err(NativePluginError::AbiMismatch {
            path: path.to_path_buf(),
            expected: NATIVE_PLUGIN_ABI_VERSION,
            actual,
        })
    }
}

fn snapshot_artifact(artifact: &Path, agent_dir: &Path) -> Result<PathBuf, NativePluginError> {
    let contents = std::fs::read(artifact).map_err(|source| NativePluginError::Io {
        path: artifact.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let digest = format!("{:x}", hasher.finalize());
    let file_name = artifact
        .file_name()
        .ok_or_else(|| NativePluginError::InvalidArtifactPath {
            package_dir: artifact
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            artifact: artifact.to_path_buf(),
        })?;
    let snapshot_dir = agent_dir.join("cache/plugins/artifacts").join(digest);
    create_dir_all(&snapshot_dir)?;
    let snapshot = snapshot_dir.join(file_name);
    if snapshot.exists() {
        return Ok(snapshot);
    }

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let temporary = snapshot_dir.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temporary, contents).map_err(|source| NativePluginError::Io {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = std::fs::rename(&temporary, &snapshot) {
        if snapshot.exists() {
            let _ = std::fs::remove_file(&temporary);
        } else {
            return Err(NativePluginError::Io {
                path: snapshot,
                source,
            });
        }
    }
    Ok(snapshot)
}

fn pinned_library(
    load_path: &Path,
    source_path: &Path,
) -> Result<Arc<PinnedLibrary>, NativePluginError> {
    static LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Arc<PinnedLibrary>>>> = OnceLock::new();
    let libraries = LIBRARIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut libraries = libraries
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(library) = libraries.get(load_path) {
        return Ok(Arc::clone(library));
    }

    // SAFETY: Loading trusted native code is the explicit purpose of this
    // adapter. Only the C-layout descriptor is used before exact-build
    // compatibility has been checked.
    let library =
        unsafe { Library::new(load_path) }.map_err(|error| NativePluginError::LibraryLoad {
            path: source_path.to_path_buf(),
            message: error.to_string(),
        })?;
    let library = Arc::new(PinnedLibrary {
        _library: ManuallyDrop::new(library),
    });
    libraries.insert(load_path.to_path_buf(), Arc::clone(&library));
    Ok(library)
}

unsafe fn load_symbol<T: Copy>(
    library: &Library,
    path: &Path,
    symbol: &[u8],
    symbol_name: &'static str,
) -> Result<T, NativePluginError> {
    // SAFETY: The caller verified the native build fingerprint and supplies
    // the exact symbol type emitted by the matching SDK macro.
    let loaded: Symbol<'_, T> =
        unsafe { library.get(symbol) }.map_err(|error| NativePluginError::MissingSymbol {
            path: path.to_path_buf(),
            symbol: symbol_name,
            message: error.to_string(),
        })?;
    Ok(*loaded)
}

fn descriptor_string(
    path: &Path,
    field: &'static str,
    bytes: pi_plugin_sdk::NativeBytes,
) -> Result<String, NativePluginError> {
    // SAFETY: Descriptor pointers are read while the owning library is live
    // and the export contract requires static UTF-8 bytes.
    let bytes = unsafe { bytes.as_slice() };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| NativePluginError::InvalidDescriptor {
            path: path.to_path_buf(),
            field,
            message: error.to_string(),
        })
}

fn validate_plugin_id(path: &Path, id: &str) -> Result<(), NativePluginError> {
    if !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Ok(());
    }
    Err(NativePluginError::InvalidDescriptor {
        path: path.to_path_buf(),
        field: "id",
        message: "expected ASCII letters, digits, '.', '_' or '-'".to_string(),
    })
}

fn verify_manifest(
    expected: &ExpectedDescriptor,
    id: &str,
    version: &str,
    kind: NativePluginKind,
) -> Result<(), NativePluginError> {
    verify_manifest_field(&expected.manifest_path, "id", &expected.id, id)?;
    verify_manifest_field(
        &expected.manifest_path,
        "version",
        &expected.version,
        version,
    )?;
    verify_manifest_field(
        &expected.manifest_path,
        "kind",
        kind_name(expected.kind),
        kind_name(kind),
    )
}

fn verify_manifest_field(
    path: &Path,
    field: &'static str,
    declared: &str,
    actual: &str,
) -> Result<(), NativePluginError> {
    if declared == actual {
        Ok(())
    } else {
        Err(NativePluginError::ManifestMismatch {
            path: path.to_path_buf(),
            field,
            declared: declared.to_string(),
            actual: actual.to_string(),
        })
    }
}

fn parse_kind(kind: &str) -> Option<NativePluginKind> {
    match kind {
        "agent" => Some(NativePluginKind::Agent),
        "provider" => Some(NativePluginKind::Provider),
        "session" => Some(NativePluginKind::Session),
        _ => None,
    }
}

fn kind_name(kind: NativePluginKind) -> &'static str {
    match kind {
        NativePluginKind::Agent => "agent",
        NativePluginKind::Provider => "provider",
        NativePluginKind::Session => "session",
    }
}

fn discover_manifests(root: &Path) -> Result<Vec<PathBuf>, NativePluginError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut manifests = Vec::new();
    discover_manifests_at_depth(root, 0, &mut manifests)?;
    manifests.sort();
    Ok(manifests)
}

fn discover_manifests_at_depth(
    directory: &Path,
    depth: usize,
    manifests: &mut Vec<PathBuf>,
) -> Result<(), NativePluginError> {
    if depth > 2 {
        return Ok(());
    }
    let entries = std::fs::read_dir(directory).map_err(|source| NativePluginError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| NativePluginError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            discover_manifests_at_depth(&path, depth + 1, manifests)?;
        } else if path
            .file_name()
            .is_some_and(|name| name == MANIFEST_FILE_NAME)
        {
            manifests.push(path);
        }
    }
    Ok(())
}

fn canonicalize(path: &Path) -> Result<PathBuf, NativePluginError> {
    std::fs::canonicalize(path).map_err(|source| NativePluginError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), NativePluginError> {
    std::fs::create_dir_all(path).map_err(|source| NativePluginError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_pre_context_agent_hook_abi_before_loading_a_constructor() {
        let path = Path::new("legacy-plugin.dylib");

        assert!(matches!(
            verify_abi_version(path, 1),
            Err(NativePluginError::AbiMismatch {
                expected: 2,
                actual: 1,
                ..
            })
        ));
        assert!(verify_abi_version(path, NATIVE_PLUGIN_ABI_VERSION).is_ok());
    }

    #[test]
    fn missing_discovery_root_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let options = NativePluginLoaderOptions::new(root.path(), root.path().join("agent"));
        assert!(
            NativePluginLoader::new(options)
                .discover()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn manifest_artifact_cannot_escape_package() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(root.path().join("outside.dylib"), b"not a library").unwrap();
        let manifest = package.join(MANIFEST_FILE_NAME);
        std::fs::write(
            &manifest,
            r#"schema = 1
[plugin]
id = "escape"
version = "0.1.0"
kind = "agent"
artifact = "../outside.dylib"
"#,
        )
        .unwrap();

        assert!(matches!(
            resolve_manifest(&manifest),
            Err(NativePluginError::InvalidArtifactPath { .. })
        ));
    }

    #[test]
    fn package_manifest_rejects_runtime_plugin_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("plugin.dylib"), b"fixture").unwrap();
        std::fs::write(
            package.join("pi-plugin.toml"),
            r#"schema = 1
[plugin]
id = "example"
version = "1.0.0"
kind = "agent"
artifact = "plugin.dylib"

[dependencies]
shared = "^1"
"#,
        )
        .unwrap();

        assert!(matches!(
            resolve_manifest(&package.join("pi-plugin.toml")),
            Err(NativePluginError::ManifestParse { .. })
        ));
    }

    #[test]
    fn discovery_ignores_hidden_transaction_directories() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["installed/0000-live", ".installed-backup/0000-old"] {
            let directory = root.path().join(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("pi-plugin.toml"), "fixture").unwrap();
        }

        let manifests = discover_manifests(root.path()).unwrap();

        assert_eq!(
            manifests,
            [root.path().join("installed/0000-live/pi-plugin.toml")]
        );
    }

    #[test]
    fn project_plugins_are_not_touched_before_trust() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("project/.pi/plugins/untrusted");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("plugin.dylib"), b"not a dynamic library").unwrap();
        std::fs::write(
            package.join(MANIFEST_FILE_NAME),
            r#"schema = 1
[plugin]
id = "untrusted"
version = "0.1.0"
kind = "agent"
artifact = "plugin.dylib"
"#,
        )
        .unwrap();
        let agent_dir = root.path().join("agent");
        let options = NativePluginLoaderOptions::new(root.path().join("project"), &agent_dir);
        assert!(
            NativePluginLoader::new(options)
                .discover()
                .unwrap()
                .is_empty()
        );

        let mut options = NativePluginLoaderOptions::new(root.path().join("project"), &agent_dir);
        options.project_trusted = true;
        assert!(matches!(
            NativePluginLoader::new(options).discover(),
            Err(NativePluginError::LibraryLoad { .. })
        ));
    }

    #[test]
    fn artifact_snapshots_are_content_addressed() {
        let root = tempfile::tempdir().unwrap();
        let artifact = root.path().join("plugin.dylib");
        let agent_dir = root.path().join("agent");
        std::fs::write(&artifact, b"first build").unwrap();

        let first = snapshot_artifact(&artifact, &agent_dir).unwrap();
        let repeated = snapshot_artifact(&artifact, &agent_dir).unwrap();
        std::fs::write(&artifact, b"second build").unwrap();
        let second = snapshot_artifact(&artifact, &agent_dir).unwrap();

        assert_eq!(first, repeated);
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first).unwrap(), b"first build");
        assert_eq!(std::fs::read(second).unwrap(), b"second build");
    }
}
