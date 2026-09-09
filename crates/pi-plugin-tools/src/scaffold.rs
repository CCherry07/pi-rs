use std::path::PathBuf;

use crate::{
    PluginKind, RUST_VERSION, Result, SOURCE_REPOSITORY, commit, invalid, io, stage, validate_id,
    write,
};

#[derive(Debug, Clone)]
pub enum SdkSource {
    /// A pi-rs checkout or its crates/pi-plugin-sdk directory.
    Path(PathBuf),
    /// Full commit hash, never a moving branch or tag.
    GitRevision(String),
}

#[derive(Debug, Clone)]
pub struct NewOptions {
    pub destination: PathBuf,
    pub name: String,
    pub kind: PluginKind,
    pub sdk: SdkSource,
}

/// Create a standalone plugin crate, a pinned toolchain, and release CI.
/// No dependency resolution, compilation, installation, or network operation occurs.
pub fn new_plugin(options: &NewOptions) -> Result<PathBuf> {
    validate_id(&options.name)?;
    if ["crate", "self", "super", "std", "core", "alloc", "test"].contains(&options.name.as_str()) {
        return Err(invalid(
            "choose a plugin name that is not a Rust reserved crate name",
        ));
    }
    let mut sdk = toml::Table::new();
    let revision = match &options.sdk {
        SdkSource::Path(path) => {
            let path = if path.join("crates/pi-plugin-sdk/Cargo.toml").is_file() {
                path.join("crates/pi-plugin-sdk")
            } else {
                path.clone()
            };
            let path = io(&path, path.canonicalize())?;
            let manifest: toml::Value = toml::from_str(&String::from_utf8_lossy(&crate::read(
                &path.join("Cargo.toml"),
            )?))
            .map_err(|e| invalid(e.to_string()))?;
            if manifest
                .get("package")
                .and_then(|v| v.get("name"))
                .and_then(toml::Value::as_str)
                != Some("pi-plugin-sdk")
            {
                return Err(invalid(
                    "--sdk must point to pi-rs or its pi-plugin-sdk crate",
                ));
            }
            sdk.insert("path".into(), path.to_string_lossy().into_owned().into());
            None
        }
        SdkSource::GitRevision(revision) => {
            if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid(
                    "SDK revision must be a full 40-character Git commit hash; use --sdk for a local checkout",
                ));
            }
            sdk.insert("git".into(), SOURCE_REPOSITORY.into());
            sdk.insert("rev".into(), revision.clone().into());
            Some(revision.as_str())
        }
    };
    sdk.insert(
        "features".into(),
        toml::Value::Array(vec![options.kind.as_str().into()]),
    );
    let manifest = toml::Table::from_iter([
        (
            "package".into(),
            toml::Value::Table(toml::Table::from_iter([
                ("name".into(), options.name.clone().into()),
                ("version".into(), "0.1.0".into()),
                ("edition".into(), "2024".into()),
                ("publish".into(), false.into()),
            ])),
        ),
        (
            "lib".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "crate-type".into(),
                toml::Value::Array(vec!["cdylib".into(), "rlib".into()]),
            )])),
        ),
        (
            "dependencies".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "pi-plugin-sdk".into(),
                sdk.into(),
            )])),
        ),
        // Keep the crate independent of any ancestor workspace.
        ("workspace".into(), toml::Table::new().into()),
    ]);
    let (destination, staging) = stage(&options.destination)?;
    write(
        &staging.path().join("Cargo.toml"),
        toml::to_string_pretty(&manifest).map_err(|e| invalid(e.to_string()))?,
    )?;
    let source_dir = staging.path().join("src");
    io(&source_dir, std::fs::create_dir(&source_dir))?;
    let trait_name = match options.kind {
        PluginKind::Agent => "AgentPlugin",
        PluginKind::Provider => "ProviderPlugin",
        PluginKind::Session => "SessionPlugin",
    };
    write(
        &source_dir.join("lib.rs"),
        format!(
            "use pi_plugin_sdk::{}::prelude::*;\n\n#[derive(Default)]\npub struct Plugin;\n\n#[pi_plugin_sdk::{}]\nimpl {trait_name} for Plugin {{}}\n\n#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn exports_expected_identity() {{\n        assert_eq!(Plugin.id().as_str(), {:?});\n    }}\n}}\n",
            options.kind.as_str(),
            options.kind.as_str(),
            options.name,
        ),
    )?;
    write(
        &staging.path().join("rust-toolchain.toml"),
        format!(
            "[toolchain]\nchannel = {RUST_VERSION:?}\nprofile = \"minimal\"\ncomponents = [\"rustfmt\", \"clippy\"]\n"
        ),
    )?;
    write(
        &staging.path().join(".gitignore"),
        "/target/\n/dist/\n/release/\n/.pi/\n/pi-host/\n",
    )?;
    // Preserve the host's registry dependency choices on the first build. Cargo
    // adds this crate and adjusts path/Git source identities on that first build.
    write(
        &staging.path().join("Cargo.lock"),
        include_str!("../../../Cargo.lock"),
    )?;
    let workflow_dir = staging.path().join(".github/workflows");
    io(&workflow_dir, std::fs::create_dir_all(&workflow_dir))?;
    let workflow = include_str!("../templates/release.yml")
        .replace(
            "__SDK_REVISION__",
            revision.unwrap_or("REPLACE_WITH_MATCHING_PI_RS_COMMIT"),
        )
        .replace("__RUST_VERSION__", RUST_VERSION);
    // A local path cannot be reproduced on a runner without an explicit revision.
    let workflow_file = if revision.is_some() {
        "release.yml"
    } else {
        "release.yml.example"
    };
    write(&workflow_dir.join(workflow_file), workflow)?;
    write(
        &staging.path().join("README.md"),
        format!(
            "# {}\n\nA native pi-rs {} plugin.\n\n~~~sh\npi plugin package\npi plugin verify dist\npi plugin install ./dist\n~~~\n\nThe first package build completes the seeded Cargo.lock. Commit Cargo.lock afterwards and use\npi plugin package --locked --output dist-next for reproducible builds. Outputs must not already exist.\n\nPackaging builds for the running host and validates the binary with its native loader.\nUse a pi-rs binary built from the same SDK checkout, Rust toolchain and Rust flags.\nAn SDK version alone does not establish Rust ABI compatibility. Never bypass a fingerprint mismatch.\n\nThe release workflow builds each target on its native runner, then merges its bundles.\n{}\n\nAfter inspecting the release assets, publish with:\n\n~~~sh\npi plugin publish github --bundle dist --repo OWNER/REPO --tag v0.1.0\n~~~\n\nThis requires an authenticated GitHub CLI and an existing tag. Publication creates a draft first;\na failed upload leaves a draft that can be inspected, and existing assets are never overwritten.\nOnly listed artifacts and release metadata are uploaded.\n",
            options.name,
            options.kind.as_str(),
            if revision.is_some() {
                "The workflow pins the same SDK revision as Cargo.toml. Commit changes before running it."
            } else {
                "The workflow is a disabled .example: replace the SDK path with a pinned Git dependency, set the matching workflow revision, and rename it to .yml before enabling CI."
            },
        ),
    )?;
    commit(&destination, staging)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_each_kind_without_overwriting_user_work() {
        let root = tempfile::tempdir().unwrap();
        for kind in [PluginKind::Agent, PluginKind::Provider, PluginKind::Session] {
            let options = NewOptions {
                destination: root.path().join(kind.as_str()),
                name: format!("hello-{}", kind.as_str()),
                kind,
                sdk: SdkSource::GitRevision("a".repeat(40)),
            };
            let path = new_plugin(&options).unwrap();
            let manifest: toml::Value =
                toml::from_str(&std::fs::read_to_string(path.join("Cargo.toml")).unwrap()).unwrap();
            assert_eq!(
                manifest["dependencies"]["pi-plugin-sdk"]["rev"].as_str(),
                Some("a".repeat(40).as_str())
            );
            assert_eq!(
                manifest["dependencies"]["pi-plugin-sdk"]["features"][0].as_str(),
                Some(kind.as_str())
            );
            assert!(path.join(".github/workflows/release.yml").is_file());
            assert!(
                new_plugin(&options)
                    .unwrap_err()
                    .to_string()
                    .contains("already exists")
            );
        }
    }

    #[test]
    fn rejects_unsafe_names_and_unpinned_revisions_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let mut options = NewOptions {
            destination: root.path().join("output"),
            name: "../escape".into(),
            kind: PluginKind::Agent,
            sdk: SdkSource::GitRevision("a".repeat(40)),
        };
        assert!(new_plugin(&options).is_err());
        options.name = "hello".into();
        options.sdk = SdkSource::GitRevision("main".into());
        assert!(new_plugin(&options).is_err());
        assert!(!options.destination.exists());
    }
}
