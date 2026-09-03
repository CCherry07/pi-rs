//! Hermes-managed procedural skills, persisted as Pi-native `SKILL.md` files.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;
use serde::Deserialize;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::content_scanner::scan_content;

const MIGRATION_SENTINEL: &str = ".skills-migrated-to-extension-storage";
const SIMILAR_NAME_THRESHOLD: f64 = 0.7;
const SIMILAR_DESCRIPTION_THRESHOLD: f64 = 0.75;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SkillScope {
    Global,
    Project,
}

impl SkillScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SkillCreate {
    pub(crate) scope: SkillScope,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) body: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SkillUpdate {
    pub(crate) skill_id: String,
    pub(crate) description: Option<String>,
    pub(crate) body: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SkillDocument {
    pub(crate) id: String,
    pub(crate) scope: SkillScope,
    pub(crate) file_name: String,
    pub(crate) path: PathBuf,
    pub(crate) project_name: Option<String>,
    pub(crate) name: String,
    pub(crate) display_name: Option<String>,
    pub(crate) description: String,
    pub(crate) version: u64,
    pub(crate) created: String,
    pub(crate) updated: String,
    pub(crate) body: String,
}

#[derive(Debug, Error)]
pub(crate) enum SkillError {
    #[error("Skill name is required.")]
    NameRequired,
    #[error("Skill description is required.")]
    DescriptionRequired,
    #[error("Skill body is required.")]
    BodyRequired,
    #[error("Skill name produces empty slug.")]
    EmptySlug,
    #[error("Project skills require an active project.")]
    ProjectUnavailable,
    #[error("Skill '{slug}' already exists ({skill_id}). Use 'patch' or 'update' to update it.")]
    Duplicate { slug: String, skill_id: String },
    #[error(
        "A similar global skill already exists ({first}). Enhance the existing skill with new learnings/failures using 'patch' or 'update' instead of creating a duplicate."
    )]
    Similar { first: String, ids: Vec<String> },
    #[error(
        "A near-name global skill already exists ({first}) but with different intent. Use a clearer differentiated name for the new skill, or patch/update the existing skill if the intent is actually the same."
    )]
    NameCollision { first: String, ids: Vec<String> },
    #[error(
        "Pi already loads a global skill named '{slug}' from {shadow}. Pi keys skills by name and loads its own root first, so a skill written to {destination} would never be the copy in effect. Choose a different name, or edit {shadow} directly."
    )]
    ShadowsPiSkill {
        slug: String,
        shadow: String,
        destination: String,
    },
    #[error("Skill '{0}' is invalid.")]
    InvalidId(String),
    #[error("Skill '{0}' not found.")]
    NotFound(String),
    #[error("At least one of description or body is required.")]
    EmptyUpdate,
    #[error("Cannot move '{label}' to {scope}: {skill_id} already exists.")]
    ScopeConflict {
        label: String,
        scope: &'static str,
        skill_id: String,
    },
    #[error("{0}")]
    UnsafeText(String),
    #[error("skill storage error: {0}")]
    Io(#[from] std::io::Error),
}

impl SkillError {
    pub(crate) fn conflict_details(&self) -> Option<(&'static str, Vec<String>, &'static str)> {
        match self {
            Self::Duplicate { skill_id, .. } => {
                Some(("duplicate", vec![skill_id.clone()], "patch"))
            }
            Self::Similar { ids, .. } => Some(("similar", ids.clone(), "patch")),
            Self::NameCollision { ids, .. } => Some(("name-collision", ids.clone(), "rename")),
            Self::ScopeConflict { skill_id, .. } => {
                Some(("scope-conflict", vec![skill_id.clone()], "rename"))
            }
            Self::ShadowsPiSkill { .. } => Some(("name-collision", Vec::new(), "rename")),
            _ => None,
        }
    }
}

pub(crate) struct SkillRoots<'a> {
    pub(crate) global_root: &'a Path,
    pub(crate) project_root: Option<&'a Path>,
    pub(crate) pi_global_root: &'a Path,
    pub(crate) project_key: Option<&'a str>,
}

