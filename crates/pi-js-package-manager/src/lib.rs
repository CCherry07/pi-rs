//! Pi-compatible discovery and installation orchestration for JavaScript extensions.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as SyncCommand;
use std::time::Duration;

use futures::future::join_all;
use glob::{MatchOptions, Pattern};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use node_semver::{Range as VersionRange, Version};
use path_clean::PathClean;
use percent_encoding::percent_decode_str;
use pi_settings::{
    PackageFilter, PackageSource, SettingsContext, SettingsManager, SettingsScope, SettingsValues,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::process::Command;
use url::Url;

const CONFIG_DIRECTORY: &str = ".pi";
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Everything the package manager needs to resolve one candidate generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveRequest {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
    pub explicit_sources: Vec<String>,
    pub discover_extensions: bool,
}

/// A fully resolved, ordered JavaScript extension load list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub extension_paths: Vec<PathBuf>,
    pub extension_identities: Vec<ResolvedExtensionIdentity>,
    pub skill_paths: Vec<PathBuf>,
    pub prompt_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
struct ResolvedPackageResources {
    skills: Vec<PathBuf>,
    prompts: Vec<PathBuf>,
}

/// User-facing identity for one or more resolved extension entry points.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolvedExtensionIdentity {
    Package(String),
    Path(PathBuf),
}

/// Settings scope used by JavaScript package management commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageScope {
    User,
    Project,
}

/// One package-management request. Keeping this dispatcher in the package
/// manager prevents CLI frontends from reimplementing source and settings policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageOperation {
    Install { source: String, scope: PackageScope },
    Remove { source: String, scope: PackageScope },
    Update { source: Option<String> },
    List,
}

/// A configured package as rendered by `pi list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredPackage {
    pub source: String,
    pub scope: PackageScope,
    pub filtered: bool,
    pub installed_path: Option<PathBuf>,
}

/// Result of a package-management request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageResult {
    Installed {
        source: String,
        scope: PackageScope,
    },
    Removed {
        source: String,
        scope: PackageScope,
        configured: bool,
    },
    Updated {
        sources: Vec<String>,
    },
    Listed {
        packages: Vec<ConfiguredPackage>,
    },
}

#[derive(Debug, Error)]
pub enum PackageManagerError {
    #[error("JavaScript extension does not exist: {0}")]
    MissingExplicitSource(PathBuf),
    #[error("refusing to use path outside package install root: {0}")]
    UnsafeManagedPath(PathBuf),
    #[error("cannot prepare package directory {path}: {source}")]
    PrepareDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot run package command {command}: {source}")]
    SpawnCommand {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("package command failed ({command}): {message}")]
    CommandFailed { command: String, message: String },
    #[error("invalid npmCommand: first array entry must be non-empty")]
    InvalidNpmCommand,
    #[error("project is not trusted; refusing to manage project packages")]
    ProjectNotTrusted,
    #[error("path does not exist: {0}")]
    MissingLocalSource(PathBuf),
    #[error(transparent)]
    Settings(#[from] pi_settings::SettingsError),
    #[error("cannot write settings {path}: {source}")]
    WriteSettings {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("No matching package found for {0}")]
    NoMatchingPackage(String),
    #[error("cannot determine an update target for git package {0}")]
    MissingGitUpdateTarget(PathBuf),
}

/// Deep module that owns Pi's JavaScript extension discovery and package policy.
pub struct PackageManager {
    cwd: PathBuf,
    agent_dir: PathBuf,
    project_trusted: bool,
    explicit_sources: Vec<String>,
    discover_extensions: bool,
    settings_manager: SettingsManager,
    settings_context: SettingsContext,
    user_settings: SettingsValues,
    project_settings: SettingsValues,
}

impl PackageManager {
    pub fn new(request: ResolveRequest) -> Self {
        let settings_manager = SettingsManager::new(&request.agent_dir);
        Self::with_settings(request, settings_manager)
    }

    /// Uses a product-owned settings manager so discovery and management
    /// commands share its trust, recovery, and persistence behavior.
    pub fn with_settings(request: ResolveRequest, settings_manager: SettingsManager) -> Self {
        let cwd = absolute_clean(&request.cwd, None);
        let agent_dir = absolute_clean(&request.agent_dir, Some(&cwd));
        let settings_context = SettingsContext::new(&cwd, request.project_trusted);
        let settings = settings_manager.load(&settings_context);
        Self {
            cwd,
            agent_dir,
            project_trusted: request.project_trusted,
            explicit_sources: request.explicit_sources,
            discover_extensions: request.discover_extensions,
            settings_manager,
            settings_context,
            user_settings: settings.global().clone(),
            project_settings: settings.project().clone(),
        }
    }

    /// Runs a Pi-compatible JavaScript package-management operation.
    pub async fn manage(
        &mut self,
        operation: ManageOperation,
    ) -> Result<ManageResult, PackageManagerError> {
        match operation {
            ManageOperation::Install { source, scope } => {
                self.ensure_management_scope(scope)?;
                self.install_source(&source, scope.into()).await?;
                self.add_source_to_settings(&source, scope)?;
                Ok(ManageResult::Installed { source, scope })
            }
            ManageOperation::Remove { source, scope } => {
                self.ensure_management_scope(scope)?;
                self.remove_source(&source, scope.into()).await?;
                let configured = self.remove_source_from_settings(&source, scope)?;
                Ok(ManageResult::Removed {
                    source,
                    scope,
                    configured,
                })
            }
            ManageOperation::Update { source } => {
                let sources = self.update_sources(source.as_deref()).await?;
                Ok(ManageResult::Updated { sources })
            }
            ManageOperation::List => Ok(ManageResult::Listed {
                packages: self.list_configured_packages(),
            }),
        }
    }

    /// Resolves explicit CLI sources first, then all configured sources in Pi precedence.
    pub async fn resolve(&self) -> Result<Resolution, PackageManagerError> {
        let (explicit, mut explicit_resources) =
            self.resolve_explicit(&self.explicit_sources).await?;
        let (configured, configured_resources) = self.resolve_configured().await?;

        let mut seen_paths = HashSet::new();
        let extensions: Vec<_> = explicit
            .into_iter()
            .chain(configured)
            .filter(|entry| seen_paths.insert(canonical_or_original(&entry.path)))
            .collect();
        let mut seen_identities = HashSet::new();
        let extension_identities = extensions
            .iter()
            .map(ResolvedExtension::identity)
            .filter(|identity| seen_identities.insert(identity.clone()))
            .collect();
        let extension_paths = extensions.into_iter().map(|entry| entry.path).collect();
        explicit_resources
            .skills
            .extend(configured_resources.skills);
        explicit_resources
            .prompts
            .extend(configured_resources.prompts);
        Ok(Resolution {
            extension_paths,
            extension_identities,
            skill_paths: dedupe_paths(explicit_resources.skills),
            prompt_paths: dedupe_paths(explicit_resources.prompts),
        })
    }

    /// Returns whether this request can require a JavaScript runtime.
    ///
    /// This is a side-effect-free startup probe for native-only adapters. It
    /// applies trust, discovery, automatic-entry, package override, and
    /// `--no-extensions` policy without installing or updating packages.
    /// Configured npm/git packages that are not installed are treated
    /// conservatively as requiring the host because their manifests cannot be
    /// inspected until the normal Node-hosted resolution transaction.
    pub fn requires_javascript_host(&self) -> bool {
        if !self.explicit_sources.is_empty() {
            return true;
        }
        if !self.discover_extensions {
            return false;
        }
        self.configured_packages_require_javascript_host()
            || self.configured_local_entries_require_javascript_host()
    }

    fn configured_packages_require_javascript_host(&self) -> bool {
        let packages = self.configured_packages();
        let mut extensions = Vec::new();
        let mut unresolved_package = false;
        for entry in &packages {
            let shadowed_by_project_delta = entry.scope == SourceScope::User
                && packages.iter().any(|candidate| {
                    candidate.scope == SourceScope::Project
                        && candidate.package.is_autoload_delta()
                        && self.package_identity(candidate.package.source(), candidate.scope)
                            == self.package_identity(entry.package.source(), entry.scope)
                });
            if shadowed_by_project_delta || !entry.package.may_enable_extensions() {
                continue;
            }

            let source = entry.package.source();
            let filter = entry.package.filter();
            let delta_base = self.find_delta_base(entry, &packages);
            let (resolved_source, resolved_scope) =
                delta_base.as_ref().map_or((source, entry.scope), |base| {
                    (base.source.as_str(), base.scope)
                });
            let metadata = ExtensionMetadata::package(source, entry.scope);
            match parse_source(resolved_source) {
                ParsedSource::Local(local) => {
                    let path = resolve_path(&local.path, &self.base_directory(resolved_scope));
                    let Ok(metadata_fs) = fs::metadata(&path) else {
                        continue;
                    };
                    let add_directly = metadata_fs.is_file()
                        || (metadata_fs.is_dir()
                            && !collect_package_extensions(
                                &path,
                                &mut extensions,
                                filter,
                                metadata.clone(),
                            ));
                    if add_directly {
                        add_extension(&mut extensions, path, metadata, true);
                    }
                }
                ParsedSource::Npm(npm) => {
                    let Ok(root) = self.npm_install_root(resolved_scope) else {
                        unresolved_package = true;
                        continue;
                    };
                    let installed = root.join("node_modules").join(&npm.name);
                    if installed_npm_matches(&npm, &installed) {
                        collect_package_extensions(&installed, &mut extensions, filter, metadata);
                    } else if !offline() {
                        unresolved_package = true;
                    }
                }
                ParsedSource::Git(git) => match self.git_install_path(&git, resolved_scope) {
                    Ok(installed) if installed.exists() => {
                        collect_package_extensions(&installed, &mut extensions, filter, metadata);
                    }
                    Ok(_) if !offline() => unresolved_package = true,
                    Ok(_) => {}
                    Err(_) => unresolved_package = true,
                },
            }
        }
        unresolved_package
            || sort_and_dedupe(extensions)
                .into_iter()
                .filter(|entry| entry.enabled)
                .flat_map(expand_resolved_extension)
                .next()
                .is_some()
    }

