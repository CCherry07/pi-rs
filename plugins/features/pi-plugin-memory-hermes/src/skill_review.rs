//! Autonomous skill writes have provenance and read-before-write checks.
use crate::curator::metadata::{self, Metadata};
use crate::execution::HermesRunKind;
use crate::{execution::ReviewObservations, store::HermesMemoryStore};
use pi_core::{ToolContext, ToolError, ToolResult};
use serde_json::{Value, json};
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

fn resolve_skill(
    store: &HermesMemoryStore,
    target: Option<&crate::curator::scope::Target>,
    id: &str,
) -> Option<crate::skills::SkillDocument> {
    let matches_scope = |d: &crate::skills::SkillDocument| {
        target.is_none_or(|t| {
            d.scope == t.scope && d.path.parent().and_then(Path::parent) == Some(t.root.as_path())
        })
    };
    store.view_skill(id).ok().filter(matches_scope).or_else(|| {
        store
            .list_skills()
            .ok()?
            .into_iter()
            .find(|d| d.name == id && matches_scope(d))
    })
}

pub(crate) fn execute(
    context: &ToolContext,
    store: &HermesMemoryStore,
    review: Option<&ReviewObservations>,
    kind: HermesRunKind,
    target: Option<&crate::curator::scope::Target>,
    input: &Value,
    normal: impl FnOnce(&Value) -> Result<ToolResult, ToolError>,
) -> Result<ToolResult, ToolError> {
    if matches!(kind, HermesRunKind::Curator { .. }) && target.is_none() {
        return Ok(error("Curator execution has no bound scope"));
    }
    let mut input = input.clone();
    if input.get("action").and_then(Value::as_str) == Some("create") && input.get("scope").is_none()
    {
        input["scope"] = json!(target.map_or("global", |t| t.scope.as_str()));
    }
    let input = &input;
    let action = input.get("action").and_then(Value::as_str).unwrap_or("");
    if kind == (HermesRunKind::Curator { dry_run: true }) && action != "view" {
        return Ok(error("Curator preview is read-only"));
    }
    let resolve = || {
        if action == "create" {
            return None;
        }
        let id = input
            .get("skill_id")
            .or_else(|| input.get("name"))
            .and_then(Value::as_str)?;
        resolve_skill(store, target, id)
    };
    let document = resolve();
    let scope = document.as_ref().map(|d| d.scope).unwrap_or_else(|| {
        if input.get("scope").and_then(Value::as_str) == Some("project") {
            crate::skills::SkillScope::Project
        } else {
            target.map_or(crate::skills::SkillScope::Global, |t| t.scope)
        }
    });
    if let Some(target) = target {
        if input
            .get("scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| scope != target.scope.as_str())
        {
            return Ok(error(
                "Requested scope is outside the Curator's bound scope",
            ));
        }
        if scope != target.scope || store.skill_root(scope).ok().as_ref() != Some(&target.root) {
            return Ok(error(
                "Curator can access only its bound scope in the current trusted checkout",
            ));
        }
        // An explicit ID must never become a bare-name lookup in another scope.
        if input
            .get("skill_id")
            .and_then(Value::as_str)
            .is_some_and(|id| {
                (id.starts_with("global:") || id.starts_with("project:"))
                    && !id.starts_with(&format!("{}:", target.key))
            })
        {
            return Ok(error("Skill ID is outside the Curator's bound scope"));
        }
    }
    let writing = !matches!(action, "view" | "");
    let root = store
        .skill_root(scope)
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let _gate = if writing {
        Some(metadata::content_lock(&root).map_err(io)?)
    } else {
        None
    };
    // Re-resolve under the gate. Every publication rechecks the exact package.
    let document = resolve();
    let before = document.as_ref().and_then(|d| {
        let directory = d.path.parent()?;
        Metadata::read(directory)
            .ok()
            .filter(|m| m.unchanged(directory))
    });
    if action == "delete"
        && document.as_ref().is_some_and(|d| {
            Metadata::read(d.path.parent().expect("skill directory")).is_ok_and(|m| m.pinned)
        })
    {
        return Ok(error(
            "Pinned skills cannot be deleted; unpin explicitly first",
        ));
    }
    if matches!(kind, HermesRunKind::Curator { .. })
        && let Some(d) = &document
        && !is_agent_owned(&d.path)
    {
        return Ok(error(
            "Curator can access only unpinned managed skills with unchanged content",
        ));
    }
    let result = execute_inner(context, store, review, kind, target, input, normal)?;
    if !result.is_error {
        if action == "create" {
            if let Some(path) = result
                .details
                .as_ref()
                .and_then(|v| v.get("path"))
                .and_then(Value::as_str)
            {
                let directory = Path::new(path).parent().expect("skill directory");
                let managed = kind != HermesRunKind::Foreground;
                Metadata::new(
                    directory,
                    managed,
                    if managed { "agent" } else { "user" },
                    chrono::Utc::now().timestamp_millis(),
                )
                .map_err(io)?
                .save(directory)
                .map_err(io)?;
            }
        } else if let Some(document) = document {
            let directory = document.path.parent().expect("skill directory");
            if directory.exists() && kind != (HermesRunKind::Curator { dry_run: true }) {
                let update = || -> std::io::Result<()> {
                    if let Some(mut m) = if writing {
                        before
                    } else {
                        Metadata::read(directory).ok()
                    } {
                        if writing {
                            m.patch_count = m.patch_count.saturating_add(1);
                            m.files = metadata::fingerprint(directory)?;
                        } else {
                            m.view_count = m.view_count.saturating_add(1);
                        }
                        // Maintenance reads must not make an idle skill immortal.
                        if kind == HermesRunKind::Foreground || writing {
                            m.last_activity_at = chrono::Utc::now().timestamp_millis();
                        }
                        m.save(directory)?;
                    }
                    Ok(())
                };
                if writing {
                    update().map_err(io)?;
                } else if let Ok(_read_gate) = metadata::content_lock(&root) {
                    let _ = update();
                }
            }
        }
    }
    Ok(result)
}

fn execute_inner(
    context: &ToolContext,
    store: &HermesMemoryStore,
    review: Option<&ReviewObservations>,
    kind: HermesRunKind,
    target: Option<&crate::curator::scope::Target>,
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
    let document = id.and_then(|id| resolve_skill(store, target, id));
    let requested_id = id.is_some();
    if let Some(document) = &document {
        input["skill_id"] = json!(document.id);
    }
    if matches!(kind, HermesRunKind::Curator { .. }) && action == "view" && document.is_none() {
        if requested_id {
            return Ok(error("Skill not found"));
        }
        let skills = store
            .list_skills()
            .map_err(|e| ToolError::Execution(e.to_string()))?
            .into_iter()
            .filter(|d| target.is_some_and(|t| d.scope == t.scope) && is_agent_owned(&d.path))
            .map(|d| json!({"name":d.name,"skillId":d.id,"description":d.description}))
            .collect::<Vec<_>>();
        let details = json!({"success":true,"skills":skills});
        let mut result = ToolResult::text(details.to_string());
        result.details = Some(details);
        return Ok(result);
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
                        .any(|d| {
                            (d.name == absorbed || d.id == absorbed)
                                && d.scope == document.scope
                                && is_agent_owned(&d.path)
                        })
                {
                    return Ok(error(
                        "Autonomous deletion requires an existing different absorbed_into skill; use verified consolidation.",
                    ));
                }
                if matches!(kind, HermesRunKind::Curator { .. }) {
                    let destination = store
                        .list_skills()
                        .map_err(|e| ToolError::Execution(e.to_string()))?
                        .into_iter()
                        .find(|d| {
                            (d.name == absorbed || d.id == absorbed)
                                && d.scope == document.scope
                                && is_agent_owned(&d.path)
                        })
                        .expect("validated destination");
                    let source_files =
                        metadata::fingerprint(document.path.parent().expect("skill directory"))
                            .map_err(io)?;
                    let destination_files =
                        metadata::fingerprint(destination.path.parent().expect("skill directory"))
                            .map_err(io)?;
                    if source_files.iter().any(|(path, digest)| {
                        path != "SKILL.md"
                            && !destination_files.values().any(|value| value == digest)
                    }) {
                        return Ok(error(
                            "Preserve every supporting file in the destination before retiring its source Skill; skip consolidation if an asset cannot be copied safely.",
                        ));
                    }
                }
                let root = store
                    .skill_root(document.scope)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                let _store_lock = metadata::lock(&root, ".skill-store.lock", true).map_err(io)?;
                let curator = crate::curator::Curator::new(root, crate::curator::Config::default());
                let archive_id = curator
                    .archive_locked(&document.name, "consolidation", Some(absorbed))
                    .map_err(io)?;
                let mut result = changed("Skill archived after consolidation");
                if let Some(details) = &mut result.details {
                    details["archiveId"] = json!(archive_id);
                    details["absorbedInto"] = json!(absorbed);
                }
                return Ok(result);
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
    if relative.eq_ignore_ascii_case("SKILL.md") {
        return Ok(path.to_path_buf());
    }
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || Path::new(relative)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("curator.json"))
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
    path.parent()
        .is_some_and(|directory| Metadata::read(directory).is_ok_and(|m| m.writable(directory)))
}