impl SkillRoots<'_> {
    fn root(&self, scope: SkillScope) -> Result<&Path, SkillError> {
        match scope {
            SkillScope::Global => Ok(self.global_root),
            SkillScope::Project => self.project_root.ok_or(SkillError::ProjectUnavailable),
        }
    }

    fn root_and_id(&self, scope: SkillScope, slug: &str) -> Result<(&Path, String), SkillError> {
        let root = self.root(scope)?;
        let id = match scope {
            SkillScope::Global => format!("global:{slug}"),
            SkillScope::Project => format!(
                "project:{}:{slug}",
                self.project_key.ok_or(SkillError::ProjectUnavailable)?
            ),
        };
        Ok((root, id))
    }

    fn path_for_id(&self, skill_id: &str) -> Result<(SkillScope, PathBuf), SkillError> {
        let (scope, slug) = if let Some(slug) = skill_id.strip_prefix("global:") {
            (SkillScope::Global, slug)
        } else if let Some(rest) = skill_id.strip_prefix("project:") {
            let Some((project, slug)) = rest.split_once(':') else {
                return Err(SkillError::InvalidId(skill_id.to_string()));
            };
            if Some(project) != self.project_key {
                return Err(SkillError::InvalidId(skill_id.to_string()));
            }
            (SkillScope::Project, slug)
        } else {
            return Err(SkillError::InvalidId(skill_id.to_string()));
        };
        if !is_slug(slug) {
            return Err(SkillError::InvalidId(skill_id.to_string()));
        }
        Ok((scope, self.root(scope)?.join(slug).join("SKILL.md")))
    }
}

pub(crate) fn ensure_and_migrate(
    roots: &SkillRoots<'_>,
    legacy_skills_dir: &Path,
    migration_root: &Path,
) -> Result<(), SkillError> {
    fs::create_dir_all(roots.global_root)?;
    if let Some(project_root) = roots.project_root {
        fs::create_dir_all(project_root)?;
    }
    let _flat_warnings = normalize_flat_global_skills(roots)?;
    let sentinel = migration_root.join(MIGRATION_SENTINEL);
    if sentinel.exists() {
        return Ok(());
    }
    fs::create_dir_all(migration_root)?;
    let legacy_warnings = migrate_legacy_markdown(roots, legacy_skills_dir)?;
    if legacy_warnings == 0 {
        write_atomic(&sentinel, &format!("{}\n", Utc::now().to_rfc3339()))?;
    }
    Ok(())
}

pub(crate) fn create(
    roots: &SkillRoots<'_>,
    input: SkillCreate,
) -> Result<SkillDocument, SkillError> {
    let display_name = input.name.trim().to_string();
    let description = input.description.trim().to_string();
    let body = input.body.trim().to_string();
    if display_name.is_empty() {
        return Err(SkillError::NameRequired);
    }
    if description.is_empty() {
        return Err(SkillError::DescriptionRequired);
    }
    if body.is_empty() {
        return Err(SkillError::BodyRequired);
    }
    scan_content(&format!("{display_name} {description} {body}"))
        .map_err(SkillError::UnsafeText)?;
    let slug = slugify(&display_name).ok_or(SkillError::EmptySlug)?;
    let (root, skill_id) = roots.root_and_id(input.scope, &slug)?;
    with_root_lock(root, || {
        let path = root.join(&slug).join("SKILL.md");
        if path.exists() {
            return Err(SkillError::Duplicate { slug, skill_id });
        }
        if input.scope == SkillScope::Global {
            let (similar, collisions) = global_conflicts(roots, &slug, &description)?;
            if let Some(first) = similar.first() {
                return Err(SkillError::Similar {
                    first: first.clone(),
                    ids: similar,
                });
            }
            if let Some(first) = collisions.first() {
                return Err(SkillError::NameCollision {
                    first: first.clone(),
                    ids: collisions,
                });
            }
            let shadow = roots.pi_global_root.join(&slug).join("SKILL.md");
            if roots.pi_global_root != roots.global_root && shadow.exists() {
                return Err(SkillError::ShadowsPiSkill {
                    slug,
                    shadow: shadow.display().to_string(),
                    destination: path.display().to_string(),
                });
            }
        }
        fs::create_dir_all(path.parent().expect("skill path has a parent"))?;
        let stamp = today();
        write_atomic(
            &path,
            &format_document(
                &slug,
                Some(&display_name),
                &description,
                1,
                &stamp,
                &stamp,
                &body,
            ),
        )?;
        read_path(roots, input.scope, &path).ok_or(SkillError::NotFound(skill_id))
    })
}