    fn configured_packages(&self) -> Vec<ScopedPackage> {
        let mut packages = Vec::new();
        if self.project_trusted {
            packages.extend(
                self.project_settings
                    .packages
                    .iter()
                    .cloned()
                    .map(|package| ScopedPackage::new(package, SourceScope::Project)),
            );
        }
        packages.extend(
            self.user_settings
                .packages
                .iter()
                .cloned()
                .map(|package| ScopedPackage::new(package, SourceScope::User)),
        );
        self.dedupe_packages(packages)
    }

    fn configured_local_entries_require_javascript_host(&self) -> bool {
        let mut extensions = Vec::new();
        if self.project_trusted {
            self.resolve_local_entries(
                &self.project_settings.extensions,
                &mut extensions,
                SourceScope::Project,
                &self.cwd.join(CONFIG_DIRECTORY),
            );
        }
        self.resolve_local_entries(
            &self.user_settings.extensions,
            &mut extensions,
            SourceScope::User,
            &self.agent_dir,
        );
        if self.project_trusted {
            self.add_automatic_entries(
                &self.cwd.join(CONFIG_DIRECTORY).join("extensions"),
                &self.project_settings.extensions,
                &mut extensions,
                SourceScope::Project,
                &self.cwd.join(CONFIG_DIRECTORY),
            );
        }
        self.add_automatic_entries(
            &self.agent_dir.join("extensions"),
            &self.user_settings.extensions,
            &mut extensions,
            SourceScope::User,
            &self.agent_dir,
        );
        sort_and_dedupe(extensions)
            .into_iter()
            .any(|entry| entry.enabled)
    }

    fn ensure_management_scope(&self, scope: PackageScope) -> Result<(), PackageManagerError> {
        if scope == PackageScope::Project && !self.project_trusted {
            return Err(PackageManagerError::ProjectNotTrusted);
        }
        Ok(())
    }

    async fn install_source(
        &self,
        source: &str,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        match parse_source(source) {
            ParsedSource::Npm(source) => self.install_npm(&source, scope).await,
            ParsedSource::Git(source) => {
                let target = self.git_install_path(&source, scope)?;
                if target.exists() {
                    self.update_git(&source, scope).await
                } else {
                    self.install_git(&source, scope).await
                }
            }
            ParsedSource::Local(source) => {
                let path = resolve_path(&source.path, &self.cwd);
                if path.exists() {
                    Ok(())
                } else {
                    Err(PackageManagerError::MissingLocalSource(path))
                }
            }
        }
    }

    async fn remove_source(
        &self,
        source: &str,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        match parse_source(source) {
            ParsedSource::Npm(source) => self.uninstall_npm(&source, scope).await,
            ParsedSource::Git(source) => self.remove_git(&source, scope),
            ParsedSource::Local(_) => Ok(()),
        }
    }

    fn add_source_to_settings(
        &mut self,
        source: &str,
        scope: PackageScope,
    ) -> Result<bool, PackageManagerError> {
        let resolved_scope = scope.into();
        let normalized = self.normalize_source_for_settings(source, resolved_scope);
        let identity = self.package_identity(source, SourceScope::Temporary);
        let mut packages = self.settings_for_scope(resolved_scope).packages.clone();
        if let Some(index) = packages.iter().position(|existing| {
            self.package_identity(existing.source(), resolved_scope) == identity
        }) {
            if packages[index].source() == normalized {
                return Ok(false);
            }
            packages[index].set_source(normalized);
        } else {
            packages.push(PackageSource::String(normalized));
        }
        self.replace_packages(resolved_scope, packages)?;
        Ok(true)
    }

    fn remove_source_from_settings(
        &mut self,
        source: &str,
        scope: PackageScope,
    ) -> Result<bool, PackageManagerError> {
        let resolved_scope = scope.into();
        let identity = self.package_identity(source, SourceScope::Temporary);
        let packages = self.settings_for_scope(resolved_scope).packages.clone();
        let next: Vec<_> = packages
            .iter()
            .filter(|existing| self.package_identity(existing.source(), resolved_scope) != identity)
            .cloned()
            .collect();
        if next.len() == packages.len() {
            return Ok(false);
        }
        self.replace_packages(resolved_scope, next)?;
        Ok(true)
    }

    fn normalize_source_for_settings(&self, source: &str, scope: SourceScope) -> String {
        let ParsedSource::Local(local) = parse_source(source) else {
            return source.to_string();
        };
        let resolved = resolve_path(&local.path, &self.cwd);
        pathdiff::diff_paths(&resolved, self.base_directory(scope)).map_or_else(
            || posix_path(&resolved),
            |relative| {
                if relative.as_os_str().is_empty() {
                    ".".to_string()
                } else {
                    posix_path(&relative)
                }
            },
        )
    }

    fn list_configured_packages(&self) -> Vec<ConfiguredPackage> {
        let mut packages = Vec::new();
        self.append_configured_packages(&mut packages, SourceScope::User);
        if self.project_trusted {
            self.append_configured_packages(&mut packages, SourceScope::Project);
        }
        packages
    }

    fn append_configured_packages(&self, output: &mut Vec<ConfiguredPackage>, scope: SourceScope) {
        for package in &self.settings_for_scope(scope).packages {
            let source = package.source();
            output.push(ConfiguredPackage {
                source: source.to_string(),
                scope: scope.into(),
                filtered: package.filter().is_some(),
                installed_path: self.installed_path(source, scope),
            });
        }
    }

    fn installed_path(&self, source: &str, scope: SourceScope) -> Option<PathBuf> {
        let path = match parse_source(source) {
            ParsedSource::Npm(source) => self.npm_install_path(&source, scope),
            ParsedSource::Git(source) => self.git_install_path(&source, scope).ok()?,
            ParsedSource::Local(source) => resolve_path(&source.path, &self.base_directory(scope)),
        };
        path.exists().then_some(path)
    }

    async fn update_sources(
        &self,
        requested: Option<&str>,
    ) -> Result<Vec<String>, PackageManagerError> {
        let identity =
            requested.map(|source| self.package_identity(source, SourceScope::Temporary));
        let mut matched = Vec::new();
        for scope in [SourceScope::User, SourceScope::Project] {
            if scope == SourceScope::Project && !self.project_trusted {
                continue;
            }
            for package in &self.settings_for_scope(scope).packages {
                if identity.as_ref().is_some_and(|identity| {
                    self.package_identity(package.source(), scope) != *identity
                }) {
                    continue;
                }
                matched.push((package.source().to_string(), scope));
            }
        }
        if let Some(source) = requested
            && matched.is_empty()
        {
            return Err(PackageManagerError::NoMatchingPackage(
                self.no_matching_package_source(source),
            ));
        }
        if offline() {
            return Ok(matched.into_iter().map(|(source, _)| source).collect());
        }

        let mut npm = Vec::new();
        let mut git = Vec::new();
        for (source, scope) in &matched {
            match parse_source(source) {
                ParsedSource::Npm(parsed) if !parsed.pinned => npm.push((parsed, *scope)),
                ParsedSource::Git(parsed) => git.push((parsed, *scope)),
                ParsedSource::Npm(_) | ParsedSource::Local(_) => {}
            }
        }
        let checks = join_all(npm.into_iter().map(|(source, scope)| async move {
            let should_update = self.should_update_npm(&source, scope).await;
            (source, scope, should_update)
        }))
        .await;
        let mut user_npm = Vec::new();
        let mut project_npm = Vec::new();
        for (source, scope, should_update) in checks {
            if !should_update {
                continue;
            }
            if scope == SourceScope::User {
                user_npm.push(source);
            } else {
                project_npm.push(source);
            }
        }
        let npm_updates = async {
            let user = async {
                if user_npm.is_empty() {
                    Ok(())
                } else {
                    self.update_npm_batch(&user_npm, SourceScope::User).await
                }
            };
            let project = async {
                if project_npm.is_empty() {
                    Ok(())
                } else {
                    self.update_npm_batch(&project_npm, SourceScope::Project)
                        .await
                }
            };
            let (user, project) = futures::join!(user, project);
            user?;
            project
        };
        let git_updates = async {
            let results = join_all(
                git.iter()
                    .map(|(source, scope)| self.update_git(source, *scope)),
            )
            .await;
            for result in results {
                result?;
            }
            Ok::<(), PackageManagerError>(())
        };
        let (npm_result, git_result) = futures::join!(npm_updates, git_updates);
        npm_result?;
        git_result?;
        Ok(matched.into_iter().map(|(source, _)| source).collect())
    }

