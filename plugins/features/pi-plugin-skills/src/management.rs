//! File-backed skill management, separate from immutable runtime generations.
//! Browsing never invokes skill activity observers. Mutations require a freshly
//! discovered path, refuse symlinks, and never fall back to permanent deletion.
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{SkillCatalog, SkillLoaderOptions, absolute, parse_skill_document, skill_roots};

const DOCUMENT_LIMIT: u64 = 2 * 1024 * 1024;
const IMPORT_LIMIT: u64 = 32 * 1024 * 1024;
const FILE_LIMIT: usize = 2048;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedSkill {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub diagnostic: Option<String>,
    pub model_visible: bool,
    pub writable: bool,
    pub delete_path: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDocument {
    pub skill: ManagedSkill,
    pub content: String,
    pub revision: String,
}

/// Uses the same roots, parsing and collision decisions as `SkillsPlugin`.
/// The caller owns its configuration and serializes concurrent UI mutations.
pub struct SkillLibrary {
    options: SkillLoaderOptions,
    destination: Option<PathBuf>,
}

impl SkillLibrary {
    pub fn new(options: SkillLoaderOptions, destination: Option<PathBuf>) -> Self {
        Self {
            options,
            destination: destination.map(|path| absolute(&path)),
        }
    }

    pub fn destination(&self) -> Option<&Path> {
        self.destination.as_deref()
    }

    pub fn list(&self) -> Vec<ManagedSkill> {
        let catalog = SkillCatalog::load(&self.options);
        let roots = skill_roots(
            &absolute(&self.options.cwd),
            &absolute(&self.options.agent_dir),
            &self.options.additional_paths,
            self.options.include_defaults,
            self.options.project_trusted,
        );
        let mut paths = HashSet::new();
        catalog
            .skills()
            .iter()
            .map(|skill| (skill.file_path.clone(), None))
            .chain(
                catalog.diagnostics().iter().map(|diagnostic| {
                    (absolute(&diagnostic.path), Some(diagnostic.message.clone()))
                }),
            )
            .filter(|(path, _)| paths.insert(path.clone()))
            .map(|(path, diagnostic)| {
                let parsed = read_content(&path)
                    .ok()
                    .and_then(|raw| parse_skill_document(&path, &raw, true).ok().flatten());
                let parent = path.parent().unwrap_or(&path);
                // A configured root itself, or an ancestor of another root, is
                // a library rather than a removable package directory.
                let delete_path = if path.file_name().is_some_and(|name| name == "SKILL.md")
                    && !roots.iter().any(|root| absolute(root).starts_with(parent))
                {
                    parent.to_path_buf()
                } else {
                    path.clone()
                };
                ManagedSkill {
                    name: parsed
                        .as_ref()
                        .map(|skill| skill.name.clone())
                        .unwrap_or_else(|| {
                            parent
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        }),
                    description: parsed
                        .as_ref()
                        .map(|skill| skill.description.clone())
                        .unwrap_or_default(),
                    model_visible: parsed.is_some_and(|skill| !skill.disable_model_invocation),
                    writable: check_path(&path).is_ok()
                        && fs::metadata(&path).is_ok_and(|m| !m.permissions().readonly()),
                    path,
                    diagnostic,
                    delete_path,
                }
            })
            .collect()
    }

    pub fn read(&self, path: &Path) -> Result<SkillDocument, String> {
        let skill = self.entry(path)?;
        let content = read_content(&skill.path)?;
        let revision = revision(&content);
        Ok(SkillDocument {
            skill,
            content,
            revision,
        })
    }

    pub fn save(&self, path: &Path, expected_revision: &str, content: &str) -> Result<(), String> {
        let document = self.read(path)?;
        check_path(&document.skill.path)?;
        validate_document(&document.skill.path, content)?;
        if document.revision != expected_revision {
            return Err("Skill changed on disk. Reopen it before saving.".into());
        }
        let parent = document
            .skill
            .path
            .parent()
            .ok_or("Skill has no parent directory")?;
        let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        pending
            .as_file()
            .set_permissions(fs::metadata(path).map_err(error)?.permissions())
            .map_err(error)?;
        pending.write_all(content.as_bytes()).map_err(error)?;
        pending.as_file().sync_all().map_err(error)?;
        check_path(path)?;
        if revision(&read_content(path)?) != expected_revision {
            return Err("Skill changed on disk. Reopen it before saving.".into());
        }
        pending.persist(path).map_err(error)?;
        Ok(())
    }

    pub fn create(&self, directory_name: &str, content: &str) -> Result<PathBuf, String> {
        validate_directory_name(directory_name)?;
        let document = Path::new(directory_name).join("SKILL.md");
        validate_document(&document, content)?;
        let root = self.write_root()?;
        let destination = root.join(directory_name);
        let stage = tempfile::tempdir_in(&root).map_err(error)?;
        fs::write(stage.path().join("SKILL.md"), content).map_err(error)?;
        publish_directory(stage, &destination)?;
        Ok(destination.join("SKILL.md"))
    }

    /// Imports a complete package or one standalone Markdown file. Links and
    /// special files are rejected, never followed into external directories.
    pub fn import(&self, source: &Path) -> Result<PathBuf, String> {
        check_path(source)?;
        let directory = source.is_dir();
        let document_path = if directory {
            source.join("SKILL.md")
        } else {
            source.to_path_buf()
        };
        check_path(&document_path)?;
        let content = read_content(&document_path)?;
        let parsed = validate_document(&document_path, &content)?;
        validate_directory_name(&parsed.name)?;
        let root = self.write_root()?;
        let destination = root.join(&parsed.name);
        if directory
            && fs::canonicalize(&root)
                .map_err(error)?
                .starts_with(fs::canonicalize(source).map_err(error)?)
        {
            return Err("Cannot import a directory into itself".into());
        }
        let stage = tempfile::tempdir_in(&root).map_err(error)?;
        if directory {
            let mut budget = (0, 0);
            copy_package(source, stage.path(), &mut budget)?;
        } else {
            fs::write(stage.path().join("SKILL.md"), content).map_err(error)?;
        }
        validate_document(
            &stage.path().join("SKILL.md"),
            &read_content(&stage.path().join("SKILL.md"))?,
        )?;
        publish_directory(stage, &destination)?;
        Ok(destination.join("SKILL.md"))
    }

    pub fn trash(&self, path: &Path, expected_revision: &str) -> Result<(), String> {
        self.trash_with(path, expected_revision, |path| {
            trash::delete(path).map_err(error)
        })
    }

    fn trash_with(
        &self,
        path: &Path,
        expected_revision: &str,
        remove: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let document = self.read(path)?;
        check_path(&document.skill.path)?;
        if document.revision != expected_revision {
            return Err("Skill changed on disk. Reopen it before deleting.".into());
        }
        let mut budget = (0, 0);
        check_tree(&document.skill.delete_path, &mut budget)?;
        if revision(&read_content(path)?) != expected_revision {
            return Err("Skill changed on disk. Reopen it before deleting.".into());
        }
        remove(&document.skill.delete_path)
    }

    fn entry(&self, path: &Path) -> Result<ManagedSkill, String> {
        if !path.is_absolute() {
            return Err("Expected an absolute catalog skill path".into());
        }
        self.list()
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| "Skill is no longer in the selected library".into())
    }

    fn write_root(&self) -> Result<PathBuf, String> {
        let root = self
            .destination
            .as_ref()
            .ok_or("Project skills require project trust")?;
        check_path(root)?;
        // Never turn an existing package into a library by adding nested skills.
        if root.join("SKILL.md").exists() {
            return Err("Destination is a skill package, not a library".into());
        }
        fs::create_dir_all(root).map_err(error)?;
        check_path(root)?;
        Ok(root.clone())
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn revision(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn read_content(path: &Path) -> Result<String, String> {
    if !fs::metadata(path).map_err(error)?.is_file() {
        return Err("Skill document must be a regular file".into());
    }
    let file = fs::File::open(path).map_err(error)?;
    let mut bytes = Vec::new();
    file.take(DOCUMENT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > DOCUMENT_LIMIT {
        return Err("Skill document exceeds 2 MiB".into());
    }
    String::from_utf8(bytes).map_err(error)
}

fn validate_document(path: &Path, content: &str) -> Result<crate::SkillInfo, String> {
    if content.len() as u64 > DOCUMENT_LIMIT {
        return Err("Skill document exceeds 2 MiB".into());
    }
    let skill = parse_skill_document(path, content, true)?.ok_or("Missing skill metadata")?;
    if skill.name.trim().is_empty() {
        return Err("Skill name must not be empty".into());
    }
    Ok(skill)
}

fn validate_directory_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
    {
        return Err(
            "Directory name must be 1–64 lowercase letters, digits or single hyphens".into(),
        );
    }
    Ok(())
}

fn check_path(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("Parent traversal is not allowed".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    "Symlink skills are read-only; manage the link outside Settings".into(),
                );
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(e)),
        }
    }
    Ok(())
}