pub(crate) fn list(roots: &SkillRoots<'_>) -> Result<Vec<SkillDocument>, SkillError> {
    let mut documents = Vec::new();
    scan_scope(
        roots,
        SkillScope::Global,
        roots.global_root,
        true,
        &mut documents,
    )?;
    if let Some(project_root) = roots.project_root {
        scan_scope(
            roots,
            SkillScope::Project,
            project_root,
            false,
            &mut documents,
        )?;
    }
    let mut seen = HashSet::new();
    documents.retain(|document| seen.insert(document.id.clone()));
    documents.sort_by(|left, right| {
        right
            .updated
            .cmp(&left.updated)
            .then_with(|| right.created.cmp(&left.created))
            .then_with(|| left.scope.as_str().cmp(right.scope.as_str()))
            .then_with(|| {
                left.display_name
                    .as_deref()
                    .unwrap_or(&left.name)
                    .cmp(right.display_name.as_deref().unwrap_or(&right.name))
            })
    });
    Ok(documents)
}

pub(crate) fn view(roots: &SkillRoots<'_>, skill_id: &str) -> Result<SkillDocument, SkillError> {
    let (scope, path) = roots.path_for_id(skill_id)?;
    read_path(roots, scope, &path).ok_or_else(|| SkillError::NotFound(skill_id.to_string()))
}

pub(crate) fn update(
    roots: &SkillRoots<'_>,
    input: SkillUpdate,
) -> Result<SkillDocument, SkillError> {
    let description = input
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    let body = input.body.as_deref().map(str::trim).unwrap_or_default();
    if description.is_empty() && body.is_empty() {
        return Err(SkillError::EmptyUpdate);
    }
    let (scope, path) = roots.path_for_id(&input.skill_id)?;
    with_root_lock(roots.root(scope)?, || {
        let existing = view(roots, &input.skill_id)?;
        let next_description = if description.is_empty() {
            existing.description.clone()
        } else {
            description.to_string()
        };
        let next_body = if body.is_empty() {
            existing.body.clone()
        } else {
            body.to_string()
        };
        scan_content(&format!("{next_description} {next_body}")).map_err(SkillError::UnsafeText)?;
        write_atomic(
            &path,
            &format_document(
                &existing.name,
                existing.display_name.as_deref(),
                &next_description,
                existing.version.saturating_add(1),
                &existing.created,
                &today(),
                &next_body,
            ),
        )?;
        read_path(roots, scope, &path).ok_or(SkillError::NotFound(input.skill_id))
    })
}

pub(crate) fn patch(
    roots: &SkillRoots<'_>,
    skill_id: &str,
    section: &str,
    new_content: &str,
) -> Result<SkillDocument, SkillError> {
    let section = normalize_section(section);
    let content = normalize_patch_content(&section, new_content)?;
    scan_content(&content).map_err(SkillError::UnsafeText)?;
    let (scope, path) = roots.path_for_id(skill_id)?;
    with_root_lock(roots.root(scope)?, || {
        let existing = view(roots, skill_id)?;
        let body = patch_body(&existing.body, &section, &content);
        write_atomic(
            &path,
            &format_document(
                &existing.name,
                existing.display_name.as_deref(),
                &existing.description,
                existing.version.saturating_add(1),
                &existing.created,
                &today(),
                &body,
            ),
        )?;
        read_path(roots, scope, &path).ok_or_else(|| SkillError::NotFound(skill_id.to_string()))
    })
}

pub(crate) fn delete(roots: &SkillRoots<'_>, skill_id: &str) -> Result<SkillDocument, SkillError> {
    let (scope, path) = roots.path_for_id(skill_id)?;
    with_root_lock(roots.root(scope)?, || {
        let existing = view(roots, skill_id)?;
        fs::remove_file(&path)?;
        remove_empty_parents(
            path.parent().expect("skill path has parent"),
            roots.root(scope)?,
        );
        Ok(existing)
    })
}