    fn no_matching_package_source(&self, requested: &str) -> String {
        let requested = requested.trim();
        for scope in [SourceScope::User, SourceScope::Project] {
            if scope == SourceScope::Project && !self.project_trusted {
                continue;
            }
            for package in &self.settings_for_scope(scope).packages {
                let configured = package.source();
                let suggested = match parse_source(configured) {
                    ParsedSource::Npm(source) => {
                        requested == source.name || requested == source.spec
                    }
                    ParsedSource::Git(source) => {
                        let shorthand = format!("{}/{}", source.host, source.path);
                        requested == shorthand
                            || source.reference.as_ref().is_some_and(|reference| {
                                requested == format!("{shorthand}@{reference}")
                            })
                    }
                    ParsedSource::Local(_) => false,
                };
                if suggested {
                    return format!("{requested}. Did you mean {configured}?");
                }
            }
        }
        requested.to_string()
    }

    async fn should_update_npm(&self, source: &NpmSource, scope: SourceScope) -> bool {
        let Ok(root) = self.npm_install_root(scope) else {
            return true;
        };
        let installed = root.join("node_modules").join(&source.name);
        let Some(installed_version) = installed_npm_version(&installed) else {
            return true;
        };
        let lookup = if source.version.is_some() {
            &source.spec
        } else {
            &source.name
        };
        let arguments = vec![
            "view".to_string(),
            lookup.to_string(),
            "version".to_string(),
            "--json".to_string(),
        ];
        let Ok(Ok(output)) = tokio::time::timeout(
            Duration::from_secs(10),
            self.run_npm_capture(&arguments, Some(&self.cwd)),
        )
        .await
        else {
            return true;
        };
        latest_npm_version(&output).is_none_or(|target| target > installed_version)
    }

    async fn update_npm_batch(
        &self,
        sources: &[NpmSource],
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let specs: Vec<_> = sources
            .iter()
            .map(|source| {
                source.version.as_ref().map_or_else(
                    || format!("{}@latest", source.name),
                    |_| source.spec.clone(),
                )
            })
            .collect();
        self.install_npm_specs(&specs, scope).await
    }

    fn settings_for_scope(&self, scope: SourceScope) -> &SettingsValues {
        match scope {
            SourceScope::User | SourceScope::Temporary => &self.user_settings,
            SourceScope::Project => &self.project_settings,
        }
    }

    fn replace_packages(
        &mut self,
        scope: SourceScope,
        packages: Vec<PackageSource>,
    ) -> Result<(), PackageManagerError> {
        if scope == SourceScope::Project && !self.project_trusted {
            return Err(PackageManagerError::ProjectNotTrusted);
        }
        let settings_scope = match scope {
            SourceScope::User | SourceScope::Temporary => SettingsScope::Global,
            SourceScope::Project => SettingsScope::Project,
        };
        let settings = self.settings_manager.replace_packages(
            &self.settings_context,
            settings_scope,
            &packages,
        )?;
        self.user_settings = settings.global().clone();
        self.project_settings = settings.project().clone();
        Ok(())
    }

    async fn resolve_configured(
        &self,
    ) -> Result<(Vec<ResolvedExtension>, ResolvedPackageResources), PackageManagerError> {
        let mut extensions = Vec::new();
        let mut resources = ResolvedPackageResources::default();
        let packages = self.configured_packages();
        self.resolve_package_sources(&packages, &mut extensions, &mut resources, false)
            .await?;

        if self.discover_extensions && self.project_trusted {
            self.resolve_local_entries(
                &self.project_settings.extensions,
                &mut extensions,
                SourceScope::Project,
                &self.cwd.join(CONFIG_DIRECTORY),
            );
        }
        if self.discover_extensions {
            self.resolve_local_entries(
                &self.user_settings.extensions,
                &mut extensions,
                SourceScope::User,
                &self.agent_dir,
            );
        }
        if self.discover_extensions && self.project_trusted {
            self.add_automatic_entries(
                &self.cwd.join(CONFIG_DIRECTORY).join("extensions"),
                &self.project_settings.extensions,
                &mut extensions,
                SourceScope::Project,
                &self.cwd.join(CONFIG_DIRECTORY),
            );
        }
        if self.discover_extensions {
            self.add_automatic_entries(
                &self.agent_dir.join("extensions"),
                &self.user_settings.extensions,
                &mut extensions,
                SourceScope::User,
                &self.agent_dir,
            );
        }

        let extensions = if self.discover_extensions {
            sort_and_dedupe(extensions)
                .into_iter()
                .filter(|entry| entry.enabled)
                .flat_map(expand_resolved_extension)
                .collect()
        } else {
            Vec::new()
        };
        Ok((extensions, resources))
    }

    async fn resolve_explicit(
        &self,
        sources: &[String],
    ) -> Result<(Vec<ResolvedExtension>, ResolvedPackageResources), PackageManagerError> {
        let packages: Vec<_> = sources
            .iter()
            .cloned()
            .map(PackageSource::String)
            .map(|package| ScopedPackage::new(package, SourceScope::Temporary))
            .collect();
        let mut extensions = Vec::new();
        let mut resources = ResolvedPackageResources::default();
        self.resolve_package_sources(&packages, &mut extensions, &mut resources, true)
            .await?;
        Ok((
            sort_and_dedupe(extensions)
                .into_iter()
                .filter(|entry| entry.enabled)
                .flat_map(expand_resolved_extension)
                .collect(),
            resources,
        ))
    }

    fn resolve_local_entries(
        &self,
        entries: &[String],
        extensions: &mut Vec<ResolvedExtension>,
        scope: SourceScope,
        base_directory: &Path,
    ) {
        let plain: Vec<_> = entries.iter().filter(|entry| !is_pattern(entry)).collect();
        let patterns: Vec<_> = entries
            .iter()
            .filter(|entry| is_pattern(entry))
            .cloned()
            .collect();
        let paths: Vec<_> = plain
            .into_iter()
            .map(|entry| resolve_path(entry, base_directory))
            .collect();
        let files = collect_extension_files(&paths);
        let enabled = apply_patterns(&files, &patterns, base_directory);
        let metadata = ExtensionMetadata::top_level("local", scope);
        for path in files {
            add_extension(
                extensions,
                path.clone(),
                metadata.clone(),
                enabled.contains(&path),
            );
        }
    }

    fn add_automatic_entries(
        &self,
        directory: &Path,
        overrides: &[String],
        extensions: &mut Vec<ResolvedExtension>,
        scope: SourceScope,
        base_directory: &Path,
    ) {
        let metadata = ExtensionMetadata::top_level("auto", scope);
        for path in discover_extensions(directory) {
            let enabled = is_enabled_by_overrides(&path, overrides, base_directory);
            add_extension(extensions, path, metadata.clone(), enabled);
        }
    }

    fn dedupe_packages(&self, packages: Vec<ScopedPackage>) -> Vec<ScopedPackage> {
        let mut result: Vec<ScopedPackage> = Vec::new();
        let mut identities: Vec<String> = Vec::new();
        for entry in packages {
            let identity = self.package_identity(entry.package.source(), entry.scope);
            if let Some(index) = identities.iter().position(|seen| seen == &identity) {
                let existing = &result[index];
                if existing.scope == SourceScope::Project
                    && entry.scope == SourceScope::User
                    && existing.package.is_autoload_delta()
                {
                    result.push(entry);
                } else if entry.scope == SourceScope::Project {
                    result[index] = entry;
                }
            } else {
                identities.push(identity);
                result.push(entry);
            }
        }
        result
    }

    async fn resolve_package_sources(
        &self,
        sources: &[ScopedPackage],
        extensions: &mut Vec<ResolvedExtension>,
        resources: &mut ResolvedPackageResources,
        explicit: bool,
    ) -> Result<(), PackageManagerError> {
        for entry in sources {
            let source = entry.package.source();
            let filter = entry.package.filter();
            let delta_base = self.find_delta_base(entry, sources);
            let (resolved_source, resolved_scope) =
                delta_base.as_ref().map_or((source, entry.scope), |base| {
                    (base.source.as_str(), base.scope)
                });
            let parsed = parse_source(resolved_source);
            let metadata = ExtensionMetadata::package(source, entry.scope);

            match parsed {
                ParsedSource::Local(local) => {
                    let path = resolve_path(&local.path, &self.base_directory(resolved_scope));
                    if !path.exists() {
                        if explicit {
                            return Err(PackageManagerError::MissingExplicitSource(path));
                        }
                        continue;
                    }
                    let Ok(metadata_fs) = fs::metadata(&path) else {
                        continue;
                    };
                    let add_directly = metadata_fs.is_file()
                        || (metadata_fs.is_dir()
                            && !collect_package_contents(
                                &path,
                                extensions,
                                resources,
                                filter,
                                metadata.clone(),
                            ));
                    if add_directly {
                        add_extension(extensions, path, metadata, true);
                    }
                }
                ParsedSource::Npm(npm) => {
                    let mut installed = self.npm_install_path(&npm, resolved_scope);
                    if !installed_npm_matches(&npm, &installed) {
                        if offline() {
                            continue;
                        }
                        self.install_npm(&npm, resolved_scope).await?;
                        installed = self.npm_install_path(&npm, resolved_scope);
                    }
                    collect_package_contents(&installed, extensions, resources, filter, metadata);
                }
                ParsedSource::Git(git) => {
                    let installed = self.git_install_path(&git, resolved_scope)?;
                    if !installed.exists() {
                        if offline() {
                            continue;
                        }
                        self.install_git(&git, resolved_scope).await?;
                    } else if resolved_scope == SourceScope::Temporary && !git.pinned && !offline()
                    {
                        self.refresh_temporary_git(&installed).await;
                    }
                    collect_package_contents(&installed, extensions, resources, filter, metadata);
                }
            }
        }
        Ok(())
    }

