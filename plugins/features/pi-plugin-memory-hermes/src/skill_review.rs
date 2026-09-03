//! Autonomous skill writes have provenance and read-before-write checks.
use crate::{execution::ReviewObservations, store::HermesMemoryStore};
use pi_core::{ToolContext, ToolError, ToolResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Component, Path};

fn error(message: impl Into<String>) -> ToolResult {
    let details = json!({"success":false,"error":message.into()});
    let mut result = ToolResult::error(details.to_string());
    result.details = Some(details);
    result
}

fn atomic(path: &Path, content: &[u8]) -> Result<(), ToolError> {
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::Execution("Missing skill directory".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    file.write_all(content).map_err(io)?;
    file.as_file().sync_all().map_err(io)?;
    file.persist(path).map_err(|e| io(e.error))?;
    Ok(())
}

fn io(error: std::io::Error) -> ToolError {
    ToolError::Execution(error.to_string())
}
fn hash(path: &Path) -> Result<String, ToolError> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path).map_err(io)?)))
}

pub(crate) fn execute(
    context: &ToolContext,
    store: &HermesMemoryStore,
    review: Option<&ReviewObservations>,
    input: &Value,
    normal: impl FnOnce(&Value) -> Result<ToolResult, ToolError>,
) -> Result<ToolResult, ToolError> {
    let mut input = input.clone();
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let background = review.is_some();
    let id = input
        .get("skill_id")
        .or_else(|| input.get("name"))
        .and_then(Value::as_str);
    let document = id.and_then(|id| {
        store.view_skill(id).ok().or_else(|| {
            store
                .list_skills()
                .ok()?
                .into_iter()
                .find(|skill| skill.name == id)
        })
    });
    if let Some(document) = &document {
        input["skill_id"] = json!(document.id);
    }
    if action == "create" && input.get("scope").is_none() {
        input["scope"] = json!("global");
    }
    // Hermes accepts a complete SKILL.md. Adapt its frontmatter to Pi's owned document.
    if matches!(action.as_str(), "create" | "edit" | "update")
        && let Some(content) = input
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
        && let Some(rest) = content.strip_prefix("---\n")
        && let Some((frontmatter, body)) = rest.split_once("\n---\n")
    {
        for line in frontmatter.lines() {
            if let Some((key, value)) = line.split_once(':')
                && matches!(key.trim(), "name" | "description")
                && input.get(key.trim()).is_none()
            {
                input[key.trim()] = json!(value.trim().trim_matches(['\'', '"']));
            }
        }
        input["content"] = json!(body);
    }
    let writing = !matches!(action.as_str(), "view" | "" | "create");
    let agent_owned = document
        .as_ref()
        .is_some_and(|document| is_agent_owned(&document.path));
    if background && writing {
        let Some(document) = &document else {
            return Ok(error(
                "Skill not found; read an existing skill before modifying it.",
            ));
        };
        if !agent_owned {
            return Ok(error(
                "Autonomous review cannot modify user-owned, externally changed, installed, or pinned skills.",
            ));
        }
        let target = supporting_target(&document.path, &input)?;
        if target.exists() && !has_read(context, review.expect("review state"), document, &target) {
            return Ok(error(
                "Read-before-write required: call skill_view(name, file_path) or read the exact file in this review before changing it.",
            ));
        }
    }
    if let Some(document) = &document {
        let target = supporting_target(&document.path, &input)?;
        match action.as_str() {
            "view" if input.get("file_path").and_then(Value::as_str).is_some() => {
                return Ok(ToolResult::text(fs::read_to_string(target).map_err(io)?));
            }
            "write_file" => {
                if target == document.path {
                    return Ok(error("Use edit or patch for SKILL.md."));
                }
                let content = input.get("content").and_then(Value::as_str).unwrap_or("");
                crate::content_scanner::scan_content(content).map_err(ToolError::Execution)?;
                fs::create_dir_all(target.parent().expect("validated skill target")).map_err(io)?;
                atomic(&target, content.as_bytes())?;
                return Ok(changed("Skill supporting file written"));
            }
            "patch" if input.get("old_string").is_some() => {
                let old = input
                    .get("old_string")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let new = input
                    .get("new_string")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let content = fs::read_to_string(&target).map_err(io)?;
                if old.is_empty() || content.matches(old).count() != 1 {
                    return Ok(error("old_string must match exactly once."));
                }
                let updated = content.replacen(old, new, 1);
                crate::content_scanner::scan_content(&updated).map_err(ToolError::Execution)?;
                if target == document.path {
                    crate::skills::validate_curated_document(&document.name, &updated)
                        .map_err(ToolError::Execution)?;
                }
                atomic(&target, updated.as_bytes())?;
                if agent_owned {
                    record(&document.path)?;
                }
                return Ok(changed("Skill patched"));
            }
            "delete" if background => {
                let absorbed = input
                    .get("absorbed_into")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if absorbed == document.name
                    || absorbed == document.id
                    || !store
                        .list_skills()
                        .map_err(|e| ToolError::Execution(e.to_string()))?
                        .iter()
                        .any(|d| d.name == absorbed || d.id == absorbed)
                {
                    return Ok(error(
                        "Autonomous deletion requires an existing different absorbed_into skill; use verified consolidation.",
                    ));
                }
                let directory = document.path.parent().expect("skill directory");
                let archive = directory
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| ToolError::Execution("Invalid skill root".into()))?
                    .join("skill-archive");
                fs::create_dir_all(&archive).map_err(io)?;
                fs::rename(
                    directory,
                    archive.join(format!("{}-{}", document.name, uuid::Uuid::new_v4())),
                )
                .map_err(io)?;
                return Ok(changed("Skill archived after consolidation"));
            }
            "remove_file" => {
                if target == document.path {
                    return Ok(error("Use delete with absorbed_into to retire a skill."));
                }
                if !target.is_file() {
                    return Ok(error("Supporting file not found."));
                }
                // Archive outside all skill roots; a mistaken removal remains recoverable.
                let directory = document.path.parent().expect("skill directory");
                let archive = directory
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| ToolError::Execution("Invalid skill root".into()))?
                    .join("skill-archive");
                fs::create_dir_all(&archive).map_err(io)?;
                fs::rename(
                    &target,
                    archive.join(format!(
                        "{}-{}-{}",
                        document.name,
                        uuid::Uuid::new_v4(),
                        target.file_name().unwrap().to_string_lossy()
                    )),
                )
                .map_err(io)?;
                return Ok(changed("Skill supporting file archived"));
            }
            _ => {}
        }
    }
    let mut result = normal(&input)?;
    if !result.is_error
        && action != "view"
        && let Some(details) = &mut result.details
    {
        details["_change"] = json!(format!("Skill {action}"));
        if (action == "create" || (agent_owned && action != "delete"))
            && let Some(path) = details.get("path").and_then(Value::as_str)
        {
            record(Path::new(path))?;
        }
    }
    Ok(result)
}

