use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use pi_plugin_loader::{NativePluginDescriptor, NativePluginLoader, NativePluginLoaderOptions};
use pi_plugin_manager::{ReleaseArtifact, ReleaseManifest};
use sha2::{Digest, Sha256};

use crate::{
    HOST_TARGET, PluginKind, RELEASE_FILE, Result, command_output, commit, invalid, io, json, read,
    stage, validate_id, write, write_json,
};

#[derive(Debug)]
pub struct Verification {
    pub release: ReleaseManifest,
    /// Present only when this platform's binary passed the actual host loader.
    pub host_verified: Option<String>,
}

struct Bundle {
    release: ReleaseManifest,
    files: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn inspect_library(artifact: &Path) -> Result<NativePluginDescriptor> {
    let scratch = io(artifact, tempfile::tempdir())?;
    let mut options = NativePluginLoaderOptions::new(scratch.path(), scratch.path().join("agent"));
    options.explicit_paths.push(artifact.to_path_buf());
    let plugins = NativePluginLoader::new(options).discover()?;
    let mut descriptors = plugins.descriptors();
    if descriptors.len() != 1 {
        return Err(invalid("expected exactly one native plugin descriptor"));
    }
    Ok(descriptors.remove(0))
}

/// Verify every checksum and, by default, this host's descriptor/ABI/constructor
/// export. Native library initialization code can run; plugin constructors do not.
/// Integrity-only verification deliberately makes no binary compatibility claim.
pub fn verify(path: &Path, integrity_only: bool) -> Result<Verification> {
    let bundle = read_bundle(path)?;
    verify_bundle(&bundle, integrity_only)?;
    Ok(Verification {
        release: bundle.release,
        host_verified: (!integrity_only).then(|| HOST_TARGET.into()),
    })
}

fn verify_bundle(bundle: &Bundle, integrity_only: bool) -> Result<()> {
    if integrity_only {
        return Ok(());
    }
    let artifact = bundle.release.artifacts.iter().find(|artifact| artifact.target == HOST_TARGET)
        .ok_or_else(|| invalid(format!("release has no artifact for {HOST_TARGET}; --integrity-only checks files without loading a binary")))?;
    let scratch = io(Path::new("plugin verification"), tempfile::tempdir())?;
    // Load the exact bytes whose checksum was checked, not a mutable source path.
    write(
        &scratch.path().join(&artifact.url),
        &bundle.files[&artifact.url],
    )?;
    write_local_manifest(scratch.path(), &bundle.release, artifact)?;
    inspect_library(&scratch.path().join("pi-plugin.toml"))?;
    Ok(())
}

fn read_bundle(path: &Path) -> Result<Bundle> {
    let manifest = if path.is_dir() {
        path.join(RELEASE_FILE)
    } else {
        path.to_path_buf()
    };
    let release: ReleaseManifest = json(&manifest)?;
    if release.schema != 1 {
        return Err(invalid("unsupported plugin release schema"));
    }
    validate_id(&release.id)?;
    release.kind.parse::<PluginKind>().map_err(invalid)?;
    semver::Version::parse(&release.version)
        .map_err(|e| invalid(format!("invalid release version: {e}")))?;
    if !release.options.is_object() {
        return Err(invalid("plugin options must be a JSON object"));
    }
    if release.artifacts.is_empty() {
        return Err(invalid("release must contain at least one artifact"));
    }
    let directory = manifest.parent().unwrap_or(Path::new("."));
    let directory = io(directory, directory.canonicalize())?;
    let mut targets = HashSet::new();
    let mut files = BTreeMap::new();
    for artifact in &release.artifacts {
        if !valid_target(&artifact.target) {
            return Err(invalid(format!("invalid Rust target {}", artifact.target)));
        }
        if !targets.insert(&artifact.target) {
            return Err(invalid(format!("duplicate target {}", artifact.target)));
        }
        validate_filename(&artifact.url)?;
        if matches!(artifact.url.as_str(), RELEASE_FILE | "pi-plugin.toml") {
            return Err(invalid("artifact filename collides with package metadata"));
        }
        // Release bundles are self-contained. Remote URLs are installer inputs,
        // not local packager inputs, and URL escapes have no local-path meaning.
        if artifact
            .file_name
            .as_ref()
            .is_some_and(|name| name != &artifact.url)
        {
            return Err(invalid(
                "bundle artifact file_name must equal its relative URL",
            ));
        }
        let suffix = if artifact.target.contains("-windows-") {
            ".dll"
        } else if artifact.target.ends_with("-apple-darwin") {
            ".dylib"
        } else if artifact.target.contains("-linux-") {
            ".so"
        } else {
            return Err(invalid(format!(
                "unsupported native plugin target {}",
                artifact.target
            )));
        };
        if !artifact.url.ends_with(suffix) {
            return Err(invalid(format!(
                "artifact for {} must end in {suffix}",
                artifact.target
            )));
        }
        let file = directory.join(&artifact.url);
        let resolved = io(&file, file.canonicalize())?;
        if !resolved.starts_with(&directory) || !resolved.is_file() {
            return Err(invalid(format!(
                "artifact {} must be a file inside the bundle",
                artifact.url
            )));
        }
        let bytes = read(&resolved)?;
        let actual = digest(&bytes);
        if !actual.eq_ignore_ascii_case(&artifact.sha256) {
            return Err(invalid(format!(
                "SHA-256 mismatch for {}: expected {}, got {actual}",
                artifact.url, artifact.sha256
            )));
        }
        if files.insert(artifact.url.clone(), bytes).is_some() {
            return Err(invalid(format!(
                "duplicate artifact filename {}",
                artifact.url
            )));
        }
    }
    // Validate the companion local install manifest when present. Otherwise an
    // intact release manifest could mask a tampered local install target/options.
    let local_manifest = directory.join("pi-plugin.toml");
    if local_manifest.exists() {
        let actual: toml::Value = toml::from_str(&String::from_utf8_lossy(&read(&local_manifest)?))
            .map_err(|e| invalid(format!("invalid pi-plugin.toml: {e}")))?;
        let matching = release
            .artifacts
            .iter()
            .find(|artifact| {
                actual
                    .get("plugin")
                    .and_then(|plugin| plugin.get("artifact"))
                    .and_then(toml::Value::as_str)
                    == Some(artifact.url.as_str())
            })
            .ok_or_else(|| invalid("local manifest artifact is absent from the release"))?;
        let expected = local_manifest_value(&release, matching)?;
        if actual != expected {
            return Err(invalid(
                "pi-plugin.toml disagrees with the release manifest",
            ));
        }
    }
    Ok(Bundle { release, files })
}

fn valid_target(target: &str) -> bool {
    target.len() <= 128
        && target.split('-').count() >= 3
        && target
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn validate_filename(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 240
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(invalid(format!(
            "artifact URL must be a plain relative filename: {name}"
        )));
    }
    Ok(())
}

fn local_manifest_value(
    release: &ReleaseManifest,
    artifact: &ReleaseArtifact,
) -> Result<toml::Value> {
    let value = serde_json::json!({
        "schema": 1,
        "plugin": { "id": release.id, "version": release.version, "kind": release.kind, "artifact": artifact.url },
        "options": release.options,
    });
    toml::Value::try_from(value).map_err(|e| {
        invalid(format!(
            "options cannot be represented in pi-plugin.toml: {e}"
        ))
    })
}

pub(crate) fn write_local_manifest(
    directory: &Path,
    release: &ReleaseManifest,
    artifact: &ReleaseArtifact,
) -> Result<()> {
    let value = local_manifest_value(release, artifact)?;
    write(
        &directory.join("pi-plugin.toml"),
        toml::to_string_pretty(&value).map_err(|e| invalid(e.to_string()))?,
    )
}

/// Merge native-runner bundles. Every input must have the same identity, version,
/// kind and defaults, with disjoint targets and artifact filenames.
pub fn merge(inputs: &[PathBuf], output: &Path) -> Result<PathBuf> {
    if inputs.is_empty() {
        return Err(invalid("provide at least one bundle to merge"));
    }
    let mut bundles = inputs
        .iter()
        .map(|path| read_bundle(path))
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let mut combined = bundles.next().expect("inputs is nonempty");
    for bundle in bundles {
        if bundle.release.id != combined.release.id
            || bundle.release.version != combined.release.version
            || bundle.release.kind != combined.release.kind
            || bundle.release.options != combined.release.options
        {
            return Err(invalid(
                "all bundles must have the same plugin id, version, kind and options",
            ));
        }
        for artifact in bundle.release.artifacts {
            if combined
                .release
                .artifacts
                .iter()
                .any(|existing| existing.target == artifact.target)
            {
                return Err(invalid(format!("duplicate target {}", artifact.target)));
            }
            combined.release.artifacts.push(artifact);
        }
        for (name, bytes) in bundle.files {
            if combined.files.insert(name.clone(), bytes).is_some() {
                return Err(invalid(format!("duplicate artifact filename {name}")));
            }
        }
    }
    combined
        .release
        .artifacts
        .sort_by(|a, b| a.target.cmp(&b.target));
    let (destination, staging) = stage(output)?;
    write_bundle(staging.path(), &combined)?;
    commit(&destination, staging)
}

fn write_bundle(directory: &Path, bundle: &Bundle) -> Result<()> {
    for (name, bytes) in &bundle.files {
        write(&directory.join(name), bytes)?;
    }
    // A multi-target release is for remote installation. Only a single-target
    // bundle has an unambiguous local loader manifest.
    if bundle.release.artifacts.len() == 1 {
        write_local_manifest(directory, &bundle.release, &bundle.release.artifacts[0])?;
    }
    write_json(&directory.join(RELEASE_FILE), &bundle.release)
}

/// Produce a static registry fragment without mutating an existing index.
pub fn registry_entry(bundle: &Path, manifest_url: &str) -> Result<serde_json::Value> {
    let release = read_bundle(bundle)?.release;
    let url =
        url::Url::parse(manifest_url).map_err(|e| invalid(format!("invalid manifest URL: {e}")))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(
            "registry release manifest must have a credential-free HTTPS URL",
        ));
    }
    Ok(
        serde_json::json!({ "schema": 1, "plugins": { release.id: [{ "version": release.version, "manifest": manifest_url }] } }),
    )
}