    fn find_delta_base(
        &self,
        entry: &ScopedPackage,
        sources: &[ScopedPackage],
    ) -> Option<DeltaBase> {
        if entry.scope != SourceScope::Project || !entry.package.is_autoload_delta() {
            return None;
        }
        let identity = self.package_identity(entry.package.source(), SourceScope::Project);
        sources
            .iter()
            .find(|candidate| {
                candidate.scope == SourceScope::User
                    && self.package_identity(candidate.package.source(), SourceScope::User)
                        == identity
            })
            .map(|candidate| DeltaBase {
                source: candidate.package.source().to_string(),
                scope: SourceScope::User,
            })
    }

    fn package_identity(&self, source: &str, scope: SourceScope) -> String {
        match parse_source(source) {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => format!(
                "local:{}",
                resolve_path(&local.path, &self.base_directory(scope)).display()
            ),
        }
    }

    fn base_directory(&self, scope: SourceScope) -> PathBuf {
        match scope {
            SourceScope::Project => self.cwd.join(CONFIG_DIRECTORY),
            SourceScope::User => self.agent_dir.clone(),
            SourceScope::Temporary => self.cwd.clone(),
        }
    }

    fn npm_command(&self) -> Result<(&str, &[String]), PackageManagerError> {
        let configured = self
            .project_settings
            .npm_command
            .as_ref()
            .or(self.user_settings.npm_command.as_ref());
        match configured {
            Some(command) => command
                .split_first()
                .filter(|(program, _)| !program.is_empty())
                .map(|(program, arguments)| (program.as_str(), arguments))
                .ok_or(PackageManagerError::InvalidNpmCommand),
            None => Ok(("npm", &[])),
        }
    }

    fn package_manager_name(&self) -> Result<String, PackageManagerError> {
        let (program, arguments) = self.npm_command()?;
        let executable = arguments
            .iter()
            .rposition(|argument| argument == "--")
            .and_then(|index| arguments.get(index + 1))
            .map_or(program, String::as_str);
        Ok(Path::new(executable)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .trim_end_matches(".cmd")
            .trim_end_matches(".exe")
            .to_ascii_lowercase())
    }

    fn npm_install_root(&self, scope: SourceScope) -> Result<PathBuf, PackageManagerError> {
        match scope {
            SourceScope::Temporary => self.temporary_directory("npm", ""),
            SourceScope::Project => Ok(self.cwd.join(CONFIG_DIRECTORY).join("npm")),
            SourceScope::User => Ok(self.agent_dir.join("npm")),
        }
    }

    fn npm_install_path(&self, source: &NpmSource, scope: SourceScope) -> PathBuf {
        let root = match self.npm_install_root(scope) {
            Ok(root) => root,
            Err(_) => return self.agent_dir.join("npm/node_modules").join(&source.name),
        };
        let managed = root.join("node_modules").join(&source.name);
        if scope != SourceScope::User || managed.exists() {
            return managed;
        }
        self.legacy_global_npm_path(&source.name)
            .filter(|path| path.exists())
            .unwrap_or(managed)
    }

    fn legacy_global_npm_path(&self, package_name: &str) -> Option<PathBuf> {
        let manager = self.package_manager_name().ok()?;
        if manager == "pnpm" {
            let output = self.run_npm_capture_sync(&["list", "-g", "--depth", "0", "--json"])?;
            let entries: serde_json::Value = serde_json::from_str(&output).ok()?;
            if let Some(path) = entries.as_array().and_then(|entries| {
                entries.iter().find_map(|entry| {
                    entry
                        .get("dependencies")?
                        .get(package_name)?
                        .get("path")?
                        .as_str()
                })
            }) {
                return Some(PathBuf::from(path));
            }
        }
        if manager == "bun" {
            let bin = self.run_npm_capture_sync(&["pm", "bin", "-g"])?;
            return PathBuf::from(bin.trim()).parent().map(|parent| {
                parent
                    .join("install/global/node_modules")
                    .join(package_name)
            });
        }
        let root = self.run_npm_capture_sync(&["root", "-g"])?;
        Some(PathBuf::from(root.trim()).join(package_name))
    }