fn supporting_target(path: &Path, input: &Value) -> Result<std::path::PathBuf, ToolError> {
    if path
        .ancestors()
        .take(3)
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    {
        return Err(ToolError::InvalidArguments(
            "Symlinked skill paths are not writable".into(),
        ));
    }
    let Some(relative) = input.get("file_path").and_then(Value::as_str) else {
        return Ok(path.to_path_buf());
    };
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || Path::new(relative)
            .file_name()
            .is_some_and(|name| name == "curator.json")
    {
        return Err(ToolError::InvalidArguments(
            "Invalid supporting-file path".into(),
        ));
    }
    let mut target = path.parent().expect("skill directory").to_path_buf();
    for part in Path::new(relative).components() {
        target.push(part);
        if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(ToolError::InvalidArguments(
                "Symlinked skill paths are not writable".into(),
            ));
        }
    }
    Ok(target)
}

fn has_read(
    context: &ToolContext,
    review: &ReviewObservations,
    document: &crate::skills::SkillDocument,
    target: &Path,
) -> bool {
    review.has_read(|name, args| {
        if matches!(name, "read" | "read_file") {
            return args.get("path").and_then(Value::as_str).is_some_and(|p| {
                context
                    .cwd()
                    .join(p)
                    .canonicalize()
                    .is_ok_and(|read| target.canonicalize().is_ok_and(|target| read == target))
            });
        }
        if name == "skill_view"
            || (name == "skill_manage"
                && args.get("action").and_then(Value::as_str) == Some("view"))
        {
            let id = args
                .get("skill_id")
                .or_else(|| args.get("name"))
                .and_then(Value::as_str);
            return id.is_some_and(|id| id == document.id || id == document.name)
                && supporting_target(&document.path, args).is_ok_and(|path| path == target);
        }
        false
    })
}

fn changed(label: &str) -> ToolResult {
    let details = json!({"success":true,"_change":label});
    let mut result = ToolResult::text(details.to_string());
    result.details = Some(details);
    result
}

fn is_agent_owned(path: &Path) -> bool {
    let provenance: Value = fs::read(path.with_file_name("curator.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    provenance.get("created_by").and_then(Value::as_str) == Some("agent")
        && provenance.get("pinned").and_then(Value::as_bool) != Some(true)
        && hash(path).is_ok_and(|hash| {
            provenance.get("sha256").and_then(Value::as_str) == Some(hash.as_str())
        })
}

fn record(path: &Path) -> Result<(), ToolError> {
    atomic(
        &path.with_file_name("curator.json"),
        json!({"created_by":"agent","pinned":false,"sha256":hash(path)?})
            .to_string()
            .as_bytes(),
    )
}
