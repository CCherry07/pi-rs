use std::path::{Path, PathBuf};
use std::process::Command;

use pi_plugin_manager::{ReleaseArtifact, ReleaseManifest};
use serde::Deserialize;

use crate::bundle::{digest, inspect_library, write_local_manifest};
use crate::{
    HOST_TARGET, RELEASE_FILE, Result, command_output, commit, invalid, io, read, stage,
    validate_id, write, write_json,
};

#[derive(Debug, Clone)]
pub struct PackageOptions {
    /// A plugin crate directory or its Cargo.toml (not a virtual workspace).
    pub manifest_path: PathBuf,
    pub output: PathBuf,
    /// Override Cargo's compilation cache without changing the release output.
    pub target_dir: Option<PathBuf>,
    pub locked: bool,
    pub debug: bool,
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    crate_types: Vec<String>,
}

/// Build the plugin on its native target, validate the exported descriptor with
/// the host loader, and emit an installable directory and remote release manifest.
pub fn package(options: &PackageOptions) -> Result<PathBuf> {
    let manifest = if options.manifest_path.is_dir() {
        options.manifest_path.join("Cargo.toml")
    } else {
        options.manifest_path.clone()
    };
    let manifest = io(&manifest, manifest.canonicalize())?;
    let root = manifest
        .parent()
        .ok_or_else(|| invalid("Cargo.toml needs a parent directory"))?;
    // Reserve a staging directory before running a potentially expensive build.
    let (destination, staging) = stage(&options.output)?;
    let mut metadata_command = cargo(root);
    metadata_command
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(&manifest);
    if options.locked {
        metadata_command.arg("--locked");
    }
    let output = command_output(&mut metadata_command)?;
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .map_err(|e| invalid(format!("invalid cargo metadata: {e}")))?;
    let package = metadata
        .packages
        .into_iter()
        .find(|p| p.manifest_path == manifest)
        .ok_or_else(|| invalid("select a plugin crate's Cargo.toml, not a virtual workspace"))?;
    let library = package
        .targets
        .iter()
        .find(|target| target.crate_types.iter().any(|kind| kind == "cdylib"))
        .ok_or_else(|| {
            invalid("plugin crate must declare [lib] crate-type = [\"cdylib\", \"rlib\"]")
        })?;
    let mut command = cargo(root);
    command
        .args([
            "build",
            "--lib",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&manifest)
        .args(["--package", &package.name, "--target", HOST_TARGET]);
    if options.locked {
        command.arg("--locked");
    }
    if let Some(target_dir) = &options.target_dir {
        command.arg("--target-dir").arg(target_dir);
    }
    if !options.debug {
        command.arg("--release");
    }
    let output = command_output(&mut command)?;
    let artifact = artifact_from_messages(&output.stdout, &library.name)?;
    let descriptor = inspect_library(&artifact)?;
    validate_id(&descriptor.id)?;
    if descriptor.version != package.version {
        return Err(invalid(format!(
            "binary version {} does not match Cargo package version {}",
            descriptor.version, package.version
        )));
    }
    semver::Version::parse(&descriptor.version)
        .map_err(|e| invalid(format!("invalid plugin version: {e}")))?;
    let kind = match descriptor.kind {
        pi_plugin_sdk::NativePluginKind::Agent => "agent",
        pi_plugin_sdk::NativePluginKind::Provider => "provider",
        pi_plugin_sdk::NativePluginKind::Session => "session",
    };
    let options_value = package
        .metadata
        .get("pi-plugin")
        .and_then(|value| value.get("options"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if !options_value.is_object() {
        return Err(invalid(
            "package.metadata.pi-plugin.options must be an object",
        ));
    }
    let bytes = read(&artifact)?;
    let filename = format!(
        "{}-{}-{HOST_TARGET}{}",
        descriptor.id,
        descriptor.version,
        std::env::consts::DLL_SUFFIX
    );
    let release = ReleaseManifest {
        schema: 1,
        id: descriptor.id,
        version: descriptor.version,
        kind: kind.into(),
        options: options_value,
        artifacts: vec![ReleaseArtifact {
            target: HOST_TARGET.into(),
            url: filename.clone(),
            sha256: digest(&bytes),
            file_name: None,
        }],
    };
    write(&staging.path().join(&filename), bytes)?;
    write_local_manifest(staging.path(), &release, &release.artifacts[0])?;
    write_json(&staging.path().join(RELEASE_FILE), &release)?;
    crate::bundle::verify(staging.path(), false)?;
    commit(&destination, staging)
}

fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

fn artifact_from_messages(messages: &[u8], library: &str) -> Result<PathBuf> {
    let mut artifact = None;
    for line in messages.split(|b| *b == b'\n') {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" || message["target"]["name"] != library {
            continue;
        }
        if !message["target"]["crate_types"]
            .as_array()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "cdylib"))
        {
            continue;
        }
        if let Some(files) = message["filenames"].as_array() {
            for file in files.iter().filter_map(serde_json::Value::as_str) {
                if file.ends_with(std::env::consts::DLL_SUFFIX) {
                    artifact = Some(PathBuf::from(file));
                }
            }
        }
    }
    artifact.ok_or_else(|| {
        invalid("cargo did not report a native cdylib artifact for the plugin crate")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_cdylib_not_dependency_or_rlib_from_cargo_output() {
        let name = format!("/build/hello{}", std::env::consts::DLL_SUFFIX);
        let lines = [
            serde_json::json!({"reason":"compiler-artifact","target":{"name":"dependency","crate_types":["cdylib"]},"filenames":[format!("/build/dependency{}", std::env::consts::DLL_SUFFIX)]}),
            serde_json::json!({"reason":"compiler-artifact","target":{"name":"hello","crate_types":["cdylib","rlib"]},"filenames":[name,"/build/libhello.rlib"]}),
        ].iter().map(serde_json::Value::to_string).collect::<Vec<_>>().join("\n");
        assert_eq!(
            artifact_from_messages(lines.as_bytes(), "hello").unwrap(),
            PathBuf::from(name)
        );
        assert!(artifact_from_messages(lines.as_bytes(), "missing").is_err());
    }
}