    fn run_npm_capture_sync(&self, arguments: &[&str]) -> Option<String> {
        let (program, prefix) = self.npm_command().ok()?;
        let output = SyncCommand::new(program)
            .args(prefix)
            .args(arguments)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn install_npm(
        &self,
        source: &NpmSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        self.install_npm_specs(std::slice::from_ref(&source.spec), scope)
            .await
    }

    async fn install_npm_specs(
        &self,
        specs: &[String],
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let root = self.npm_install_root(scope)?;
        ensure_package_directory(&root)?;
        let manager = self.package_manager_name()?;
        let mut arguments = vec!["install".to_string()];
        arguments.extend_from_slice(specs);
        if manager == "bun" {
            arguments.extend([
                "--cwd".to_string(),
                root.display().to_string(),
                "--omit=peer".to_string(),
            ]);
        } else if manager == "pnpm" {
            arguments.extend([
                "--prefix".to_string(),
                root.display().to_string(),
                "--config.auto-install-peers=false".to_string(),
                "--config.strict-peer-dependencies=false".to_string(),
                "--config.strict-dep-builds=false".to_string(),
            ]);
        } else {
            arguments.extend([
                "--prefix".to_string(),
                root.display().to_string(),
                "--legacy-peer-deps".to_string(),
            ]);
        }
        self.run_npm(&arguments, None).await
    }

    async fn uninstall_npm(
        &self,
        source: &NpmSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let root = self.npm_install_root(scope)?;
        if !root.exists() {
            return Ok(());
        }
        let manager = self.package_manager_name()?;
        let mut arguments = vec!["uninstall".to_string(), source.name.clone()];
        if manager == "bun" {
            arguments.extend(["--cwd".to_string(), root.display().to_string()]);
        } else {
            arguments.extend(["--prefix".to_string(), root.display().to_string()]);
            if manager != "pnpm" {
                arguments.push("--legacy-peer-deps".to_string());
            }
        }
        self.run_npm(&arguments, None).await
    }

    fn git_install_path(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<PathBuf, PackageManagerError> {
        if scope == SourceScope::Temporary {
            return self.temporary_directory(&format!("git-{}", source.host), &source.path);
        }
        let root = if scope == SourceScope::Project {
            self.cwd.join(CONFIG_DIRECTORY).join("git")
        } else {
            self.agent_dir.join("git")
        };
        managed_path(&root, &[&source.host, &source.path])
    }

    fn temporary_directory(
        &self,
        prefix: &str,
        suffix: &str,
    ) -> Result<PathBuf, PackageManagerError> {
        let base = self.agent_dir.join("tmp/extensions").join(prefix);
        let hash = hex::encode(Sha256::digest(format!("{prefix}-{suffix}").as_bytes()));
        managed_path(&base, &[&hash[..8], suffix])
    }

    async fn install_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target = self.git_install_path(source, scope)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| PackageManagerError::PrepareDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let clone = self
            .run_command(
                "git",
                &["clone", &source.repo, &target.display().to_string()],
                None,
                true,
            )
            .await;
        if let Err(error) = clone {
            let _ = fs::remove_dir_all(&target);
            return Err(error);
        }
        let result = async {
            if let Some(reference) = &source.reference {
                self.run_command("git", &["checkout", reference], Some(&target), true)
                    .await?;
            }
            self.install_git_dependencies(&target).await
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_dir_all(&target);
        }
        result
    }

    async fn refresh_temporary_git(&self, target: &Path) {
        if self
            .run_command("git", &["pull", "--ff-only"], Some(target), true)
            .await
            .is_ok()
        {
            let _ = self.install_git_dependencies(target).await;
        }
    }

    async fn update_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target = self.git_install_path(source, scope)?;
        if !target.exists() {
            return self.install_git(source, scope).await;
        }
        if let Some(reference) = &source.reference {
            self.ensure_git_ref(&target, &["fetch", "origin", reference], "FETCH_HEAD")
                .await?;
            return Ok(());
        }
        let upstream = self
            .run_command_capture(
                "git",
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
                Some(&target),
                true,
            )
            .await
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.run_command_capture_sync(
                    "git",
                    &["symbolic-ref", "refs/remotes/origin/HEAD"],
                    Some(&target),
                )
                .map(|value| value.trim().trim_start_matches("refs/remotes/").to_string())
            })
            .ok_or_else(|| PackageManagerError::MissingGitUpdateTarget(target.clone()))?;
        let branch = upstream.strip_prefix("origin/").unwrap_or(&upstream);
        let fetch_spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        self.ensure_git_ref(
            &target,
            &["fetch", "--prune", "--no-tags", "origin", &fetch_spec],
            &format!("origin/{branch}"),
        )
        .await
    }

    async fn ensure_git_ref(
        &self,
        target: &Path,
        fetch_arguments: &[&str],
        reference: &str,
    ) -> Result<(), PackageManagerError> {
        self.run_command("git", fetch_arguments, Some(target), true)
            .await?;
        let local = self
            .run_command_capture("git", &["rev-parse", "HEAD"], Some(target), true)
            .await?;
        let commit_reference = format!("{reference}^{{commit}}");
        let remote = self
            .run_command_capture("git", &["rev-parse", &commit_reference], Some(target), true)
            .await?;
        let marker = git_update_marker(target);
        if local.trim() == remote.trim() {
            if marker.exists() {
                self.clean_and_install_git_dependencies(target, &marker)
                    .await?;
            } else if git_dependencies_missing(target) {
                self.install_git_dependencies(target).await?;
            }
            return Ok(());
        }
        fs::write(&marker, "").map_err(|source| PackageManagerError::WriteSettings {
            path: marker.clone(),
            source,
        })?;
        self.run_command(
            "git",
            &["reset", "--hard", &commit_reference],
            Some(target),
            true,
        )
        .await?;
        self.clean_and_install_git_dependencies(target, &marker)
            .await
    }

    async fn clean_and_install_git_dependencies(
        &self,
        target: &Path,
        marker: &Path,
    ) -> Result<(), PackageManagerError> {
        self.run_command("git", &["clean", "-fdx"], Some(target), true)
            .await?;
        self.install_git_dependencies(target).await?;
        fs::remove_file(marker).map_err(|source| PackageManagerError::WriteSettings {
            path: marker.to_path_buf(),
            source,
        })
    }

    fn remove_git(
        &self,
        source: &GitSource,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        let target = self.git_install_path(source, scope)?;
        if target.exists() {
            fs::remove_dir_all(&target).map_err(|source| PackageManagerError::WriteSettings {
                path: target.clone(),
                source,
            })?;
        }
        let marker = git_update_marker(&target);
        if marker.exists() {
            fs::remove_file(&marker).map_err(|source| PackageManagerError::WriteSettings {
                path: marker,
                source,
            })?;
        }
        let root = match scope {
            SourceScope::Project => self.cwd.join(CONFIG_DIRECTORY).join("git"),
            SourceScope::User => self.agent_dir.join("git"),
            SourceScope::Temporary => return Ok(()),
        };
        prune_empty_parents(&target, &root);
        Ok(())
    }

    async fn install_git_dependencies(&self, target: &Path) -> Result<(), PackageManagerError> {
        if !target.join("package.json").exists() {
            return Ok(());
        }
        let configured =
            self.project_settings.npm_command.is_some() || self.user_settings.npm_command.is_some();
        let arguments = if configured {
            vec!["install".to_string()]
        } else {
            vec!["install".to_string(), "--omit=dev".to_string()]
        };
        self.run_npm(&arguments, Some(target)).await
    }

    async fn run_npm(
        &self,
        arguments: &[String],
        cwd: Option<&Path>,
    ) -> Result<(), PackageManagerError> {
        let (program, prefix) = self.npm_command()?;
        let mut all_arguments = prefix.to_vec();
        all_arguments.extend_from_slice(arguments);
        self.run_command_strings(program, &all_arguments, cwd, false)
            .await
    }

    async fn run_npm_capture(
        &self,
        arguments: &[String],
        cwd: Option<&Path>,
    ) -> Result<String, PackageManagerError> {
        let (program, prefix) = self.npm_command()?;
        let mut all_arguments = prefix.to_vec();
        all_arguments.extend_from_slice(arguments);
        self.run_command_capture_strings(program, &all_arguments, cwd, false)
            .await
    }

    async fn run_command(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
        disable_git_prompt: bool,
    ) -> Result<(), PackageManagerError> {
        let arguments: Vec<_> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        self.run_command_strings_with_env(program, &arguments, cwd, disable_git_prompt)
            .await
    }

    async fn run_command_strings(
        &self,
        program: &str,
        arguments: &[String],
        cwd: Option<&Path>,
        disable_git_prompt: bool,
    ) -> Result<(), PackageManagerError> {
        self.run_command_strings_with_env(program, arguments, cwd, disable_git_prompt)
            .await
    }

    async fn run_command_strings_with_env(
        &self,
        program: &str,
        arguments: &[String],
        cwd: Option<&Path>,
        disable_git_prompt: bool,
    ) -> Result<(), PackageManagerError> {
        let mut command = Command::new(program);
        command.args(arguments);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        if disable_git_prompt {
            command.env("GIT_TERMINAL_PROMPT", "0");
        }
        let display = format_command(program, arguments);
        let output =
            command
                .output()
                .await
                .map_err(|source| PackageManagerError::SpawnCommand {
                    command: display.clone(),
                    source,
                })?;
        if output.status.success() {
            return Ok(());
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(PackageManagerError::CommandFailed {
            command: display,
            message: if message.is_empty() {
                output.status.to_string()
            } else {
                message
            },
        })
    }

    async fn run_command_capture(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
        disable_git_prompt: bool,
    ) -> Result<String, PackageManagerError> {
        let arguments: Vec<_> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        self.run_command_capture_strings(program, &arguments, cwd, disable_git_prompt)
            .await
    }

    async fn run_command_capture_strings(
        &self,
        program: &str,
        arguments: &[String],
        cwd: Option<&Path>,
        disable_git_prompt: bool,
    ) -> Result<String, PackageManagerError> {
        let mut command = Command::new(program);
        command.args(arguments);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        if disable_git_prompt {
            command.env("GIT_TERMINAL_PROMPT", "0");
        }
        let display = format_command(program, arguments);
        let output =
            command
                .output()
                .await
                .map_err(|source| PackageManagerError::SpawnCommand {
                    command: display.clone(),
                    source,
                })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(PackageManagerError::CommandFailed {
            command: display,
            message: if message.is_empty() {
                output.status.to_string()
            } else {
                message
            },
        })
    }

    fn run_command_capture_sync(
        &self,
        program: &str,
        arguments: &[&str],
        cwd: Option<&Path>,
    ) -> Option<String> {
        let mut command = SyncCommand::new(program);
        command.args(arguments);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.env("GIT_TERMINAL_PROMPT", "0");
        let output = command.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[derive(Debug, Clone)]
struct PiManifest {
    extensions: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    prompts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceScope {
    Project,
    User,
    Temporary,
}

impl From<PackageScope> for SourceScope {
    fn from(scope: PackageScope) -> Self {
        match scope {
            PackageScope::User => Self::User,
            PackageScope::Project => Self::Project,
        }
    }
}

impl From<SourceScope> for PackageScope {
    fn from(scope: SourceScope) -> Self {
        match scope {
            SourceScope::Project => Self::Project,
            SourceScope::User | SourceScope::Temporary => Self::User,
        }
    }
}

#[derive(Debug, Clone)]
struct ScopedPackage {
    package: PackageSource,
    scope: SourceScope,
}

impl ScopedPackage {
    fn new(package: PackageSource, scope: SourceScope) -> Self {
        Self { package, scope }
    }
}

struct DeltaBase {
    source: String,
    scope: SourceScope,
}

#[derive(Debug, Clone)]
struct ExtensionMetadata {
    source: String,
    scope: SourceScope,
    origin: ExtensionOrigin,
}

impl ExtensionMetadata {
    fn top_level(source: &str, scope: SourceScope) -> Self {
        Self {
            source: source.to_string(),
            scope,
            origin: ExtensionOrigin::TopLevel,
        }
    }

    fn package(source: &str, scope: SourceScope) -> Self {
        Self {
            source: source.to_string(),
            scope,
            origin: ExtensionOrigin::Package,
        }
    }

    fn rank(&self) -> u8 {
        if self.origin == ExtensionOrigin::Package {
            return 4;
        }
        let scope = if self.scope == SourceScope::Project {
            0
        } else {
            2
        };
        scope + u8::from(self.source != "local")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionOrigin {
    Package,
    TopLevel,
}

#[derive(Debug, Clone)]
struct ResolvedExtension {
    path: PathBuf,
    enabled: bool,
    metadata: ExtensionMetadata,
}

impl ResolvedExtension {
    fn identity(&self) -> ResolvedExtensionIdentity {
        if self.metadata.origin == ExtensionOrigin::Package
            && (self.metadata.scope != SourceScope::Temporary
                || !matches!(parse_source(&self.metadata.source), ParsedSource::Local(_)))
        {
            ResolvedExtensionIdentity::Package(self.metadata.source.clone())
        } else {
            ResolvedExtensionIdentity::Path(self.path.clone())
        }
    }
}

#[derive(Debug, Clone)]
struct NpmSource {
    spec: String,
    name: String,
    version: Option<String>,
    range: Option<VersionRange>,
    pinned: bool,
}

#[derive(Debug, Clone)]
struct LocalSource {
    path: String,
}

#[derive(Debug, Clone)]
struct GitSource {
    repo: String,
    host: String,
    path: String,
    reference: Option<String>,
    pinned: bool,
}

enum ParsedSource {
    Npm(NpmSource),
    Local(LocalSource),
    Git(GitSource),
}

fn installed_npm_version(installed: &Path) -> Option<Version> {
    let contents = fs::read(installed.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&contents).ok()?;
    Version::parse(value.get("version")?.as_str()?).ok()
}

fn latest_npm_version(output: &str) -> Option<Version> {
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    match value {
        serde_json::Value::String(version) => Version::parse(&version).ok(),
        serde_json::Value::Array(versions) => versions
            .into_iter()
            .filter_map(|version| {
                version
                    .as_str()
                    .and_then(|value| Version::parse(value).ok())
            })
            .max(),
        _ => None,
    }
}

fn git_update_marker(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("package");
    target.with_file_name(format!(".{name}.pi-update-incomplete"))
}

fn git_dependencies_missing(target: &Path) -> bool {
    let Ok(contents) = fs::read(target.join("package.json")) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&contents) else {
        return false;
    };
    let Some(dependencies) = manifest
        .get("dependencies")
        .and_then(|value| value.as_object())
    else {
        return false;
    };
    dependencies
        .keys()
        .any(|name| !target.join("node_modules").join(name).exists())
}

fn prune_empty_parents(target: &Path, root: &Path) {
    let root = absolute_clean(root, None);
    let mut current = target.parent().map(Path::to_path_buf);
    while let Some(directory) = current {
        if directory == root || !directory.starts_with(&root) {
            break;
        }
        let empty = fs::read_dir(&directory)
            .ok()
            .is_some_and(|mut entries| entries.next().is_none());
        if !empty || fs::remove_dir(&directory).is_err() {
            break;
        }
        current = directory.parent().map(Path::to_path_buf);
    }
}

fn read_pi_manifest(directory: &Path) -> Option<PiManifest> {
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("package.json")).ok()?).ok()?;
    let pi = value.get("pi")?.as_object()?;
    let string_list = |key: &str| {
        let values = pi.get(key)?.as_array()?;
        values
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
    };
    Some(PiManifest {
        extensions: string_list("extensions"),
        skills: string_list("skills"),
        prompts: string_list("prompts"),
    })
}

fn discover_extensions(directory: &Path) -> Vec<PathBuf> {
    if !directory.exists() {
        return Vec::new();
    }
    if let Some(entries) = resolve_extension_entries(directory) {
        return entries;
    }
    let ignore = build_ignore_matcher(directory);
    let mut entries: Vec<_> = match fs::read_dir(directory) {
        Ok(entries) => entries.filter_map(Result::ok).collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_key(|entry| entry.file_name());
    let mut discovered = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if ignore.as_ref().is_some_and(|ignore| {
            ignore
                .matched_path_or_any_parents(&path, metadata.is_dir())
                .is_ignore()
        }) {
            continue;
        }
        if metadata.is_file() && (name.ends_with(".ts") || name.ends_with(".js")) {
            discovered.push(path);
        } else if metadata.is_dir() {
            discovered.extend(resolve_extension_entries(&path).unwrap_or_default());
        }
    }
    discovered
}

fn build_ignore_matcher(root: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    for filename in IGNORE_FILE_NAMES {
        let path = root.join(filename);
        if path.exists() {
            let _ = builder.add(path);
        }
    }
    builder.build().ok()
}

fn resolve_extension_entries(directory: &Path) -> Option<Vec<PathBuf>> {
    if let Some(entries) = read_pi_manifest(directory).and_then(|manifest| manifest.extensions)
        && !entries.is_empty()
    {
        let resolved: Vec<_> = entries
            .into_iter()
            .map(|entry| directory.join(entry).clean())
            .filter(|entry| entry.exists())
            .collect();
        if !resolved.is_empty() {
            return Some(resolved);
        }
    }
    ["index.ts", "index.js"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|entry| entry.exists())
        .map(|entry| vec![entry])
}

fn collect_package_extensions(
    package_root: &Path,
    extensions: &mut Vec<ResolvedExtension>,
    filter: Option<&PackageFilter>,
    metadata: ExtensionMetadata,
) -> bool {
    if let Some(filter) = filter {
        let files = collect_manifest_files(package_root);
        if filter.autoload == Some(false) {
            for (path, enabled) in apply_autoload_disabled_patterns(
                &files,
                filter.extensions.as_deref().unwrap_or_default(),
                package_root,
            ) {
                add_extension(extensions, path, metadata.clone(), enabled);
            }
        } else if let Some(patterns) = &filter.extensions {
            let enabled = if patterns.is_empty() {
                HashSet::new()
            } else {
                apply_patterns(&files, patterns, package_root)
            };
            for path in files {
                let is_enabled = enabled.contains(&path);
                add_extension(extensions, path, metadata.clone(), is_enabled);
            }
        } else {
            for path in files {
                add_extension(extensions, path, metadata.clone(), true);
            }
        }
        return true;
    }

    if let Some(manifest) = read_pi_manifest(package_root) {
        if let Some(entries) = manifest.extensions {
            for path in manifest_extension_files(package_root, &entries) {
                add_extension(extensions, path, metadata.clone(), true);
            }
        }
        return true;
    }
    let convention = package_root.join("extensions");
    if !convention.exists() {
        return false;
    }
    for path in discover_extensions(&convention) {
        add_extension(extensions, path, metadata.clone(), true);
    }
    true
}

fn collect_package_contents(
    package_root: &Path,
    extensions: &mut Vec<ResolvedExtension>,
    resources: &mut ResolvedPackageResources,
    filter: Option<&PackageFilter>,
    metadata: ExtensionMetadata,
) -> bool {
    let recognized = collect_package_extensions(package_root, extensions, filter, metadata);
    let skills = collect_package_resource_paths(package_root, ResourceKind::Skill, filter);
    let prompts = collect_package_resource_paths(package_root, ResourceKind::Prompt, filter);
    let has_resources = !skills.is_empty() || !prompts.is_empty();
    resources.skills.extend(skills);
    resources.prompts.extend(prompts);
    recognized || has_resources || read_pi_manifest(package_root).is_some()
}

#[derive(Debug, Clone, Copy)]
enum ResourceKind {
    Skill,
    Prompt,
}

impl ResourceKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Prompt => "prompts",
        }
    }

    fn manifest_entries(self, manifest: &PiManifest) -> Option<&[String]> {
        match self {
            Self::Skill => manifest.skills.as_deref(),
            Self::Prompt => manifest.prompts.as_deref(),
        }
    }

    fn filter_patterns(self, filter: &PackageFilter) -> Option<&[String]> {
        match self {
            Self::Skill => filter.skills.as_deref(),
            Self::Prompt => filter.prompts.as_deref(),
        }
    }
}

