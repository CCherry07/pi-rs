//! Read-only discovery of trusted, locally bundled desktop presentation extensions.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ProjectTrustEvaluation, ProjectTrustService};

const MAX_RESOURCE: u64 = 16 * 1024 * 1024;
const MAX_CATALOG: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    id: String,
    #[serde(default)]
    name: Option<String>,
    entry: PathBuf,
    #[serde(default)]
    styles: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopExtensionSource {
    pub id: String,
    pub revision: String,
    pub javascript: String,
    pub css: String,
    pub project: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopExtensionCatalog {
    pub extensions: Vec<DesktopExtensionSource>,
    pub project_trusted: bool,
    pub error: Option<String>,
}

/// A failure retains the authorization result, so revocation cannot be hidden by
/// an unrelated malformed package. No read here creates a session or grants trust.
pub fn read_catalog(
    agent_dir: &Path,
    cwd: &Path,
    trust: &ProjectTrustService,
) -> DesktopExtensionCatalog {
    let authorization = trust.evaluate_resource_access(cwd);
    let project_trusted = matches!(&authorization, Ok(ProjectTrustEvaluation::Known(true)));
    let result = authorization
        .map_err(|error| error.to_string())
        .and_then(|_| {
            let mut sources = Vec::new();
            read_root(&agent_dir.join("desktop-extensions"), false, &mut sources)?;
            if project_trusted {
                read_root(&cwd.join(".pi/desktop-extensions"), true, &mut sources)?;
            }
            let mut ids = BTreeSet::new();
            let mut size = 0;
            for source in &sources {
                if !ids.insert(&source.id) {
                    return Err(format!("duplicate desktop extension id: {}", source.id));
                }
                size += source.javascript.len() + source.css.len();
                if size > MAX_CATALOG {
                    return Err("desktop extension catalog exceeds 64 MiB".into());
                }
            }
            Ok(sources)
        });
    match result {
        Ok(extensions) => DesktopExtensionCatalog {
            extensions,
            project_trusted,
            error: None,
        },
        Err(error) => DesktopExtensionCatalog {
            extensions: Vec::new(),
            project_trusted,
            error: Some(error),
        },
    }
}

fn read_root(
    root: &Path,
    project: bool,
    sources: &mut Vec<DesktopExtensionSource>,
) -> Result<(), String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    let mut packages = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    packages.sort();
    for package in packages {
        if !package.is_dir() || !package.join("pi-desktop.json").is_file() {
            continue;
        }
        let package = package.canonicalize().map_err(|error| error.to_string())?;
        let manifest_text = read_resource(&package, Path::new("pi-desktop.json"))?;
        let manifest: Manifest = serde_json::from_str(&manifest_text)
            .map_err(|error| format!("{}: {error}", package.display()))?;
        if manifest.schema_version != 1
            || manifest.id.is_empty()
            || manifest.id.len() > 128
            || !manifest
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(format!(
                "{}: invalid desktop manifest version or id",
                package.display()
            ));
        }
        let _ = manifest.name;
        let javascript = read_resource(&package, &manifest.entry)?;
        let mut css = String::new();
        for style in &manifest.styles {
            css.push_str(&read_resource(&package, style)?);
            css.push('\n');
            if css.len() > MAX_RESOURCE as usize {
                return Err("desktop styles exceed 16 MiB".into());
            }
        }
        let mut hash = Sha256::new();
        hash.update(manifest_text.as_bytes());
        hash.update(javascript.as_bytes());
        hash.update(css.as_bytes());
        let previous_size: usize = sources
            .iter()
            .map(|source| source.javascript.len() + source.css.len())
            .sum();
        if sources.len() >= 256 || previous_size + javascript.len() + css.len() > MAX_CATALOG {
            return Err("desktop extension catalog exceeds its package or size limit".into());
        }
        sources.push(DesktopExtensionSource {
            id: manifest.id,
            revision: format!("{:x}", hash.finalize()),
            javascript,
            css,
            project,
        });
    }
    Ok(())
}

fn read_resource(package: &Path, relative: &Path) -> Result<String, String> {
    if relative.is_absolute() {
        return Err("desktop resources must be package-relative".into());
    }
    let path = package
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("{}: {error}", package.join(relative).display()))?;
    if !path.starts_with(package) {
        return Err("desktop resource escapes package".into());
    }
    let file = fs::File::open(&path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("desktop resource is not a file".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(MAX_RESOURCE + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RESOURCE {
        return Err(format!("{} exceeds 16 MiB", path.display()));
    }
    String::from_utf8(bytes).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(root: &Path, id: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("pi-desktop.json"),
            serde_json::json!({"schemaVersion":1,"id":id,"entry":"index.js"}).to_string(),
        )
        .unwrap();
        fs::write(root.join("index.js"), "export default {};").unwrap();
    }

    #[test]
    fn project_discovery_uses_existing_trust_and_atomic_catalog_validation() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let cwd = dir.path().join("project");
        package(&agent.join("desktop-extensions/global"), "example.global");
        package(&cwd.join(".pi/desktop-extensions/local"), "example.local");
        let (trust, _) =
            ProjectTrustService::new(&agent, None, false, pi_settings::DefaultProjectTrust::Ask)
                .unwrap();
        let untrusted = read_catalog(&agent, &cwd, &trust);
        assert!(!untrusted.project_trusted);
        assert_eq!(untrusted.extensions.len(), 1);
        trust.remember(&cwd, true).unwrap();
        let trusted = read_catalog(&agent, &cwd, &trust);
        assert_eq!(trusted.extensions.len(), 2);
        package(
            &cwd.join(".pi/desktop-extensions/collision"),
            "example.global",
        );
        assert!(
            read_catalog(&agent, &cwd, &trust)
                .error
                .unwrap()
                .contains("duplicate")
        );
        trust.remember(&cwd, false).unwrap();
        let revoked = read_catalog(&agent, &cwd, &trust);
        assert!(!revoked.project_trusted);
        assert_eq!(revoked.extensions.len(), 1);
    }

    #[test]
    fn rejects_resource_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("package");
        package(&root, "example");
        fs::write(dir.path().join("outside.js"), "secret").unwrap();
        assert!(read_resource(&root, Path::new("../outside.js")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside.js"), root.join("link.js"))
                .unwrap();
            assert!(read_resource(&root, Path::new("link.js")).is_err());
        }
    }
}