fn check_tree(path: &Path, budget: &mut (usize, u64)) -> Result<(), String> {
    check_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(error)?;
    budget.0 += 1;
    budget.1 += metadata.len();
    if budget.0 > FILE_LIMIT || budget.1 > IMPORT_LIMIT {
        return Err("Skill package exceeds 2048 entries or 32 MiB".into());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(error)? {
            check_tree(&entry.map_err(error)?.path(), budget)?;
        }
    } else if !metadata.is_file() {
        return Err("Skill packages may only contain regular files and directories".into());
    }
    Ok(())
}

fn copy_package(source: &Path, target: &Path, budget: &mut (usize, u64)) -> Result<(), String> {
    // Validate the complete tree before creating any copied contents.
    check_tree(source, budget)?;
    fn copy(source: &Path, target: &Path) -> Result<(), String> {
        for entry in fs::read_dir(source).map_err(error)? {
            let entry = entry.map_err(error)?;
            check_path(&entry.path())?;
            let dest = target.join(entry.file_name());
            if entry.file_type().map_err(error)?.is_dir() {
                fs::create_dir(&dest).map_err(error)?;
                copy(&entry.path(), &dest)?;
            } else {
                fs::copy(entry.path(), dest).map_err(error)?;
            }
        }
        Ok(())
    }
    copy(source, target)
}