pub(crate) fn move_to_scope(
    roots: &SkillRoots<'_>,
    skill_id: &str,
    target_scope: SkillScope,
) -> Result<SkillDocument, SkillError> {
    let (source_scope, source_path) = roots.path_for_id(skill_id)?;
    let existing = view(roots, skill_id)?;
    if source_scope == target_scope {
        return Ok(existing);
    }

    let slug = existing.name.clone();
    let (target_root, target_id) = roots.root_and_id(target_scope, &slug)?;
    let source_root = roots.root(source_scope)?;
    let target_path = target_root.join(&slug).join("SKILL.md");
    let label = existing
        .display_name
        .as_deref()
        .unwrap_or(&existing.name)
        .to_string();

    with_two_root_locks(source_root, target_root, || {
        // Re-read under the locks so a concurrent delete/update cannot turn
        // this into a duplicate or a phantom success.
        let existing = read_path(roots, source_scope, &source_path)
            .ok_or_else(|| SkillError::NotFound(skill_id.to_string()))?;
        if target_path.exists() {
            return Err(SkillError::ScopeConflict {
                label,
                scope: target_scope.as_str(),
                skill_id: target_id,
            });
        }
        if target_scope == SkillScope::Global {
            let (similar, collisions) = global_conflicts(roots, &slug, &existing.description)?;
            if let Some(first) = similar.first() {
                return Err(SkillError::Similar {
                    first: first.clone(),
                    ids: similar,
                });
            }
            if let Some(first) = collisions.first() {
                return Err(SkillError::NameCollision {
                    first: first.clone(),
                    ids: collisions,
                });
            }
        }

        fs::create_dir_all(target_path.parent().expect("skill path has a parent"))?;
        match fs::rename(&source_path, &target_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                write_atomic(
                    &target_path,
                    &format_document(
                        &slug,
                        existing.display_name.as_deref(),
                        &existing.description,
                        existing.version,
                        &existing.created,
                        &existing.updated,
                        &existing.body,
                    ),
                )?;
                if let Err(remove_error) = fs::remove_file(&source_path) {
                    let rollback = fs::remove_file(&target_path);
                    if rollback.is_ok() {
                        remove_empty_parents(
                            target_path.parent().expect("skill path has a parent"),
                            target_root,
                        );
                    }
                    return Err(SkillError::Io(std::io::Error::other(
                        if rollback.is_err() {
                            format!(
                                "Move to {} failed while removing source skill '{skill_id}', and rollback also failed. Source path: {}. Destination path: {}. Error: {remove_error}",
                                target_scope.as_str(),
                                source_path.display(),
                                target_path.display()
                            )
                        } else {
                            format!(
                                "Move to {} failed while removing source skill '{skill_id}'. Rolled back destination copy. Source path: {}. Destination path: {}. Error: {remove_error}",
                                target_scope.as_str(),
                                source_path.display(),
                                target_path.display()
                            )
                        },
                    )));
                }
            }
            Err(error) => {
                return Err(SkillError::Io(std::io::Error::other(format!(
                    "Move to {} failed before copy for skill '{skill_id}'. Source path: {}. Destination path: {}. Error: {error}",
                    target_scope.as_str(),
                    source_path.display(),
                    target_path.display()
                ))));
            }
        }
        remove_empty_parents(
            source_path.parent().expect("skill path has a parent"),
            source_root,
        );
        read_path(roots, target_scope, &target_path).ok_or(SkillError::NotFound(target_id))
    })
}

fn scan_scope(
    roots: &SkillRoots<'_>,
    scope: SkillScope,
    root: &Path,
    allow_root_markdown: bool,
    documents: &mut Vec<SkillDocument>,
) -> Result<(), SkillError> {
    if !root.exists() {
        return Ok(());
    }
    scan_directory(roots, scope, root, root, allow_root_markdown, documents)
}

fn scan_directory(
    roots: &SkillRoots<'_>,
    scope: SkillScope,
    root: &Path,
    directory: &Path,
    allow_root_markdown: bool,
    documents: &mut Vec<SkillDocument>,
) -> Result<(), SkillError> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in &entries {
        if !entry.file_type()?.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let child = entry.path();
        let skill_path = child.join("SKILL.md");
        if skill_path.is_file()
            && let Some(document) = read_path(roots, scope, &skill_path)
        {
            documents.push(document);
        }
        scan_directory(roots, scope, root, &child, false, documents)?;
    }
    if allow_root_markdown && directory == root {
        for entry in entries {
            let path = entry.path();
            if entry.file_type()?.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("md")
                && path.file_name().and_then(|value| value.to_str()) != Some("SKILL.md")
                && let Some(document) = read_path(roots, scope, &path)
            {
                documents.push(document);
            }
        }
    }
    Ok(())
}