fn collect_package_resource_paths(
    package_root: &Path,
    kind: ResourceKind,
    filter: Option<&PackageFilter>,
) -> Vec<PathBuf> {
    let manifest = read_pi_manifest(package_root);
    let files = match (&manifest, filter) {
        (Some(manifest), Some(_)) => kind.manifest_entries(manifest).map_or_else(
            || collect_resource_files(&package_root.join(kind.directory()), kind),
            |entries| manifest_resource_files(package_root, entries, kind),
        ),
        (Some(manifest), None) => kind
            .manifest_entries(manifest)
            .map_or_else(Vec::new, |entries| {
                manifest_resource_files(package_root, entries, kind)
            }),
        (None, _) => collect_resource_files(&package_root.join(kind.directory()), kind),
    };
    let Some(filter) = filter else {
        return files;
    };
    let patterns = kind.filter_patterns(filter);
    if filter.autoload == Some(false) {
        return apply_autoload_disabled_patterns(
            &files,
            patterns.unwrap_or_default(),
            package_root,
        )
        .into_iter()
        .filter_map(|(path, enabled)| enabled.then_some(path))
        .collect();
    }
    let Some(patterns) = patterns else {
        return files;
    };
    if patterns.is_empty() {
        return Vec::new();
    }
    let enabled = apply_patterns(&files, patterns, package_root);
    files
        .into_iter()
        .filter(|path| enabled.contains(path))
        .collect()
}

fn manifest_resource_files(
    package_root: &Path,
    entries: &[String],
    kind: ResourceKind,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in entries.iter().filter(|entry| !is_override_pattern(entry)) {
        if has_glob_pattern(entry) {
            let pattern = package_root.join(entry).to_string_lossy().into_owned();
            if let Ok(matches) = glob::glob(&pattern) {
                paths.extend(matches.filter_map(Result::ok).map(|path| path.clean()));
            }
        } else {
            paths.push(package_root.join(entry).clean());
        }
    }
    let files = paths
        .into_iter()
        .flat_map(|path| collect_resource_files(&path, kind))
        .collect::<Vec<_>>();
    let overrides = entries
        .iter()
        .filter(|entry| is_override_pattern(entry))
        .cloned()
        .collect::<Vec<_>>();
    if overrides.is_empty() {
        files
    } else {
        let enabled = apply_patterns(&files, &overrides, package_root);
        files
            .into_iter()
            .filter(|path| enabled.contains(path))
            .collect()
    }
}

fn collect_resource_files(path: &Path, kind: ResourceKind) -> Vec<PathBuf> {
    if path.is_file() {
        return (path.extension().is_some_and(|extension| extension == "md"))
            .then(|| path.to_path_buf())
            .into_iter()
            .collect();
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let root = path.to_path_buf();
    let mut output = Vec::new();
    collect_resource_directory(path, &root, kind, &mut output);
    output
}

fn collect_resource_directory(
    directory: &Path,
    root: &Path,
    kind: ResourceKind,
    output: &mut Vec<PathBuf>,
) {
    let mut entries = match fs::read_dir(directory) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return,
    };
    entries.sort_by_key(fs::DirEntry::file_name);
    if matches!(kind, ResourceKind::Skill) {
        let declared = directory.join("SKILL.md");
        if declared.is_file() {
            output.push(declared);
            return;
        }
    }
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_resource_directory(&path, root, kind, output);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && (matches!(kind, ResourceKind::Prompt) || directory == root)
        {
            output.push(path);
        }
    }
}

