use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use serde_json::{Value, json};

use crate::config::{
    MEMORY_TOOL_DESCRIPTION, SKILL_TOOL_DESCRIPTION, SessionSearchVariant, char_len, char_prefix,
};
use crate::database::{MemorySearchOptions, SessionSearchOptions};
use crate::execution::{HermesRunState, HermesRuns};
use crate::skills::{SkillCreate, SkillDocument, SkillError, SkillUpdate};
use crate::store::{
    FailureOptions, HermesMemoryStore, MemoryCategory, MemoryResult, MemoryTarget, StoreError,
};

pub(crate) fn register(
    context: &mut RegisterContext<'_>,
    store: Arc<HermesMemoryStore>,
    runs: Arc<HermesRuns>,
) -> pi_core::Result<()> {
    for operation in Operation::ALL {
        context.register_tool(Arc::new(HermesMemoryTool {
            store: Arc::clone(&store),
            runs: Arc::clone(&runs),
            operation,
        }))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Operation {
    Memory,
    SkillView,
    SkillsList,
    MemorySearch,
    SessionSearch,
    SkillManage,
}

impl Operation {
    const ALL: [Self; 6] = [
        Self::Memory,
        Self::SkillView,
        Self::SkillsList,
        Self::MemorySearch,
        Self::SessionSearch,
        Self::SkillManage,
    ];
}

struct HermesMemoryTool {
    store: Arc<HermesMemoryStore>,
    runs: Arc<HermesRuns>,
    operation: Operation,
}

#[async_trait]
impl Tool for HermesMemoryTool {
    fn spec(&self) -> ToolSpec {
        let (name, label, description, parameters, snippet, guidelines) = match self.operation {
            Operation::Memory => (
                "memory", "Memory", MEMORY_TOOL_DESCRIPTION.to_string(),
                json!({"type":"object","properties":{
                    "action":{"type":"string","enum":["add","replace","remove"]},
                    "target":{"type":"string","enum":["memory","user"]},
                    "content":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},
                    "operations":{"type":"array","items":{"type":"object","properties":{
                        "action":{"type":"string","enum":["add","replace","remove"]},"content":{"type":"string"},"new_text":{"type":"string"},"old_text":{"type":"string"}},"required":["action"]}}
                }}), "Save durable user facts and notes.", Vec::new()),
            Operation::SkillView => ("skill_view", "View Skill", "Read a skill before using or changing it.".into(),
                json!({"type":"object","properties":{"name":{"type":"string"},"skill_id":{"type":"string"},"file_path":{"type":"string"}}}),
                "Read current skill content.", Vec::new()),
            Operation::SkillsList => ("skills_list", "List Skills", "List available procedural skills.".into(),
                json!({"type":"object","properties":{}}), "Discover existing reusable skills.", Vec::new()),
            Operation::MemorySearch => (
                "memory_search",
                "Memory Search",
                MEMORY_SEARCH_DESCRIPTION.to_string(),
                json!({"type":"object","properties":{"query":{"type":"string","description":"Search query. Use natural language or specific terms."},"project":{"type":"string","description":"Filter by project name. Pass null for global memories only."},"target":{"type":"string","enum":["memory","user","failure","project"],"description":"Filter by target type: memory, user, failure, or project-attributed memories."},"category":{"type":"string","enum":["failure","correction","insight","preference","convention","tool-quirk"],"description":"Filter by memory category."},"limit":{"type":"number","description":"Maximum results to return (default: 10, max: 20)."}},"required":["query"],"additionalProperties":false}),
                "Search extended memory store (unlimited capacity)",
                vec![
                    "Use memory_search when you need context beyond what is in the system prompt.".to_string(),
                    "Use memory_search to find project-specific memories or user preferences.".to_string(),
                    "Use memory_search with category filter to find specific types of memories (failure, correction, insight, etc.).".to_string(),
                ],
            ),
            Operation::SessionSearch => match self.store.config().session_search_variant {
                SessionSearchVariant::Legacy => (
                    "session_search",
                    "Session Search",
                    SESSION_SEARCH_LEGACY_DESCRIPTION.to_string(),
                    json!({"type":"object","properties":{"query":{"type":"string","description":"Search query. Use natural language or specific terms."},"project":{"type":"string","description":"Filter by project name (optional)."},"role":{"type":"string","enum":["user","assistant"],"description":"Filter by message role (optional)."},"limit":{"type":"number","description":"Maximum results to return (default: 10, min: 1, max: 20).","minimum":1,"maximum":20},"snippetChars":{"type":"number","description":"Maximum characters per result snippet (default: 1200, max: 4000).","minimum":100,"maximum":4000}},"required":["query"],"additionalProperties":false}),
                    "Search past conversations for relevant context",
                    vec![
                        "Use session_search when the user asks about previous discussions or past work.".to_string(),
                        "Use session_search when you need context from earlier sessions.".to_string(),
                    ],
                ),
                SessionSearchVariant::Anchors => (
                    "session_search",
                    "Session Search",
                    SESSION_SEARCH_ANCHOR_DESCRIPTION.to_string(),
                    json!({"type":"object","properties":{"markdown":{"type":"string","description":"Markdown request with optional from/to/cwd/limit fields and all/any/exclude lists."}},"required":["markdown"],"additionalProperties":false}),
                    "Search past session JSONL files for compact source anchors",
                    vec![
                        "Use session_search with markdown only when the session search anchor mode is configured.".to_string(),
                        "Request source anchors, not summaries or previews.".to_string(),
                        "Use all for required terms, any for alternatives, and exclude for terms that must not appear in a returned range.".to_string(),
                    ],
                ),
            },
            Operation::SkillManage => (
                "skill_manage",
                "Skill Manager",
                SKILL_TOOL_DESCRIPTION.to_string(),
                json!({"type":"object","properties":{"action":{"type":"string","enum":["create","view","patch","update","edit","delete","write_file","remove_file"],"description":"The skill action to perform."},"name":{"type":"string","description":"Skill name for create. e.g., 'debug-typescript-errors'."},"skill_id":{"type":"string","description":"Stable skill id for view/patch/update/delete. e.g., 'global:debug-typescript-errors' or 'project:my-repo:release-app'. Legacy alias 'edit' also accepts this field."},"description":{"type":"string","description":"One-line description of when to use this skill. Required for create; optional for update/edit."},"scope":{"type":"string","enum":["global","project"],"description":"Required for create. Use 'global' for portable procedures and 'project' for repo-specific workflows."},"section":{"type":"string","description":"Required for patch. Section header to patch. e.g., 'Procedure', 'Pitfalls', 'Verification', 'When to Use'."},"file_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"absorbed_into":{"type":"string"},"content":{"type":"string","description":"Raw markdown body for create/update/edit, or Markdown section body for patch. Prefer structured fields over free-form content when possible. For patch, JSON arrays are auto-coerced for list sections; JSON objects are rejected."},"when_to_use":{"type":"string","description":"Structured create/update/edit field, or structured patch body when section is 'When to Use'."},"procedure_steps":{"type":"array","items":{"type":"string"},"description":"Structured create/update/edit field, or structured patch body when section is 'Procedure'. Ordered concrete steps."},"pitfalls":{"type":"array","items":{"type":"string"},"description":"Structured create/update/edit field, or structured patch body when section is 'Pitfalls'."},"verification_steps":{"type":"array","items":{"type":"string"},"description":"Structured create/update/edit field, or structured patch body when section is 'Verification'."}},"required":["action"],"additionalProperties":true}),
                "Create, inspect, and update reusable procedures and patterns",
                vec![
                    "Use skill_manage after completing complex tasks that required trial and error or multiple tool calls.".to_string(),
                    "Use 'create' to save a new reusable procedure, 'patch' to update a section of an existing skill by skill_id, and 'update' for a full rewrite.".to_string(),
                    "Scope is required on create: choose scope='global' for transferable procedures and scope='project' to write into the trusted Git checkout's .hermes/skills directory when the workflow depends on this repo's paths, scripts, conventions, or deploy steps.".to_string(),
                    "Prefer structured fields for create/update/patch: when_to_use, procedure_steps, pitfalls, and verification_steps. The tool renders valid SKILL.md sections for you.".to_string(),
                    "For patch, pass section plus the matching structured field (e.g. section='Procedure' with procedure_steps). Avoid free-form content that is a JSON array/object string.".to_string(),
                    "Prefer 'update' for multi-section rewrites when patch content would be large or format-unstable.".to_string(),
                    "Use 'view' before patching or updating when you need to inspect an existing skill.".to_string(),
                    "Do NOT use skills for temporary task state — only for durable, reusable procedures.".to_string(),
                ],
            ),
        };
        ToolSpec {
            name: name.to_string(),
            label: label.to_string(),
            description,
            parameters,
            execution_mode: ToolExecutionMode::Sequential,
            prompt_snippet: Some(snippet.to_string()),
            prompt_guidelines: guidelines,
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let run = self.runs.get(context.run_id());
        if matches!(self.operation, Operation::Memory | Operation::SkillManage)
            && context.run_id().is_some()
            && run.is_none()
        {
            return Err(ToolError::Execution(
                "Hermes invocation state is unavailable. Start this execution with the matching Hermes plugin; recreate a review after reloading its generation.".into(),
            ));
        }
        self.store
            .bind_project(context.cwd())
            .map_err(store_error)?;
        let result = match self.operation {
            Operation::Memory => self.execute_memory(run.as_deref(), &input),
            Operation::SkillView => self.execute_skill_manage(&context, run.as_deref(), &json!({"action":"view","skill_id":input.get("skill_id").or_else(|| input.get("name")), "file_path":input.get("file_path")})),
            Operation::SkillsList => self.execute_skill_manage(&context, run.as_deref(), &json!({"action":"view"})),
            Operation::MemorySearch => self.execute_memory_search(&input),
            Operation::SessionSearch => self.execute_session_search(&input),
            Operation::SkillManage => self.execute_skill_manage(&context, run.as_deref(), &input),
        }?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        Ok(result)
    }
}

impl HermesMemoryTool {
    fn execute_memory(
        &self,
        run: Option<&HermesRunState>,
        input: &Value,
    ) -> Result<ToolResult, ToolError> {
        let mut input = input.clone();
        if input.get("content").is_none()
            && let Some(content) = input.get("new_text").cloned()
        {
            input["content"] = content;
        }
        let target = target(&input)?;
        if !matches!(target, MemoryTarget::Memory | MemoryTarget::User) {
            return Ok(ToolResult::error("memory target must be memory or user"));
        }
        let result = if let Some(operations) = input
            .get("operations")
            .and_then(Value::as_array)
            .filter(|operations| !operations.is_empty())
        {
            let mut plan = Vec::new();
            for op in operations {
                let action = match op.get("action").and_then(Value::as_str) {
                    Some("add") => crate::store::MutationAction::Add,
                    Some("replace") => crate::store::MutationAction::Replace,
                    Some("remove") => crate::store::MutationAction::Remove,
                    _ => return self.finish_memory(run, target, MemoryResult::consolidation_error(
                        "Unknown or missing memory action. No operations were applied (batch is all-or-nothing)."
                    )),
                };
                plan.push(crate::store::MemoryMutationOperation {
                    action,
                    content: optional_string(op, "content")
                        .or_else(|| optional_string(op, "new_text")),
                    old_text: optional_string(op, "old_text"),
                    category: None,
                    failure_reason: None,
                    project: None,
                });
            }
            self.store
                .apply_mutation_plan(target, &plan, false)
                .map_err(store_error)?
        } else {
            match required(&input, "action")? {
                "add" => self
                    .store
                    .add(
                        target,
                        required(&input, "content")?,
                        FailureOptions::default(),
                    )
                    .map_err(store_error)?,
                "replace" => self.execute_replace(&input)?,
                "remove" => self.execute_remove(&input)?,
                _ => return Ok(ToolResult::error("Unknown memory action")),
            }
        };
        self.finish_memory(run, target, result)
    }

    fn finish_memory(
        &self,
        run: Option<&HermesRunState>,
        target: MemoryTarget,
        result: MemoryResult,
    ) -> Result<ToolResult, ToolError> {
        let result = match run {
            Some(run) => run.consolidation.observe(result),
            None => result,
        };
        let retryable = result.consolidation_failure && result.done != Some(true);
        let result = render_memory_result(result);
        let mut details = result.details.unwrap_or_else(|| json!({}));
        if result.is_error {
            if retryable {
                let current_entries = self.store.entries(target).map_err(store_error)?;
                let current = char_len(&current_entries.join("\n§\n"));
                if details.get("current_entries").is_none() {
                    details["current_entries"] = json!(&current_entries);
                }
                if details.get("usage").is_none() {
                    let limit = match target {
                        MemoryTarget::Memory => self.store.config().memory_char_limit,
                        MemoryTarget::User => self.store.config().user_char_limit,
                        MemoryTarget::Project | MemoryTarget::Failure => unreachable!(
                            "the public memory tool accepts only memory and user targets"
                        ),
                    };
                    details["usage"] = json!(format!("{current}/{limit} chars"));
                }
                details["guidance"] = json!(
                    "Use replace/remove or an atomic operations batch to consolidate, then retry within this turn. Do not start another Agent."
                );
            }
        } else {
            details["done"] = json!(true);
            if details.get("message").and_then(Value::as_str)
                != Some("Entry already exists (no duplicate added).")
            {
                details["_change"] = json!(format!("Memory updated: {}", target.as_str()));
            }
            if let Some(object) = details.as_object_mut() {
                object.remove("entries");
            }
        }
        Ok(tool_json(details.to_string(), details, result.is_error))
    }

    fn execute_replace(&self, input: &Value) -> Result<MemoryResult, ToolError> {
        let target = target(input)?;
        let mut result = self
            .store
            .replace(
                target,
                required(input, "old_text")?,
                required(input, "content")?,
            )
            .map_err(store_error)?;
        add_wrong_target_hint(
            &self.store,
            target,
            required(input, "old_text")?,
            &mut result,
        );
        Ok(result)
    }

    fn execute_remove(&self, input: &Value) -> Result<MemoryResult, ToolError> {
        let target = target(input)?;
        let mut result = self
            .store
            .remove(target, required(input, "old_text")?)
            .map_err(store_error)?;
        add_wrong_target_hint(
            &self.store,
            target,
            required(input, "old_text")?,
            &mut result,
        );
        Ok(result)
    }

    fn execute_memory_search(&self, input: &Value) -> Result<ToolResult, ToolError> {
        let Some(query) = input
            .get("query")
            .and_then(Value::as_str)
            .filter(|query| !query.trim().is_empty())
        else {
            return Ok(tool_json(
                "query is required",
                json!({"success":false,"message":"query is required"}),
                true,
            ));
        };
        if self.store.indexed_memory_count().map_err(store_error)? == 0 {
            return Ok(tool_json(
                "No memories in extended store yet. Use memory(action='add') to store memories.",
                json!({"success":false,"message":"No memories in extended store yet. Use memory(action='add') to store memories."}),
                true,
            ));
        }
        let project = if input.get("project").is_some_and(Value::is_null) {
            Some(None)
        } else {
            input
                .get("project")
                .and_then(Value::as_str)
                .map(|value| Some(value.to_string()))
        };
        let options = MemorySearchOptions {
            project,
            target: input
                .get("target")
                .and_then(Value::as_str)
                .map(MemoryTarget::parse)
                .transpose()
                .map_err(store_error)?,
            category: input
                .get("category")
                .and_then(Value::as_str)
                .and_then(MemoryCategory::parse),
            limit: number(input, "limit", 10, 1, 20),
        };
        let hits = self
            .store
            .search_memories(query, &options)
            .map_err(store_error)?;
        if hits.is_empty() {
            return Ok(tool_json(
                format!(
                    "No memories found matching \"{query}\". Try a different search term or broader query."
                ),
                json!({"success":true,"count":0,"message":format!("No memories found matching \"{query}\". Try a different search term or broader query.")}),
                false,
            ));
        }
        let mut output = format!("Found {} memories matching \"{query}\":\n\n", hits.len());
        for hit in &hits {
            let mutation_target = if hit.target == MemoryTarget::Project {
                MemoryTarget::Project
            } else {
                hit.target
            };
            let scope = hit.project.as_ref().map_or_else(
                || "global".to_string(),
                |project| format!("project:{}", percent_encode(project)),
            );
            let icon = match hit.target {
                MemoryTarget::User => "👤",
                MemoryTarget::Failure => "⚠️",
                MemoryTarget::Memory | MemoryTarget::Project => "🧠",
            };
            let category = hit
                .category
                .map(|category| format!(" [{}]", category.as_str()))
                .unwrap_or_default();
            output.push_str(&format!(
                "{icon} scope={scope} [target={}]{} {}\n   Created: {} | Last used: {}\n\n",
                mutation_target.as_str(),
                category,
                hit.content,
                hit.created,
                hit.last_referenced
            ));
        }
        let output = output.trim().to_string();
        Ok(tool_json(
            output.clone(),
            json!({"success":true,"count":hits.len(),"output":output}),
            false,
        ))
    }

    fn execute_session_search(&self, input: &Value) -> Result<ToolResult, ToolError> {
        if self.store.config().session_search_variant == SessionSearchVariant::Anchors {
            let Some(markdown) = input
                .get("markdown")
                .and_then(Value::as_str)
                .filter(|markdown| !markdown.trim().is_empty())
            else {
                return Ok(tool_json(
                    "markdown is required",
                    json!({"success":false,"message":"markdown is required"}),
                    false,
                ));
            };
            return crate::anchor_search::execute(markdown, self.store.database().session_roots())
                .map_err(ToolError::Execution);
        }
        let Some(query) = input
            .get("query")
            .and_then(Value::as_str)
            .filter(|query| !query.trim().is_empty())
        else {
            return Ok(tool_json(
                "query is required",
                json!({"success":false,"message":"query is required"}),
                false,
            ));
        };
        if self
            .store
            .database()
            .indexed_message_count()
            .map_err(store_error)?
            == 0
        {
            return Ok(tool_json(
                "No sessions indexed yet. Run /memory-index-sessions to import past sessions.",
                json!({"success":false,"message":"No sessions indexed yet. Run /memory-index-sessions to import past sessions."}),
                false,
            ));
        }
        let snippet_chars = number(input, "snippetChars", 1_200, 100, 4_000);
        let options = SessionSearchOptions {
            project: optional_string(input, "project"),
            role: optional_string(input, "role"),
            since: None,
            limit: number(input, "limit", 10, 1, 20),
            snippet_chars,
        };
        let hits = self
            .store
            .search_sessions(query, &options)
            .map_err(store_error)?;
        let mut truncated_count = 0;
        let text = if hits.is_empty() {
            "No results found. Try a different search term or broader query.".to_string()
        } else {
            let mut blocks = vec![format!("Found {} results for \"{query}\":", hits.len())];
            for hit in &hits {
                let (snippet, truncated) = truncate(&hit.snippet, snippet_chars);
                truncated_count += usize::from(truncated);
                blocks.push(format!(
                    "---\n📅 {} | 📁 {} | {}\n{}",
                    display_date(&hit.timestamp),
                    hit.project,
                    if hit.role == "user" {
                        "👤 User"
                    } else {
                        "🤖 Assistant"
                    },
                    snippet
                ));
            }
            blocks.join("\n\n")
        };
        let (text, output_truncated) = cap_output(text, 50 * 1024);
        Ok(tool_json(
            text.clone(),
            json!({"success":true,"count":hits.len(),"truncatedCount":truncated_count,"snippetChars":snippet_chars,"outputChars":char_len(&text),"outputTruncated":output_truncated}),
            false,
        ))
    }

    fn execute_skill_manage(
        &self,
        context: &ToolContext,
        run: Option<&HermesRunState>,
        input: &Value,
    ) -> Result<ToolResult, ToolError> {
        let review = run.and_then(|run| run.review.as_ref());
        crate::skill_review::execute(context, &self.store, review, input, |input| {
            skill_manage(&self.store, input)
        })
    }
}

fn skill_manage(store: &HermesMemoryStore, input: &Value) -> Result<ToolResult, ToolError> {
    let Some(action) = input
        .get("action")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(skill_input_error("action is required."));
    };
    match action {
        "create" => {
            let Some(name) = optional_string(input, "name") else {
                return Ok(skill_input_error("name is required for 'create' action."));
            };
            let Some(description) = optional_string(input, "description") else {
                return Ok(skill_input_error(
                    "description is required for 'create' action.",
                ));
            };
            let body = match build_body(input) {
                Ok(Some(body)) => body,
                Ok(None) => {
                    return Ok(skill_input_error(
                        "Either content or structured fields are required. Prefer when_to_use, procedure_steps, pitfalls, and verification_steps for create/update.",
                    ));
                }
                Err(error) => return Ok(skill_input_error(error)),
            };
            let Some(scope_value) = input.get("scope").cloned() else {
                return Ok(skill_input_error(
                    "scope is required for 'create' action. Use 'global' or 'project'.",
                ));
            };
            let scope = match serde_json::from_value(scope_value) {
                Ok(scope) => scope,
                Err(_) => {
                    return Ok(skill_input_error(
                        "scope is required for 'create' action. Use 'global' or 'project'.",
                    ));
                }
            };
            Ok(
                match store.create_skill(SkillCreate {
                    scope,
                    name,
                    description,
                    body,
                }) {
                    Ok(document) => {
                        let label = document.display_name.as_deref().unwrap_or(&document.name);
                        render_skill_mutation(
                            format!(
                                "Skill '{label}' created as a {} skill.",
                                document.scope.as_str()
                            ),
                            &document,
                        )
                    }
                    Err(error) => render_skill_error(error),
                },
            )
        }
        "view" => {
            if let Some(skill_id) = optional_string(input, "skill_id") {
                Ok(match store.view_skill(&skill_id) {
                    Ok(document) => render_skill_document(&document),
                    Err(error) => render_skill_error(error),
                })
            } else {
                Ok(match store.list_skills() {
                    Ok(skills) => {
                        let details = json!({"success":true,"skills":skills.iter().map(skill_index_json).collect::<Vec<_>>()});
                        tool_json(details.to_string(), details, false)
                    }
                    Err(error) => render_skill_error(error),
                })
            }
        }
        "patch" => {
            let Some(skill_id) = optional_string(input, "skill_id") else {
                return Ok(skill_input_error(
                    "skill_id is required for 'patch' action.",
                ));
            };
            let Some(section_text) = optional_string(input, "section") else {
                return Ok(skill_input_error("section is required for 'patch' action."));
            };
            let content = match patch_content(input, &section_text) {
                Ok(content) => content,
                Err(error) => return Ok(skill_input_error(error)),
            };
            Ok(
                match store.patch_skill(&skill_id, &section_text, &content) {
                    Ok(document) => {
                        let label = document.display_name.as_deref().unwrap_or(&document.name);
                        let section = section_text.trim_start_matches('#').trim();
                        render_skill_mutation(
                            format!("Skill '{label}' section '{section}' updated."),
                            &document,
                        )
                    }
                    Err(error) => render_skill_error(error),
                },
            )
        }
        "update" | "edit" => {
            let Some(skill_id) = optional_string(input, "skill_id") else {
                return Ok(skill_input_error(format!(
                    "skill_id is required for '{action}' action."
                )));
            };
            let body_result = build_body(input);
            let description = optional_string(input, "description");
            let body = match body_result {
                Ok(body) => body,
                Err(error) => return Ok(skill_input_error(error)),
            };
            if description.is_none() && body.is_none() {
                return Ok(skill_input_error(format!(
                    "Provide description, content, or structured fields for '{action}'."
                )));
            }
            Ok(
                match store.update_skill(SkillUpdate {
                    skill_id,
                    description,
                    body,
                }) {
                    Ok(document) => {
                        let label = document.display_name.as_deref().unwrap_or(&document.name);
                        render_skill_mutation(format!("Skill '{label}' updated."), &document)
                    }
                    Err(error) => render_skill_error(error),
                },
            )
        }
        "delete" => {
            let Some(skill_id) = optional_string(input, "skill_id") else {
                return Ok(skill_input_error(
                    "skill_id is required for 'delete' action.",
                ));
            };
            Ok(match store.delete_skill(&skill_id) {
                Ok(document) => {
                    let label = document.display_name.as_deref().unwrap_or(&document.name);
                    let details = json!({"success":true,"message":format!("Skill '{label}' deleted."),"fileName":document.file_name,"skillId":document.id,"scope":document.scope.as_str(),"path":document.path});
                    tool_json(details.to_string(), details, false)
                }
                Err(error) => render_skill_error(error),
            })
        }
        other => Ok(skill_input_error(format!(
            "Unknown action '{other}'. Use: create, view, patch, update, delete"
        ))),
    }
}

fn build_body(input: &Value) -> Result<Option<String>, String> {
    if let Some(content) = optional_string(input, "content") {
        return Ok(Some(content));
    }
    let when_to_use = optional_string(input, "when_to_use");
    let procedure_steps = string_array(input, "procedure_steps");
    let pitfalls = string_array(input, "pitfalls");
    let verification_steps = string_array(input, "verification_steps");
    let has_structured = when_to_use.is_some()
        || !procedure_steps.is_empty()
        || !pitfalls.is_empty()
        || !verification_steps.is_empty();
    if !has_structured {
        return Ok(None);
    }
    let when_to_use = when_to_use
        .ok_or_else(|| "when_to_use is required when content is omitted.".to_string())?;
    if procedure_steps.is_empty() {
        return Err("procedure_steps is required when content is omitted.".to_string());
    }
    if verification_steps.is_empty() {
        return Err("verification_steps is required when content is omitted.".to_string());
    }
    let ordered = |items: &[String]| {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}. {item}", index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let pitfalls = if pitfalls.is_empty() {
        "- No notable pitfalls recorded yet.".to_string()
    } else {
        pitfalls
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Some(format!(
        "## When to Use\n{when_to_use}\n\n## Procedure\n{}\n\n## Pitfalls\n{pitfalls}\n\n## Verification\n{}",
        ordered(&procedure_steps),
        ordered(&verification_steps)
    )))
}

fn patch_content(input: &Value, section: &str) -> Result<String, String> {
    let section = section
        .trim_start_matches('#')
        .trim()
        .to_ascii_lowercase()
        .replace('_', " ");
    let procedure = string_array(input, "procedure_steps");
    let pitfalls = string_array(input, "pitfalls");
    let verification = string_array(input, "verification_steps");
    let when = optional_string(input, "when_to_use");
    let has_procedure = !procedure.is_empty();
    let has_pitfalls = !pitfalls.is_empty();
    let has_verification = !verification.is_empty();
    if section == "procedure" && has_procedure {
        return list_markdown(&procedure, true);
    }
    if section == "pitfalls" && has_pitfalls {
        return list_markdown(&pitfalls, false);
    }
    if section == "verification" && has_verification {
        return list_markdown(&verification, true);
    }
    if section == "when to use"
        && let Some(when) = when.as_deref()
    {
        return Ok(when.to_string());
    }
    if let Some(content) = optional_string(input, "content") {
        return Ok(content);
    }
    let populated = [
        has_procedure.then_some((procedure.as_slice(), true)),
        has_pitfalls.then_some((pitfalls.as_slice(), false)),
        has_verification.then_some((verification.as_slice(), true)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if populated.len() == 1 && when.is_none() {
        return list_markdown(populated[0].0, populated[0].1);
    }
    if populated.is_empty()
        && let Some(when) = when.as_deref()
    {
        return Ok(when.to_string());
    }
    let structured_count = usize::from(has_procedure)
        + usize::from(has_pitfalls)
        + usize::from(has_verification)
        + usize::from(when.is_some());
    if structured_count > 1 {
        return Err("For patch, provide content or exactly one structured field matching the target section (procedure_steps, pitfalls, verification_steps, or when_to_use). Use update for multi-section rewrites.".to_string());
    }
    Err("content or a matching structured field is required for 'patch' action. Prefer procedure_steps/pitfalls/verification_steps/when_to_use.".to_string())
}

fn list_markdown(items: &[String], ordered: bool) -> Result<String, String> {
    if items.is_empty() {
        return Err("a matching non-empty structured field is required for patch".to_string());
    }
    Ok(items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if ordered {
                format!("{}. {item}", index + 1)
            } else {
                format!("- {item}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn render_skill_mutation(message: impl Into<String>, skill: &SkillDocument) -> ToolResult {
    let details = json!({"success":true,"message":message.into(),"fileName":skill.file_name,"skillId":skill.id,"scope":skill.scope.as_str(),"path":skill.path});
    tool_json(details.to_string(), details, false)
}

fn skill_input_error(error: impl Into<String>) -> ToolResult {
    let text = json!({"success":false,"error":error.into()}).to_string();
    tool_json(text, json!({}), false)
}

fn render_skill_document(skill: &SkillDocument) -> ToolResult {
    let details = json!({"success":true,"skillId":skill.id,"scope":skill.scope.as_str(),"fileName":skill.file_name,"path":skill.path,"projectName":skill.project_name,"name":skill.name,"displayName":skill.display_name,"description":skill.description,"created":skill.created,"updated":skill.updated,"version":skill.version,"body":skill.body});
    tool_json(details.to_string(), details, false)
}

fn skill_index_json(skill: &SkillDocument) -> Value {
    json!({"skillId":skill.id,"scope":skill.scope.as_str(),"fileName":skill.file_name,"path":skill.path,"projectName":skill.project_name,"name":skill.name,"displayName":skill.display_name,"description":skill.description,"created":skill.created,"updated":skill.updated})
}

fn render_skill_error(error: SkillError) -> ToolResult {
    let mut details = json!({"success":false,"error":error.to_string()});
    if let Some((conflict, ids, suggested)) = error.conflict_details()
        && let Some(object) = details.as_object_mut()
    {
        object.insert("conflictType".to_string(), json!(conflict));
        if !ids.is_empty() {
            object.insert("similarSkillIds".to_string(), json!(ids));
        }
        object.insert("suggestedAction".to_string(), json!(suggested));
    }
    tool_json(details.to_string(), details, false)
}

fn add_wrong_target_hint(
    store: &HermesMemoryStore,
    target: MemoryTarget,
    old_text: &str,
    result: &mut MemoryResult,
) {
    if result.success
        || !result
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("No entry matched"))
    {
        return;
    }
    let alternatives = store
        .matching_targets(old_text)
        .into_iter()
        .filter(|candidate| *candidate != target)
        .collect::<Vec<_>>();
    if alternatives.is_empty() {
        return;
    }
    let targets = alternatives
        .iter()
        .map(|target| format!("\"{}\"", target.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    result.error = Some(format!(
        "No match in target \"{}\"; matching entry found in {} {targets}. Retry with the displayed target.",
        target.as_str(),
        if alternatives.len() == 1 {
            "target"
        } else {
            "targets"
        }
    ));
    result.matching_targets = alternatives;
}

fn render_memory_result(result: MemoryResult) -> ToolResult {
    let is_error = !result.success;
    let text = if result.success && !result.evicted_entries.is_empty() {
        let mut lines = vec![
            result.message.clone().unwrap_or_default(),
            String::new(),
            "Rotated active memory entries:".to_string(),
            String::new(),
        ];
        for (index, entry) in result.evicted_entries.iter().enumerate() {
            lines.push(format!("{}. {entry}", index + 1));
            lines.push(String::new());
        }
        lines.push("If one of these entries should stay active, add it again.".to_string());
        if let Some(usage) = result.usage.as_deref() {
            lines.push(format!("Usage: {usage}"));
        }
        lines.join("\n").trim().to_string()
    } else {
        serde_json::to_string(&result).unwrap_or_else(|error| error.to_string())
    };
    let details = serde_json::to_value(result).ok();
    ToolResult {
        content: vec![pi_core::ContentBlock::Text(pi_core::TextContent::new(text))],
        details,
        usage: None,
        added_tool_names: None,
        is_error,
        terminate: false,
    }
}

fn tool_json(text: impl Into<String>, details: Value, is_error: bool) -> ToolResult {
    ToolResult {
        content: vec![pi_core::ContentBlock::Text(pi_core::TextContent::new(text))],
        details: Some(details),
        usage: None,
        added_tool_names: None,
        is_error,
        terminate: false,
    }
}

const MEMORY_SEARCH_DESCRIPTION: &str = r#"Search extended memory store for relevant entries. Use this when you need context beyond what's in the system prompt — the extended store has unlimited capacity and is searchable.

Use cases:
- Find memories about a specific topic: "What do I know about auth setup?"
- Search legacy project-attributed memories: "What conventions were saved for project X?"
- Find user preferences: "What are the user's testing preferences?"
- Search for past failures: "memory_search('auth', category='failure')"

target="project" returns only legacy project-attributed entries; combine with project to search a named project. These entries remain searchable for migration compatibility but are read-only.

Returns matching memory entries with their scope and dates. Global memory/user targets can be passed to memory(action='replace') and memory(action='remove'); legacy project entries cannot be mutated through the memory tool."#;

const SESSION_SEARCH_LEGACY_DESCRIPTION: &str = r#"Search across past Pi coding sessions for relevant conversation context. Use this when the user asks about previous discussions, past work, or when you need context from earlier sessions.

Examples:
- "What did we discuss about auth last week?"
- "Find the PR where we fixed the test hang"
- "What approach did we take for the database migration?"

Returns bounded conversation snippets with session dates and project context. Large messages are truncated with their original character count."#;

const SESSION_SEARCH_ANCHOR_DESCRIPTION: &str = r#"Search Pi session JSONL files in the opt-in anchor mode using a Markdown request.

This mode accepts only a markdown request. Supported scalar fields are from, to, cwd, and limit. Supported list sections are all, any, and exclude: all terms must match, any requires at least one listed term, and exclude removes matching ranges. It returns compact JSONL line-range anchors, not summaries or previews. Output is plain text: count, optional message, then anchors as path:startLine-endLine with a short reason.

Example:
from: 2026-05-14
to: 2026-05-15
cwd: /path/to/project
limit: 20

all:
- alpha

any:
- beta
- gamma

exclude:
- delta"#;

fn target(input: &Value) -> Result<MemoryTarget, ToolError> {
    MemoryTarget::parse(
        input
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("memory"),
    )
    .map_err(store_error)
}

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a non-empty string")))
}

fn optional_string(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn number(input: &Value, key: &str, default: usize, minimum: usize, maximum: usize) -> usize {
    input
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value.floor().max(minimum as f64).min(maximum as f64) as usize)
        .unwrap_or(default)
}

fn truncate(text: &str, limit: usize) -> (String, bool) {
    if char_len(text) <= limit {
        return (text.to_string(), false);
    }
    let prefix = char_prefix(text, limit);
    (
        format!(
            "{prefix}\n... (truncated, {} chars total — refine the query or increase snippetChars)",
            char_len(text)
        ),
        true,
    )
}

fn cap_output(text: String, limit: usize) -> (String, bool) {
    let count = char_len(&text);
    if count <= limit {
        return (text, false);
    }
    let suffix = format!(
        "\n... (output truncated, {count} chars total — refine the query or lower the result limit)"
    );
    let keep = limit.saturating_sub(char_len(&suffix));
    (format!("{}{suffix}", char_prefix(&text, keep)), true)
}

fn display_date(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%b %-d, %Y").to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments(message.into())
}

fn store_error(error: StoreError) -> ToolError {
    ToolError::Execution(error.to_string())
}