/// Publish exactly the validated manifest and artifacts through an authenticated
/// GitHub CLI. Existing releases/assets are never overwritten. No shell is used.
pub fn publish_github(path: &Path, repository: &str, tag: &str, draft: bool) -> Result<String> {
    let bundle = read_bundle(path)?;
    validate_publication(&bundle.release, repository, tag)?;
    verify_bundle(&bundle, false)?;
    publish_with(&bundle, repository, tag, draft, |command| {
        command_output(command).map(|_| ())
    })
}

fn publish_with(
    bundle: &Bundle,
    repository: &str,
    tag: &str,
    draft: bool,
    mut run: impl FnMut(&mut Command) -> Result<()>,
) -> Result<String> {
    // Snapshot before any remote writes. Mutation of the source directory while
    // gh runs cannot substitute an unverified artifact or add unrelated files.
    let snapshot = io(Path::new("plugin publication"), tempfile::tempdir())?;
    write_bundle(snapshot.path(), bundle)?;
    let mut create = Command::new("gh");
    create
        .args([
            "release",
            "create",
            tag,
            "--repo",
            repository,
            "--verify-tag",
            "--draft",
            "--title",
            tag,
            "--notes",
        ])
        .arg(format!(
            "Native {} plugin {} {}.",
            bundle.release.kind, bundle.release.id, bundle.release.version
        ));
    run(&mut create)?;
    let result = (|| {
        let mut upload = Command::new("gh");
        upload
            .args(["release", "upload", tag, "--repo", repository])
            .arg(snapshot.path().join(RELEASE_FILE));
        for name in bundle.files.keys() {
            upload.arg(snapshot.path().join(name));
        }
        run(&mut upload)?;
        if !draft {
            run(Command::new("gh").args([
                "release",
                "edit",
                tag,
                "--repo",
                repository,
                "--draft=false",
            ]))?;
        }
        Ok(())
    })();
    result.map_err(|e: crate::Error| {
        invalid(format!(
            "publication failed; inspect the retained draft {repository}@{tag}: {e}"
        ))
    })?;
    Ok(format!(
        "https://github.com/{repository}/releases/tag/{tag}"
    ))
}