fn collect_manifest_files(package_root: &Path) -> Vec<PathBuf> {
    if let Some(entries) = read_pi_manifest(package_root).and_then(|manifest| manifest.extensions)
        && !entries.is_empty()
    {
        return manifest_extension_files(package_root, &entries);
    }
    let convention = package_root.join("extensions");
    if convention.exists() {
        discover_extensions(&convention)
    } else {
        Vec::new()
    }
}

fn manifest_extension_files(package_root: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut resolved = Vec::new();
    for entry in entries.iter().filter(|entry| !is_override_pattern(entry)) {
        if has_glob_pattern(entry) {
            let pattern = package_root.join(entry).to_string_lossy().into_owned();
            if let Ok(matches) = glob::glob(&pattern) {
                resolved.extend(matches.filter_map(Result::ok).map(|path| path.clean()));
            }
        } else {
            resolved.push(package_root.join(entry).clean());
        }
    }
    let files = collect_extension_files(&resolved);
    let patterns: Vec<_> = entries
        .iter()
        .filter(|entry| is_override_pattern(entry))
        .cloned()
        .collect();
    if patterns.is_empty() {
        files
    } else {
        let enabled = apply_patterns(&files, &patterns, package_root);
        files
            .into_iter()
            .filter(|path| enabled.contains(path))
            .collect()
    }
}

fn collect_extension_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if metadata.is_file() {
            files.push(path.clone());
        } else if metadata.is_dir() {
            files.extend(discover_extensions(path));
        }
    }
    files
}

fn add_extension(
    extensions: &mut Vec<ResolvedExtension>,
    path: PathBuf,
    metadata: ExtensionMetadata,
    enabled: bool,
) {
    if !extensions.iter().any(|entry| entry.path == path) {
        extensions.push(ResolvedExtension {
            path,
            enabled,
            metadata,
        });
    }
}

fn sort_and_dedupe(mut extensions: Vec<ResolvedExtension>) -> Vec<ResolvedExtension> {
    extensions.sort_by_key(|entry| entry.metadata.rank());
    let mut seen = HashSet::new();
    extensions
        .into_iter()
        .filter(|entry| seen.insert(canonical_or_original(&entry.path)))
        .collect()
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(canonical_or_original(path)))
        .collect()
}

fn expand_entry(path: &Path) -> Vec<PathBuf> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => discover_extensions(path),
        Ok(_) => vec![path.to_path_buf()],
        Err(_) => Vec::new(),
    }
}

fn expand_resolved_extension(entry: ResolvedExtension) -> Vec<ResolvedExtension> {
    expand_entry(&entry.path)
        .into_iter()
        .map(|path| ResolvedExtension {
            path,
            enabled: entry.enabled,
            metadata: entry.metadata.clone(),
        })
        .collect()
}

fn apply_patterns(paths: &[PathBuf], patterns: &[String], base: &Path) -> HashSet<PathBuf> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();
    for pattern in patterns {
        if let Some(pattern) = pattern.strip_prefix('+') {
            force_includes.push(pattern.to_string());
        } else if let Some(pattern) = pattern.strip_prefix('-') {
            force_excludes.push(pattern.to_string());
        } else if let Some(pattern) = pattern.strip_prefix('!') {
            excludes.push(pattern.to_string());
        } else {
            includes.push(pattern.clone());
        }
    }
    let mut enabled: HashSet<_> = if includes.is_empty() {
        paths.iter().cloned().collect()
    } else {
        paths
            .iter()
            .filter(|path| matches_any_pattern(path, &includes, base))
            .cloned()
            .collect()
    };
    enabled.retain(|path| !matches_any_pattern(path, &excludes, base));
    for path in paths {
        if matches_any_exact_pattern(path, &force_includes, base) {
            enabled.insert(path.clone());
        }
    }
    enabled.retain(|path| !matches_any_exact_pattern(path, &force_excludes, base));
    enabled
}

fn apply_autoload_disabled_patterns(
    paths: &[PathBuf],
    patterns: &[String],
    base: &Path,
) -> Vec<(PathBuf, bool)> {
    let mut result: Vec<(PathBuf, bool)> = Vec::new();
    for pattern in patterns {
        let prefixed = pattern.starts_with(['+', '-', '!']);
        let target = if prefixed { &pattern[1..] } else { pattern };
        let exact = pattern.starts_with(['+', '-']);
        let enabled = !pattern.starts_with(['-', '!']);
        for path in paths {
            let matches = if exact {
                matches_any_exact_pattern(path, &[target.to_string()], base)
            } else {
                matches_any_pattern(path, &[target.to_string()], base)
            };
            if matches {
                if let Some(existing) = result.iter_mut().find(|(candidate, _)| candidate == path) {
                    existing.1 = enabled;
                } else {
                    result.push((path.clone(), enabled));
                }
            }
        }
    }
    result
}

fn is_enabled_by_overrides(path: &Path, patterns: &[String], base: &Path) -> bool {
    let overrides: Vec<_> = patterns
        .iter()
        .filter(|pattern| is_override_pattern(pattern))
        .collect();
    let excludes: Vec<_> = overrides
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('!'))
        .map(str::to_string)
        .collect();
    let includes: Vec<_> = overrides
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('+'))
        .map(str::to_string)
        .collect();
    let force_excludes: Vec<_> = overrides
        .iter()
        .filter_map(|pattern| pattern.strip_prefix('-'))
        .map(str::to_string)
        .collect();
    let mut enabled = excludes.is_empty() || !matches_any_pattern(path, &excludes, base);
    if matches_any_exact_pattern(path, &includes, base) {
        enabled = true;
    }
    if matches_any_exact_pattern(path, &force_excludes, base) {
        enabled = false;
    }
    enabled
}

fn matches_any_pattern(path: &Path, patterns: &[String], base: &Path) -> bool {
    let relative = pathdiff::diff_paths(path, base)
        .map(|path| posix_path(&path))
        .unwrap_or_default();
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let absolute = posix_path(path);
    let skill_parent = (name == "SKILL.md").then(|| path.parent()).flatten();
    let skill_parent_relative = skill_parent
        .and_then(|parent| pathdiff::diff_paths(parent, base))
        .map(|path| posix_path(&path));
    let skill_parent_name = skill_parent
        .and_then(Path::file_name)
        .and_then(OsStr::to_str);
    let skill_parent_absolute = skill_parent.map(posix_path);
    patterns.iter().any(|pattern| {
        let pattern = pattern.replace('\\', "/");
        glob_matches(&relative, &pattern)
            || glob_matches(name, &pattern)
            || glob_matches(&absolute, &pattern)
            || skill_parent_relative
                .as_deref()
                .is_some_and(|value| glob_matches(value, &pattern))
            || skill_parent_name.is_some_and(|value| glob_matches(value, &pattern))
            || skill_parent_absolute
                .as_deref()
                .is_some_and(|value| glob_matches(value, &pattern))
    })
}

fn matches_any_exact_pattern(path: &Path, patterns: &[String], base: &Path) -> bool {
    let relative = pathdiff::diff_paths(path, base)
        .map(|path| posix_path(&path))
        .unwrap_or_default();
    let absolute = posix_path(path);
    let skill_parent = path
        .file_name()
        .is_some_and(|name| name == "SKILL.md")
        .then(|| path.parent())
        .flatten();
    let skill_parent_relative = skill_parent
        .and_then(|parent| pathdiff::diff_paths(parent, base))
        .map(|path| posix_path(&path));
    let skill_parent_absolute = skill_parent.map(posix_path);
    patterns.iter().any(|pattern| {
        let normalized = pattern
            .strip_prefix("./")
            .or_else(|| pattern.strip_prefix(".\\"))
            .unwrap_or(pattern)
            .replace('\\', "/");
        normalized == relative
            || normalized == absolute
            || skill_parent_relative.as_deref() == Some(normalized.as_str())
            || skill_parent_absolute.as_deref() == Some(normalized.as_str())
    })
}

fn glob_matches(value: &str, pattern: &str) -> bool {
    Pattern::new(pattern).is_ok_and(|pattern| {
        pattern.matches_with(
            value,
            MatchOptions {
                case_sensitive: true,
                require_literal_separator: true,
                require_literal_leading_dot: true,
            },
        )
    })
}

fn is_pattern(value: &str) -> bool {
    is_override_pattern(value) || has_glob_pattern(value)
}

fn is_override_pattern(value: &str) -> bool {
    value.starts_with(['!', '+', '-'])
}

fn has_glob_pattern(value: &str) -> bool {
    value.contains(['*', '?'])
}