fn read_path(roots: &SkillRoots<'_>, scope: SkillScope, path: &Path) -> Option<SkillDocument> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed = parse_frontmatter(&raw);
    let fallback = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())?;
    let name = parsed
        .metadata
        .get("name")
        .map(String::as_str)
        .unwrap_or(fallback)
        .trim()
        .to_string();
    let slug = slugify(&name)?;
    let (_, id) = roots.root_and_id(scope, &slug).ok()?;
    Some(SkillDocument {
        id,
        scope,
        file_name: path.file_name()?.to_string_lossy().to_string(),
        path: path.to_path_buf(),
        project_name: (scope == SkillScope::Project)
            .then(|| roots.project_key.map(str::to_string))
            .flatten(),
        name,
        display_name: parsed.metadata.get("display_name").cloned(),
        description: parsed
            .metadata
            .get("description")
            .cloned()
            .unwrap_or_default(),
        version: parsed
            .metadata
            .get("version")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
        created: parsed
            .metadata
            .get("created")
            .cloned()
            .unwrap_or_else(today),
        updated: parsed
            .metadata
            .get("updated")
            .cloned()
            .unwrap_or_else(today),
        body: parsed.body,
    })
}

struct ParsedSkill {
    metadata: std::collections::HashMap<String, String>,
    body: String,
}

pub(crate) fn validate_curated_document(name: &str, raw: &str) -> Result<(), String> {
    let parsed = parse_frontmatter(raw);
    if parsed.metadata.get("name").map(String::as_str) != Some(name)
        || parsed
            .metadata
            .get("description")
            .is_none_or(|text| text.trim().is_empty())
        || parsed.body.trim().is_empty()
    {
        return Err(
            "Patch must preserve the skill name, description, and nonempty Markdown body.".into(),
        );
    }
    Ok(())
}

fn parse_frontmatter(raw: &str) -> ParsedSkill {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return ParsedSkill {
            metadata: Default::default(),
            body: raw.trim().to_string(),
        };
    };
    let Some((header, body)) = rest.split_once("\n---\n") else {
        return ParsedSkill {
            metadata: Default::default(),
            body: raw.trim().to_string(),
        };
    };
    let metadata = header
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            (!key.is_empty()).then(|| (key.to_string(), parse_scalar(value.trim())))
        })
        .collect();
    ParsedSkill {
        metadata,
        body: body.trim().to_string(),
    }
}

fn parse_scalar(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        serde_json::from_str(value).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}

fn format_document(
    name: &str,
    display_name: Option<&str>,
    description: &str,
    version: u64,
    created: &str,
    updated: &str,
    body: &str,
) -> String {
    let mut lines = vec![
        "---".to_string(),
        format!("name: {}", json_string(name)),
        format!("description: {}", json_string(description)),
        format!("version: {version}"),
        format!("created: {}", json_string(created)),
        format!("updated: {}", json_string(updated)),
    ];
    if let Some(display_name) = display_name.map(str::trim)
        && !display_name.is_empty()
        && display_name != name
    {
        lines.push(format!("display_name: {}", json_string(display_name)));
    }
    lines.extend(["---".to_string(), body.to_string()]);
    lines.join("\n")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a Rust string always serializes as JSON")
}

fn normalize_section(section: &str) -> String {
    section.trim_start_matches('#').trim().to_string()
}

fn normalize_patch_content(section: &str, raw: &str) -> Result<String, SkillError> {
    if section.is_empty() {
        return Err(SkillError::UnsafeText(
            "section is required for patch.".to_string(),
        ));
    }
    let mut content = raw.trim().to_string();
    if content.is_empty() {
        return Err(SkillError::UnsafeText(
            "New content is required for patch. Prefer structured fields (procedure_steps, pitfalls, verification_steps, when_to_use) over free-form content.".to_string(),
        ));
    }
    if content.starts_with('{') && content.ends_with('}') {
        return Err(SkillError::UnsafeText(
            "Patch content looks like a JSON object. Provide Markdown section body or a string array via structured fields.".to_string(),
        ));
    }
    if content.starts_with('[') && content.ends_with(']') {
        let values = serde_json::from_str::<Vec<String>>(&content).map_err(|_| {
            SkillError::UnsafeText(
                "Patch content looks like a JSON array but could not be parsed. Use Markdown or structured string[] fields.".to_string(),
            )
        })?;
        let values = values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Err(SkillError::UnsafeText(
                "Patch content JSON array must contain non-empty strings.".to_string(),
            ));
        }
        let key = section.to_ascii_lowercase();
        content = if key == "when to use" {
            values.join("\n\n")
        } else if key == "pitfalls" {
            values
                .into_iter()
                .map(|item| format!("- {}", trim_list_prefix(&item)))
                .collect::<Vec<_>>()
                .join("\n")
        } else if matches!(key.as_str(), "procedure" | "verification") {
            values
                .into_iter()
                .enumerate()
                .map(|(index, item)| format!("{}. {}", index + 1, trim_list_prefix(&item)))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            values
                .into_iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
    }
    if content.lines().any(|line| {
        let trimmed = line.trim_start();
        let hashes = trimmed
            .chars()
            .take_while(|character| *character == '#')
            .count();
        (1..=6).contains(&hashes) && trimmed.chars().nth(hashes).is_some_and(char::is_whitespace)
    }) {
        return Err(SkillError::UnsafeText(
            "Patch content must not include Markdown section headers (## ...). Patch only the body of the target section.".to_string(),
        ));
    }
    Ok(content)
}