fn validate_publication(release: &ReleaseManifest, repository: &str, tag: &str) -> Result<()> {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || part.starts_with('.')
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    {
        return Err(invalid("--repo must be OWNER/REPO"));
    }
    if tag != format!("v{}", release.version) {
        return Err(invalid(format!(
            "release tag must match the plugin version: v{}",
            release.version
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, name: &str, target: &str) -> PathBuf {
        let directory = root.join(name);
        std::fs::create_dir(&directory).unwrap();
        let filename = format!(
            "hello-{target}.{}",
            if target.contains("apple") {
                "dylib"
            } else {
                "so"
            }
        );
        let bytes = target.as_bytes();
        write(&directory.join(&filename), bytes).unwrap();
        write_json(
            &directory.join(RELEASE_FILE),
            &ReleaseManifest {
                schema: 1,
                id: "hello".into(),
                version: "1.2.3".into(),
                kind: "agent".into(),
                options: serde_json::json!({}),
                artifacts: vec![ReleaseArtifact {
                    target: target.into(),
                    url: filename,
                    sha256: digest(bytes),
                    file_name: None,
                }],
            },
        )
        .unwrap();
        directory
    }

    #[test]
    fn merges_target_bundles_deterministically_and_refuses_collisions() {
        let root = tempfile::tempdir().unwrap();
        let mac = fixture(root.path(), "mac", "aarch64-apple-darwin");
        let linux = fixture(root.path(), "linux", "x86_64-unknown-linux-gnu");
        let merged = merge(&[linux.clone(), mac.clone()], &root.path().join("merged")).unwrap();
        let reverse = merge(&[mac.clone(), linux], &root.path().join("reverse")).unwrap();
        assert_eq!(
            read(&merged.join(RELEASE_FILE)).unwrap(),
            read(&reverse.join(RELEASE_FILE)).unwrap()
        );
        assert_eq!(verify(&merged, true).unwrap().release.artifacts.len(), 2);
        assert!(!merged.join("pi-plugin.toml").exists());
        assert!(merge(&[mac.clone(), mac], &root.path().join("duplicate")).is_err());
        assert!(!root.path().join("duplicate").exists());
        // Checksums do not make these synthetic bytes loadable.
        assert!(verify(&merged, false).is_err());
    }

    #[test]
    fn rejects_corrupt_missing_and_escaping_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let directory = fixture(root.path(), "bundle", "x86_64-unknown-linux-gnu");
        let path = directory.join(RELEASE_FILE);
        let mut release: ReleaseManifest = json(&path).unwrap();
        release.artifacts[0].sha256 = "0".repeat(64);
        write_json(&path, &release).unwrap();
        assert!(
            verify(&directory, true)
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
        release.artifacts[0].url = "../escape.so".into();
        write_json(&path, &release).unwrap();
        assert!(
            verify(&directory, true)
                .unwrap_err()
                .to_string()
                .contains("relative filename")
        );
        release.artifacts[0].url = "missing.so".into();
        write_json(&path, &release).unwrap();
        assert!(verify(&directory, true).is_err());
    }

    #[test]
    fn detects_local_install_manifest_tampering() {
        let root = tempfile::tempdir().unwrap();
        let directory = fixture(root.path(), "bundle", "aarch64-apple-darwin");
        let release: ReleaseManifest = json(&directory.join(RELEASE_FILE)).unwrap();
        write_local_manifest(&directory, &release, &release.artifacts[0]).unwrap();
        verify(&directory, true).unwrap();
        let mut changed = release.clone();
        changed.options = serde_json::json!({"unexpected": true});
        write_local_manifest(&directory, &changed, &release.artifacts[0]).unwrap();
        assert!(
            verify(&directory, true)
                .unwrap_err()
                .to_string()
                .contains("disagrees")
        );
    }

    #[test]
    fn validates_publication_coordinates_and_generates_registry_fragment() {
        let root = tempfile::tempdir().unwrap();
        let directory = fixture(root.path(), "bundle", "aarch64-apple-darwin");
        let release = verify(&directory, true).unwrap().release;
        validate_publication(&release, "owner/repo", "v1.2.3").unwrap();
        assert!(validate_publication(&release, "--repo/other", "v1.2.3").is_err());
        assert!(validate_publication(&release, "owner/repo", "v2.0.0").is_err());
        let fragment = registry_entry(&directory, "https://example.com/release.json").unwrap();
        assert_eq!(fragment["plugins"]["hello"][0]["version"], "1.2.3");
        assert!(registry_entry(&directory, "http://example.com/release.json").is_err());
    }

    #[test]
    fn publication_uploads_only_verified_snapshots_and_never_exposes_a_failed_upload() {
        let root = tempfile::tempdir().unwrap();
        let directory = fixture(root.path(), "bundle", "aarch64-apple-darwin");
        write(&directory.join("private.env"), b"must not be uploaded").unwrap();
        let bundle = read_bundle(&directory).unwrap();
        let name = bundle.release.artifacts[0].url.clone();
        let mut calls = Vec::new();
        let url = publish_with(&bundle, "owner/repo", "v1.2.3", false, |command| {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[1] == "create" {
                // A source mutation after verification must not change uploaded bytes.
                write(&directory.join(&name), b"changed after verification")?;
            }
            if args[1] == "upload" {
                let paths = args[5..].iter().map(PathBuf::from).collect::<Vec<_>>();
                assert_eq!(paths.len(), 2);
                assert!(
                    paths
                        .iter()
                        .all(|path| path.file_name().unwrap() == RELEASE_FILE
                            || path.file_name().unwrap() == name.as_str())
                );
                let artifact = paths
                    .iter()
                    .find(|path| path.file_name().unwrap() == name.as_str())
                    .unwrap();
                assert_eq!(read(artifact)?, bundle.files[&name]);
                assert!(!args.iter().any(|arg| arg == "--clobber"));
            }
            calls.push(args[1].clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(url, "https://github.com/owner/repo/releases/tag/v1.2.3");
        assert_eq!(calls, ["create", "upload", "edit"]);

        let mut calls = Vec::new();
        let error = publish_with(&bundle, "owner/repo", "v1.2.3", false, |command| {
            let action = command
                .get_args()
                .nth(1)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            calls.push(action.clone());
            if action == "upload" {
                Err(invalid("upload failed"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(calls, ["create", "upload"]);
        assert!(error.to_string().contains("retained draft"));

        calls.clear();
        publish_with(&bundle, "owner/repo", "v1.2.3", true, |command| {
            calls.push(
                command
                    .get_args()
                    .nth(1)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, ["create", "upload"]);
    }
}