fn parse_source(source: &str) -> ParsedSource {
    if let Some(spec) = source.strip_prefix("npm:") {
        let spec = spec.trim().to_string();
        let (name, version) = parse_npm_spec(&spec);
        let range = version
            .as_deref()
            .and_then(|version| VersionRange::parse(version).ok());
        let pinned = version
            .as_deref()
            .is_some_and(|version| Version::parse(version).is_ok());
        return ParsedSource::Npm(NpmSource {
            spec,
            name,
            version,
            range,
            pinned,
        });
    }
    if is_local_path(source) {
        return ParsedSource::Local(LocalSource {
            path: source.to_string(),
        });
    }
    parse_git_source(source).map_or_else(
        || {
            ParsedSource::Local(LocalSource {
                path: source.to_string(),
            })
        },
        ParsedSource::Git,
    )
}

fn parse_npm_spec(spec: &str) -> (String, Option<String>) {
    let separator = if spec.starts_with('@') {
        spec.find('/')
            .and_then(|slash| spec[slash + 1..].find('@').map(|at| slash + 1 + at))
    } else {
        spec.rfind('@').filter(|index| *index > 0)
    };
    separator.map_or_else(
        || (spec.to_string(), None),
        |index| {
            (
                spec[..index].to_string(),
                Some(spec[index + 1..].to_string()),
            )
        },
    )
}

fn is_local_path(source: &str) -> bool {
    let source = source.trim();
    !["npm:", "git:", "github:", "http:", "https:", "ssh:"]
        .iter()
        .any(|prefix| source.starts_with(prefix))
}

fn parse_git_source(source: &str) -> Option<GitSource> {
    let source = source.trim();
    let prefixed = source.starts_with("git:");
    let value = if prefixed {
        source.strip_prefix("git:")?.trim()
    } else {
        source
    };
    if !prefixed
        && !["http://", "https://", "ssh://", "git://"]
            .iter()
            .any(|prefix| value.to_ascii_lowercase().starts_with(prefix))
    {
        return None;
    }
    let (repo_without_ref, reference) = split_git_ref(value);
    let (repo, host, path) = if let Some(captures) = parse_scp_like(&repo_without_ref) {
        (repo_without_ref, captures.0, captures.1)
    } else if repo_without_ref.contains("://") {
        let parsed = Url::parse(&repo_without_ref).ok()?;
        (
            repo_without_ref,
            parsed.host_str()?.to_string(),
            parsed.path().trim_start_matches('/').to_string(),
        )
    } else {
        let (host, path) = repo_without_ref.split_once('/')?;
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        (
            format!("https://{repo_without_ref}"),
            host.to_string(),
            path.to_string(),
        )
    };
    build_git_source(repo, host, path, reference)
}

fn split_git_ref(value: &str) -> (String, Option<String>) {
    if let Some((repo, reference)) = value.rsplit_once('#')
        && !repo.is_empty()
        && !reference.is_empty()
    {
        return (repo.to_string(), Some(reference.to_string()));
    }
    if let Some((host, path)) = parse_scp_like(value) {
        if let Some(index) = path.find('@') {
            let repo_path = &path[..index];
            let reference = &path[index + 1..];
            if !repo_path.is_empty() && !reference.is_empty() {
                return (
                    format!("git@{host}:{repo_path}"),
                    Some(reference.to_string()),
                );
            }
        }
        return (value.to_string(), None);
    }
    if value.contains("://") {
        if let Ok(mut parsed) = Url::parse(value) {
            let path = parsed.path().trim_start_matches('/');
            if let Some(index) = path.find('@') {
                let repo_path = &path[..index];
                let reference = &path[index + 1..];
                if !repo_path.is_empty() && !reference.is_empty() {
                    let repo_path = repo_path.to_string();
                    let reference = reference.to_string();
                    parsed.set_path(&format!("/{repo_path}"));
                    return (
                        parsed.as_str().trim_end_matches('/').to_string(),
                        Some(reference),
                    );
                }
            }
        }
        return (value.to_string(), None);
    }
    let Some(slash) = value.find('/') else {
        return (value.to_string(), None);
    };
    let path = &value[slash + 1..];
    let Some(index) = path.find('@') else {
        return (value.to_string(), None);
    };
    let repo_path = &path[..index];
    let reference = &path[index + 1..];
    if repo_path.is_empty() || reference.is_empty() {
        return (value.to_string(), None);
    }
    (
        format!("{}/{}", &value[..slash], repo_path),
        Some(reference.to_string()),
    )
}

fn parse_scp_like(value: &str) -> Option<(String, String)> {
    let value = value.strip_prefix("git@")?;
    let (host, path) = value.split_once(':')?;
    Some((host.to_string(), path.to_string()))
}

fn build_git_source(
    repo: String,
    host: String,
    path: String,
    reference: Option<String>,
) -> Option<GitSource> {
    let path = path
        .trim_start_matches('/')
        .trim_end_matches(".git")
        .to_string();
    if host.is_empty()
        || path.split('/').count() < 2
        || unsafe_git_part(&host, false)
        || unsafe_git_part(&path, true)
    {
        return None;
    }
    Some(GitSource {
        repo,
        host,
        path,
        pinned: reference.is_some(),
        reference,
    })
}

fn unsafe_git_part(value: &str, allow_slash: bool) -> bool {
    let decoded = percent_decode_str(value).decode_utf8().ok();
    [Some(value), decoded.as_deref()]
        .into_iter()
        .flatten()
        .any(|candidate| {
            candidate.contains('\0')
                || candidate.contains('\\')
                || candidate.starts_with('/')
                || (!allow_slash && candidate.contains('/'))
                || candidate.split('/').any(|part| part == "..")
        })
}

fn installed_npm_matches(source: &NpmSource, installed: &Path) -> bool {
    let Ok(contents) = fs::read(installed.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&contents) else {
        return false;
    };
    let Some(version) = value.get("version").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(version) = Version::parse(version) else {
        return false;
    };
    source
        .range
        .as_ref()
        .is_none_or(|range| range.satisfies(&version))
}

fn ensure_package_directory(directory: &Path) -> Result<(), PackageManagerError> {
    fs::create_dir_all(directory).map_err(|source| PackageManagerError::PrepareDirectory {
        path: directory.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
    }
    let ignore = directory.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "*\n!.gitignore\n").map_err(|source| {
            PackageManagerError::PrepareDirectory {
                path: ignore,
                source,
            }
        })?;
    }
    let package = directory.join("package.json");
    if !package.exists() {
        fs::write(
            &package,
            "{\n  \"name\": \"pi-extensions\",\n  \"private\": true\n}\n",
        )
        .map_err(|source| PackageManagerError::PrepareDirectory {
            path: package,
            source,
        })?;
    }
    Ok(())
}

fn managed_path(root: &Path, parts: &[&str]) -> Result<PathBuf, PackageManagerError> {
    let root = absolute_clean(root, None);
    let mut path = root.clone();
    for part in parts {
        if !part.is_empty() {
            path.push(part);
        }
    }
    let path = path.clean();
    if path != root && !path.starts_with(&root) {
        return Err(PackageManagerError::UnsafeManagedPath(path));
    }
    Ok(path)
}

fn resolve_path(input: &str, base: &Path) -> PathBuf {
    let mut normalized: String = input
        .trim()
        .chars()
        .map(|character| {
            if matches!(
                character,
                '\u{00a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            ) {
                ' '
            } else {
                character
            }
        })
        .collect();
    #[cfg(windows)]
    {
        normalized = normalize_windows_shell_path(&normalized);
    }
    if normalized == "~" {
        normalized = home_directory().display().to_string();
    } else if normalized.starts_with("~/") || normalized.starts_with("~\\") {
        normalized = home_directory()
            .join(&normalized[2..])
            .display()
            .to_string();
    } else if normalized.starts_with("file://")
        && let Ok(url) = Url::parse(&normalized)
        && let Ok(path) = url.to_file_path()
    {
        return path.clean();
    }
    absolute_clean(Path::new(&normalized), Some(base))
}

#[cfg(any(windows, test))]
fn normalize_windows_shell_path(path: &str) -> String {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        return path.to_string();
    }
    let components: Vec<_> = path.trim_start_matches('/').split('/').collect();
    let (drive, suffix) = match components.as_slice() {
        [prefix, drive, rest @ ..] if matches!(*prefix, "mnt" | "cygdrive") => (*drive, rest),
        [drive, rest @ ..] => (*drive, rest),
        _ => return path.to_string(),
    };
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return path.to_string();
    }
    let suffix = suffix.join("\\");
    format!("{}:\\{suffix}", drive.to_ascii_uppercase())
}

fn absolute_clean(path: &Path, base: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path.clean()
    } else {
        base.map_or_else(
            || {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(path)
                    .clean()
            },
            |base| base.join(path).clean(),
        )
    }
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn posix_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn offline() -> bool {
    std::env::var("PI_OFFLINE").is_ok_and(|value| {
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
    })
}

fn format_command(program: &str, arguments: &[String]) -> String {
    std::iter::once(program)
        .chain(arguments.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod path_tests {
    use super::normalize_windows_shell_path;

    #[test]
    fn windows_shell_paths_cover_msys_cygwin_and_wsl_shapes() {
        assert_eq!(normalize_windows_shell_path("/c/work/pi"), "C:\\work\\pi");
        assert_eq!(
            normalize_windows_shell_path("/mnt/d/work/pi"),
            "D:\\work\\pi"
        );
        assert_eq!(
            normalize_windows_shell_path("/cygdrive/e/work/pi"),
            "E:\\work\\pi"
        );
        assert_eq!(
            normalize_windows_shell_path("//server/share"),
            "//server/share"
        );
    }
}