fn trim_list_prefix(value: &str) -> &str {
    let value = value
        .strip_prefix("- ")
        .or_else(|| value.strip_prefix("* "))
        .unwrap_or(value);
    let digits = value.chars().take_while(char::is_ascii_digit).count();
    value
        .get(digits..)
        .and_then(|rest| rest.strip_prefix(". "))
        .unwrap_or(value)
}

fn patch_body(body: &str, section: &str, content: &str) -> String {
    let lines = body.lines().collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut found = false;
    let mut index = 0;
    while index < lines.len() {
        if is_exact_section(lines[index], section) {
            output.push(format!("## {section}"));
            output.extend(content.lines().map(str::to_string));
            found = true;
            index += 1;
            while index < lines.len() && !lines[index].trim().starts_with("## ") {
                index += 1;
            }
            continue;
        }
        output.push(lines[index].to_string());
        index += 1;
    }
    if !found {
        if output.last().is_some_and(|line| !line.is_empty()) {
            output.push(String::new());
        }
        output.push(format!("## {section}"));
        output.extend(content.lines().map(str::to_string));
    }
    output.join("\n").trim().to_string()
}

fn is_exact_section(line: &str, section: &str) -> bool {
    line.trim()
        .strip_prefix("## ")
        .is_some_and(|heading| heading.trim().eq_ignore_ascii_case(section))
}

fn global_conflicts(
    roots: &SkillRoots<'_>,
    candidate_slug: &str,
    candidate_description: &str,
) -> Result<(Vec<String>, Vec<String>), SkillError> {
    let candidate_name = tokens(&candidate_slug.replace('-', " "));
    let candidate_description = tokens(candidate_description);
    let mut scored = list(roots)?
        .into_iter()
        .filter(|skill| skill.scope == SkillScope::Global)
        .map(|skill| {
            let name = tokens(
                &skill
                    .display_name
                    .as_deref()
                    .unwrap_or(&skill.name)
                    .replace('-', " "),
            );
            let description = tokens(&skill.description);
            (
                skill.id,
                jaccard(&candidate_name, &name),
                jaccard(&candidate_description, &description),
            )
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.2.total_cmp(&left.2))
    });
    let similar = scored
        .iter()
        .filter(|(_, name, description)| {
            *name > SIMILAR_NAME_THRESHOLD && *description > SIMILAR_DESCRIPTION_THRESHOLD
        })
        .map(|(id, _, _)| id.clone())
        .collect();
    let collisions = scored
        .into_iter()
        .filter(|(_, name, description)| {
            *name > SIMILAR_NAME_THRESHOLD && *description <= SIMILAR_DESCRIPTION_THRESHOLD
        })
        .map(|(id, _, _)| id)
        .collect();
    Ok((similar, collisions))
}

fn tokens(value: &str) -> HashSet<String> {
    const STOP: &[&str] = &[
        "a",
        "an",
        "and",
        "are",
        "as",
        "at",
        "be",
        "by",
        "for",
        "from",
        "how",
        "in",
        "into",
        "is",
        "it",
        "of",
        "on",
        "or",
        "that",
        "the",
        "this",
        "to",
        "use",
        "using",
        "with",
        "workflow",
        "procedure",
        "step",
        "steps",
        "guide",
        "skill",
        "skills",
        "repo",
        "project",
    ];
    value
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1 && !STOP.contains(token))
        .map(str::to_string)
        .collect()
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    left.intersection(right).count() as f64 / left.union(right).count() as f64
}

