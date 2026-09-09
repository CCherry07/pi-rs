//! Native plugin authoring, release assembly, and publication. Runtime loading and
//! installation stay in their existing modules; this module never edits plugin intent.

mod bundle;
mod cargo;
mod scaffold;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use bundle::{Verification, merge, publish_github, registry_entry, verify};
pub use cargo::{PackageOptions, package};
pub use pi_plugin_manager::HOST_TARGET;
pub use scaffold::{NewOptions, SdkSource, new_plugin};
use serde::{Deserialize, Serialize};

pub const SOURCE_REVISION: &str = env!("PI_PLUGIN_SOURCE_REVISION");
pub const RUST_VERSION: &str = env!("PI_PLUGIN_RUST_VERSION");
pub const RELEASE_FILE: &str = "pi-plugin-release.json";
pub const SOURCE_REPOSITORY: &str = "https://github.com/CCherry07/pi-rs.git";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Loader(#[from] pi_plugin_loader::NativePluginError),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Agent,
    Provider,
    Session,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Provider => "provider",
            Self::Session => "session",
        }
    }
}

impl std::str::FromStr for PluginKind {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "agent" => Ok(Self::Agent),
            "provider" => Ok(Self::Provider),
            "session" => Ok(Self::Session),
            _ => Err("plugin kind must be agent, provider, or session".to_string()),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn io<T>(path: &Path, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read(path: &Path) -> Result<Vec<u8>> {
    io(path, fs::read(path))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    io(path, fs::write(path, bytes))
}

fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&read(path)?).map_err(|e| invalid(format!("{}: {e}", path.display())))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| invalid(e.to_string()))?;
    bytes.push(b'\n');
    write(path, bytes)
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id.as_bytes()[id.len() - 1].is_ascii_alphanumeric()
    {
        return Err(invalid(
            "plugin name must start with a lowercase letter, contain only lowercase letters, digits and hyphens, and end with a letter or digit (max 128 bytes)",
        ));
    }
    Ok(())
}

fn command_output(command: &mut Command) -> Result<std::process::Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| invalid(format!("cannot run {program}: {e}")))?;
    if !output.status.success() {
        return Err(invalid(format!(
            "{program} failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

// Stage complete directories beside their destination. Existing destinations are
// never replaced, including an empty directory or dangling symlink.
fn stage(destination: &Path) -> Result<(PathBuf, tempfile::TempDir)> {
    let destination = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        io(destination, std::env::current_dir())?.join(destination)
    };
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(invalid(format!(
            "{} already exists; choose a new output directory",
            destination.display()
        )));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("output needs a parent directory"))?;
    io(parent, fs::create_dir_all(parent))?;
    let staging = io(
        parent,
        tempfile::Builder::new()
            .prefix(".pi-plugin-")
            .tempdir_in(parent),
    )?;
    Ok((destination, staging))
}

fn commit(destination: &Path, staging: tempfile::TempDir) -> Result<PathBuf> {
    // Reserve the destination before moving files to avoid replacing a concurrent
    // writer's directory. The bundle becomes discoverable when its manifest is moved last.
    io(destination, fs::create_dir(destination))?;
    let mut entries = io(staging.path(), fs::read_dir(staging.path()))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| Error::Io {
            path: staging.path().to_path_buf(),
            source: e,
        })?;
    entries.sort_by_key(|entry| {
        if entry.file_name() == RELEASE_FILE {
            2
        } else if entry.file_name() == "pi-plugin.toml" {
            1
        } else {
            0
        }
    });
    for entry in entries {
        let target = destination.join(entry.file_name());
        io(&target, fs::rename(entry.path(), &target))?;
    }
    Ok(destination.to_path_buf())
}