fn publish_directory(stage: tempfile::TempDir, destination: &Path) -> Result<(), String> {
    // Atomically reserve the name; an existing destination is never overwritten.
    fs::create_dir(destination).map_err(error)?;
    if let Err(e) = fs::rename(stage.path(), destination) {
        let _ = fs::remove_dir(destination);
        return Err(error(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, SkillLibrary) {
        let dir = tempfile::tempdir().unwrap();
        // macOS /var is a system symlink; use its canonical form in tests.
        let root = fs::canonicalize(dir.path()).unwrap();
        let options = SkillLoaderOptions::new(&root, root.join("agent"));
        let library = SkillLibrary::new(options, Some(root.join("agent/skills")));
        (dir, library)
    }
    const RAW: &str =
        "---\nname: sample\ndescription: Test skill\ncustom-field: retained\n---\nHello\n";
    #[test]
    fn crud_preserves_raw_content_and_conflicts_never_overwrite() {
        let (_dir, library) = fixture();
        let path = library.create("sample", RAW).unwrap();
        assert!(library.create("sample", RAW).is_err());
        let doc = library.read(&path).unwrap();
        assert_eq!(doc.content, RAW);
        library
            .save(&path, &doc.revision, &RAW.replace("Hello", "Updated"))
            .unwrap();
        assert!(library.save(&path, &doc.revision, RAW).is_err());
        assert!(
            library
                .trash_with(&path, &doc.revision, |_| panic!("must not delete"))
                .is_err()
        );
        let doc = library.read(&path).unwrap();
        assert!(doc.content.contains("custom-field: retained"));
        assert!(
            library
                .trash_with(&path, &doc.revision, |_| Err("trash unavailable".into()))
                .is_err()
        );
        assert!(path.exists());
        library
            .trash_with(&path, &doc.revision, |target| {
                assert_eq!(target, path.parent().unwrap());
                fs::remove_dir_all(target).map_err(error)
            })
            .unwrap();
        assert!(library.list().is_empty());
    }
    #[test]
    fn rejects_invalid_documents_and_paths_and_imports_references() {
        let (dir, library) = fixture();
        assert!(library.create("../escape", RAW).is_err());
        assert!(
            library
                .create("invalid", "---\nname: invalid\n---")
                .is_err()
        );
        let source = fs::canonicalize(dir.path()).unwrap().join("source");
        fs::create_dir_all(source.join("references")).unwrap();
        fs::write(source.join("SKILL.md"), RAW).unwrap();
        fs::write(source.join("references/a.md"), "reference").unwrap();
        assert!(library.read(&source.join("SKILL.md")).is_err());
        let path = library.import(&source).unwrap();
        assert_eq!(
            fs::read_to_string(path.parent().unwrap().join("references/a.md")).unwrap(),
            "reference"
        );
        assert!(library.import(&source).is_err());
    }
    #[test]
    fn invalid_and_collision_entries_remain_visible_for_repair() {
        let (_dir, library) = fixture();
        let path = library.create("sample", RAW).unwrap();
        library.create("duplicate", RAW).unwrap();
        assert!(library.list().iter().any(|row| row.diagnostic.is_some()));
        fs::write(&path, "---\ndescription: [\n---").unwrap();
        let doc = library.read(&path).unwrap();
        assert!(doc.skill.diagnostic.is_some());
        library.save(&path, &doc.revision, RAW).unwrap();
    }
    #[test]
    fn configured_library_root_and_standalone_documents_never_delete_the_library() {
        let (dir, _) = fixture();
        let root = fs::canonicalize(dir.path()).unwrap();
        let package = root.join("package");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("SKILL.md"), RAW).unwrap();
        fs::write(package.join("keep.txt"), "keep").unwrap();
        let mut options = SkillLoaderOptions::new(&root, root.join("agent"));
        options.include_defaults = false;
        options.additional_paths.push(package.clone());
        let library = SkillLibrary::new(options, None);
        let doc = library.read(&package.join("SKILL.md")).unwrap();
        assert_eq!(doc.skill.delete_path, package.join("SKILL.md"));
        library
            .trash_with(&doc.skill.path, &doc.revision, |path| {
                fs::remove_file(path).map_err(error)
            })
            .unwrap();
        assert!(package.join("keep.txt").exists());
        fs::write(package.join("single.md"), RAW).unwrap();
        let doc = library.read(&package.join("single.md")).unwrap();
        assert_eq!(doc.skill.delete_path, package.join("single.md"));
    }

    #[test]
    fn browsing_does_not_touch_curator_or_source_files() {
        let (_dir, library) = fixture();
        let path = library.create("sample", RAW).unwrap();
        let metadata = path.parent().unwrap().join("curator.json");
        fs::write(&metadata, "{\"use_count\":5}").unwrap();
        library.list();
        library.read(&path).unwrap();
        assert_eq!(fs::read_to_string(metadata).unwrap(), "{\"use_count\":5}");
        assert_eq!(fs::read_to_string(path).unwrap(), RAW);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_mutations_and_import_are_rejected() {
        let (dir, library) = fixture();
        let path = library.create("sample", RAW).unwrap();
        let link = fs::canonicalize(dir.path()).unwrap().join("linked");
        std::os::unix::fs::symlink(path.parent().unwrap(), &link).unwrap();
        assert!(library.import(&link).is_err());
        let doc = library.read(&path).unwrap();
        std::os::unix::fs::symlink(path.parent().unwrap(), path.parent().unwrap().join("cycle"))
            .unwrap();
        assert!(
            library
                .trash_with(&path, &doc.revision, |_| panic!("must not delete"))
                .is_err()
        );
    }
}