fn normalize_flat_global_skills(roots: &SkillRoots<'_>) -> Result<usize, SkillError> {
    let mut files = fs::read_dir(roots.global_root)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().and_then(|value| value.to_str()) == Some("md")
                && entry.file_name() != "SKILL.md"
        })
        .collect::<Vec<_>>();
    files.sort_by_key(std::fs::DirEntry::file_name);
    let mut warnings = 0_usize;
    for entry in files {
        let result = (|| -> Result<(), SkillError> {
            let source = entry.path();
            let raw = fs::read_to_string(&source)?;
            let parsed = parse_frontmatter(&raw);
            let fallback = source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let Some(slug) = slugify(
                parsed
                    .metadata
                    .get("name")
                    .map(String::as_str)
                    .unwrap_or(fallback),
            ) else {
                return Ok(());
            };
            let destination = roots.global_root.join(&slug).join("SKILL.md");
            if destination.exists() {
                fs::remove_file(source)?;
                return Ok(());
            }
            fs::create_dir_all(destination.parent().expect("skill path has parent"))?;
            write_atomic(&destination, &migrated_document(&slug, parsed))?;
            fs::remove_file(source)?;
            Ok(())
        })();
        if result.is_err() {
            warnings = warnings.saturating_add(1);
        }
    }
    Ok(warnings)
}

fn migrate_legacy_markdown(
    roots: &SkillRoots<'_>,
    legacy_skills_dir: &Path,
) -> Result<usize, SkillError> {
    let directory = match fs::read_dir(legacy_skills_dir) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut files = directory.collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(std::fs::DirEntry::file_name);
    let mut warnings = 0_usize;
    for entry in files {
        let source = entry.path();
        if source.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let result = (|| -> Result<(), SkillError> {
            let raw = fs::read_to_string(&source)?;
            let parsed = parse_frontmatter(&raw);
            let fallback = source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let Some(slug) = slugify(
                parsed
                    .metadata
                    .get("name")
                    .map(String::as_str)
                    .unwrap_or(fallback),
            ) else {
                return Ok(());
            };
            let destination = roots.global_root.join(&slug).join("SKILL.md");
            if destination.exists() {
                return Ok(());
            }
            fs::create_dir_all(destination.parent().expect("skill path has parent"))?;
            write_atomic(&destination, &migrated_document(&slug, parsed))
        })();
        if result.is_err() {
            warnings = warnings.saturating_add(1);
        }
    }
    Ok(warnings)
}

fn migrated_document(slug: &str, parsed: ParsedSkill) -> String {
    let display = parsed
        .metadata
        .get("display_name")
        .or_else(|| parsed.metadata.get("name"))
        .map(String::as_str);
    let description = parsed
        .metadata
        .get("description")
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("Migrated legacy skill: {slug}"));
    let version = parsed
        .metadata
        .get("version")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let created = parsed
        .metadata
        .get("created")
        .cloned()
        .unwrap_or_else(today);
    let updated = parsed
        .metadata
        .get("updated")
        .cloned()
        .unwrap_or_else(today);
    let body = if parsed.body.is_empty() {
        format!("# {slug}\n")
    } else {
        parsed.body
    };
    format_document(
        slug,
        display,
        &description,
        version,
        &created,
        &updated,
        &body,
    )
}

fn slugify(value: &str) -> Option<String> {
    let slug = value
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!slug.is_empty()).then(|| slug.chars().take(64).collect())
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn today() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

fn with_root_lock<T>(
    root: &Path,
    action: impl FnOnce() -> Result<T, SkillError>,
) -> Result<T, SkillError> {
    fs::create_dir_all(root)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(root.join(".skill-store.lock"))?;
    lock.lock_exclusive()?;
    let result = action();
    lock.unlock()?;
    result
}

fn with_two_root_locks<T>(
    left: &Path,
    right: &Path,
    action: impl FnOnce() -> Result<T, SkillError>,
) -> Result<T, SkillError> {
    fs::create_dir_all(left)?;
    fs::create_dir_all(right)?;
    let (first_root, second_root) = if left.as_os_str() <= right.as_os_str() {
        (left, right)
    } else {
        (right, left)
    };
    let open = |root: &Path| {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.join(".skill-store.lock"))
    };
    let first = open(first_root)?;
    first.lock_exclusive()?;
    let second = if first_root == second_root {
        None
    } else {
        let second = open(second_root)?;
        second.lock_exclusive()?;
        Some(second)
    };
    let result = action();
    let second_unlock = second.as_ref().map_or(Ok(()), FileExt::unlock);
    let first_unlock = first.unlock();
    match (result, second_unlock, first_unlock) {
        (Err(error), _, _) => Err(error),
        (Ok(_), Err(error), _) | (Ok(_), Ok(()), Err(error)) => Err(error.into()),
        (Ok(value), Ok(()), Ok(())) => Ok(value),
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), SkillError> {
    fs::create_dir_all(path.parent().expect("skill path has parent"))?;
    let mut temporary = NamedTempFile::new_in(path.parent().expect("skill path has parent"))?;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| SkillError::Io(error.error))?;
    Ok(())
}

fn remove_empty_parents(start: &Path, stop: &Path) {
    let mut current = start.to_path_buf();
    while current.starts_with(stop) && current != stop {
        let empty = fs::read_dir(&current)
            .ok()
            .is_some_and(|mut entries| entries.next().is_none());
        if !empty || fs::remove_dir(&current).is_err() {
            return;
        }
        let Some(parent) = current.parent() else {
            return;
        };
        current = parent.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots<'a>(directory: &'a tempfile::TempDir) -> SkillRoots<'a> {
        let agent = directory.path().join("agent");
        let global = Box::leak(Box::new(agent.join("pi-hermes-memory/skills")));
        let project = Box::leak(Box::new(agent.join("projects-memory/demo/skills")));
        let pi = Box::leak(Box::new(agent.join("skills")));
        SkillRoots {
            global_root: global,
            project_root: Some(project),
            pi_global_root: pi,
            project_key: Some("demo"),
        }
    }

    #[test]
    fn preserves_raw_body_and_tracks_upstream_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let roots = roots(&directory);
        let created = create(
            &roots,
            SkillCreate {
                scope: SkillScope::Project,
                name: "Deploy Demo".to_string(),
                description: "Deploy the demo project safely".to_string(),
                body: "# Custom body\n\nAnything valid is preserved.".to_string(),
            },
        )
        .unwrap();
        assert_eq!(created.id, "project:demo:deploy-demo");
        assert_eq!(created.display_name.as_deref(), Some("Deploy Demo"));
        assert_eq!(created.version, 1);
        let updated = patch(&roots, &created.id, "Verification", "1. Check logs").unwrap();
        assert!(updated.body.starts_with("# Custom body"));
        assert!(updated.body.contains("## Verification\n1. Check logs"));
        assert_eq!(updated.version, 2);
    }

    #[test]
    fn project_create_requires_a_real_active_project() {
        let directory = tempfile::tempdir().unwrap();
        let roots = SkillRoots {
            global_root: directory.path(),
            project_root: None,
            pi_global_root: directory.path(),
            project_key: None,
        };
        let error = create(
            &roots,
            SkillCreate {
                scope: SkillScope::Project,
                name: "demo".to_string(),
                description: "demo project flow".to_string(),
                body: "Do the work".to_string(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, SkillError::ProjectUnavailable));
    }

    #[test]
    fn json_array_patch_is_coerced_like_upstream() {
        assert_eq!(
            normalize_patch_content("Procedure", r#"["one", "two"]"#).unwrap(),
            "1. one\n2. two"
        );
    }

    #[test]
    fn moves_between_global_and_project_scopes_and_blocks_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let roots = roots(&directory);
        let created = create(
            &roots,
            SkillCreate {
                scope: SkillScope::Global,
                name: "Move Me".to_string(),
                description: "Reusable process".to_string(),
                body: "## Procedure\n1. Do it".to_string(),
            },
        )
        .unwrap();
        let moved = move_to_scope(&roots, &created.id, SkillScope::Project).unwrap();
        assert_eq!(moved.id, "project:demo:move-me");
        assert!(!roots.global_root.join("move-me/SKILL.md").exists());
        assert!(
            roots
                .project_root
                .unwrap()
                .join("move-me/SKILL.md")
                .exists()
        );

        create(
            &roots,
            SkillCreate {
                scope: SkillScope::Global,
                name: "Move Me".to_string(),
                description: "Global variant".to_string(),
                body: "body".to_string(),
            },
        )
        .unwrap();
        let error = move_to_scope(&roots, &moved.id, SkillScope::Global).unwrap_err();
        assert!(matches!(error, SkillError::ScopeConflict { .. }));
    }
}
