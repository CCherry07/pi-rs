//! User-facing Hermes slash commands.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    RegisterContext, UiMultiSelectAction, UiMultiSelectOption, UiMultiSelectRequest,
};

use crate::config::{STANDING_MAX_CHARS, STANDING_MAX_ENTRIES, char_len, char_prefix};
use crate::execution::HermesRuns;
use crate::store::{HermesMemoryStore, MemoryTarget};

pub(crate) fn register(
    context: &mut RegisterContext<'_>,
    store: Arc<HermesMemoryStore>,
    runs: Arc<HermesRuns>,
) -> pi_core::Result<()> {
    let mut kinds = vec![
        Kind::Consolidate,
        Kind::IndexSessions,
        Kind::Insights,
        Kind::Interview,
        Kind::Learn,
        Kind::Preview,
        Kind::Skills,
        Kind::SwitchProject,
        Kind::SyncMarkdown,
    ];
    if store.standing().is_some() {
        kinds.push(Kind::Pin);
    }
    for kind in kinds {
        context.register_command(Arc::new(HermesCommand {
            store: Arc::clone(&store),
            runs: Arc::clone(&runs),
            kind,
        }))?;
    }
    Ok(())
}

struct HermesCommand {
    store: Arc<HermesMemoryStore>,
    runs: Arc<HermesRuns>,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    Consolidate,
    IndexSessions,
    Insights,
    Interview,
    Learn,
    Preview,
    Skills,
    Pin,
    SwitchProject,
    SyncMarkdown,
}

impl Kind {
    fn spec(self) -> CommandSpec {
        let (name, description, hint) = match self {
            Self::Consolidate => (
                "memory-consolidate",
                "Manually trigger memory consolidation to free up space",
                None,
            ),
            Self::IndexSessions => (
                "memory-index-sessions",
                "Import past Pi sessions into the search database",
                None,
            ),
            Self::Insights => (
                "memory-insights",
                "Show what's stored in persistent memory",
                None,
            ),
            Self::Interview => (
                "memory-interview",
                "Answer a few questions to pre-fill your user profile so the agent remembers you across sessions",
                None,
            ),
            Self::Learn => (
                "learn-memory-tool",
                "Learn how to use the pi-hermes-memory extension effectively",
                None,
            ),
            Self::Preview => (
                "memory-preview-context",
                "Preview the memory policy or legacy memory context blocks",
                None,
            ),
            Self::Skills => (
                "memory-skills",
                "Manage global, active-project, and loaded external procedural skills",
                None,
            ),
            Self::Pin => (
                "memory-pin",
                "Pin a standing instruction that is injected into every session",
                Some("[list | remove <n> | clear | <instruction>]"),
            ),
            Self::SwitchProject => (
                "memory-switch-project",
                "Switch the active project for project-scoped memory",
                None,
            ),
            Self::SyncMarkdown => (
                "memory-sync-markdown",
                "Reconcile the SQLite search mirror with Markdown memories",
                None,
            ),
        };
        CommandSpec {
            name: name.to_string(),
            description: description.to_string(),
            argument_hint: hint.map(str::to_string),
        }
    }
}

#[async_trait]
impl Command for HermesCommand {
    fn spec(&self) -> CommandSpec {
        self.kind.spec()
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        context
            .signal()
            .check()
            .map_err(|_| CommandError::Aborted)?;
        self.store.bind_project(context.cwd()).map_err(execution)?;
        let arguments = arguments.trim();
        match self.kind {
            Kind::Interview => {
                if let Ok(entries) = self.store.entries(MemoryTarget::User)
                    && !entries.is_empty()
                {
                    let previews = entries
                        .iter()
                        .map(|entry| format!("     • {}", preview(entry, 80)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    notify(
                        &context,
                        format!(
                            "\n  🧠 You already have {} profile {}:\n{}\n\n  Starting the interview will add to or update these.\n",
                            entries.len(),
                            if entries.len() == 1 {
                                "entry"
                            } else {
                                "entries"
                            },
                            previews,
                        ),
                    )?;
                }
                return Ok(CommandOutcome::TransformInput(INTERVIEW_PROMPT.to_string()));
            }
            Kind::Consolidate => {
                let mut outcomes = Vec::new();
                let target_count = 2;
                let _ = context.ui.notify(
                    NoticeLevel::Info,
                    format!(
                        "🔄 Starting memory consolidation for {target_count} target{}...",
                        if target_count == 1 { "" } else { "s" }
                    ),
                );
                for target in [MemoryTarget::Memory, MemoryTarget::User] {
                    let label = target.as_str();
                    if self.store.entries(target).map_err(execution)?.is_empty() {
                        outcomes.push(format!("{label}: (empty, nothing to consolidate)"));
                        continue;
                    }
                    let _ = context
                        .ui
                        .notify(NoticeLevel::Info, format!("⏳ Consolidating {label}..."));
                    let result = crate::consolidation::with_command_context(
                        &context,
                        Arc::clone(&self.store),
                        Arc::clone(&self.runs),
                        target,
                    )
                    .await;
                    outcomes.push(if result.consolidated {
                        format!("{label}: ✅ consolidated")
                    } else {
                        format!(
                            "{label}: ❌ {}",
                            result.error.unwrap_or_else(|| "unknown error".to_string())
                        )
                    });
                }
                notify(
                    &context,
                    format!(
                        "\n  🔄 Memory Consolidation\n  {}\n{}",
                        "─".repeat(30),
                        outcomes
                            .iter()
                            .map(|outcome| format!("  {outcome}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                )?;
            }
            Kind::IndexSessions => {
                notify(&context, "🔍 Scanning session directories...")?;
                let (files, projects) = self.store.session_file_inventory();
                notify(
                    &context,
                    format!(
                        "📁 Found {files} session files across {projects} projects\n⏳ Indexing..."
                    ),
                )?;
                let result = match self.store.backfill_sessions(None) {
                    Ok(result) => result,
                    Err(error) => {
                        notify_level(
                            &context,
                            NoticeLevel::Error,
                            format!("❌ Session indexing failed: {error}"),
                        )?;
                        return Ok(CommandOutcome::Handled);
                    }
                };
                let stats = match self.store.session_stats() {
                    Ok(stats) => stats,
                    Err(error) => {
                        notify_level(
                            &context,
                            NoticeLevel::Error,
                            format!("❌ Session indexing failed: {error}"),
                        )?;
                        return Ok(CommandOutcome::Handled);
                    }
                };
                let mut output = format!(
                    "\n✅ Session indexing complete!\n\n📊 Results:\n├─ Sessions processed: {}\n├─ Sessions indexed: {}\n├─ Sessions skipped (already indexed): {}\n└─ Messages indexed: {}\n",
                    result.sessions_processed,
                    result.sessions_indexed,
                    result.sessions_skipped,
                    result.messages_indexed,
                );
                if !stats.projects.is_empty() {
                    output.push_str("\n📁 Projects indexed:\n");
                    for project in &stats.projects {
                        output.push_str(&format!(
                            "├─ {}: {} sessions, {} messages\n",
                            project.project, project.sessions, project.messages
                        ));
                    }
                }
                output.push_str(&format!(
                    "\n📈 Database totals:\n├─ {} sessions\n├─ {} messages\n└─ {} projects\n",
                    stats.total_sessions,
                    stats.total_messages,
                    stats.projects.len()
                ));
                if !result.errors.is_empty() {
                    output.push_str(&format!("\n⚠️ Errors ({}):\n", result.errors.len()));
                    for error in result.errors.iter().take(3) {
                        output.push_str(&format!("├─ {error}\n"));
                    }
                    if result.errors.len() > 3 {
                        output.push_str(&format!("└─ ... and {} more\n", result.errors.len() - 3));
                    }
                }
                output.push_str(
                    "\n💡 Use the session_search tool to search across indexed sessions.",
                );
                notify(&context, output)?;
            }
            Kind::Insights => {
                notify(&context, render_insights(&self.store).map_err(execution)?)?;
            }
            Kind::Learn => {
                learn(&context).await?;
            }
            Kind::Preview => {
                notify(&context, render_preview(&self.store).map_err(execution)?)?;
            }
            Kind::Skills => {
                manage_skills(&context, &self.store).await?;
            }
            Kind::Pin => {
                pin(&context, &self.store, arguments)?;
            }
            Kind::SwitchProject => {
                let projects = self.store.project_summaries().map_err(execution)?;
                let text = if projects.is_empty() {
                    "\n  📁 No existing project memories found.\n\n  The memory tool writes global memory/user notes; use project-scoped skills for reusable repository procedures.\n".to_string()
                } else {
                    let rows = projects
                        .iter()
                        .map(|(name, count)| {
                            format!(
                                "  📁 {name} ({count} {})",
                                if *count == 1 { "entry" } else { "entries" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "\n  ╔══════════════════════════════════════════════╗\n  ║        📁 Project Memory — Switch           ║\n  ╚══════════════════════════════════════════════╝\n\n  Available project memories:\n\n{rows}\n\n  Use memory(action='add') with target 'project' to manage\n  project-scoped memory. Project is auto-detected from\n  your current directory: {}",
                        context.cwd().display()
                    )
                };
                notify(&context, text)?;
            }
            Kind::SyncMarkdown => {
                notify(
                    &context,
                    "🔄 Reconciling the SQLite search mirror with Markdown memories...",
                )?;
                let result = match self.store.sync_markdown_memories() {
                    Ok(result) => result,
                    Err(error) => {
                        notify_level(
                            &context,
                            NoticeLevel::Error,
                            format!("❌ Markdown sync failed: {error}"),
                        )?;
                        return Ok(CommandOutcome::Handled);
                    }
                };
                let mut output = format!(
                    "\n✅ Markdown → SQLite sync complete!\n\n📊 Results:\n├─ Files scanned: {}\n├─ Entries scanned: {}\n├─ Imported into SQLite: {}\n├─ Skipped as duplicates: {}\n└─ Removed orphaned rows: {}\n",
                    result.files_scanned,
                    result.entries_scanned,
                    result.imported,
                    result.skipped,
                    result.removed,
                );
                if result.project_count > 0 {
                    output.push_str(&format!(
                        "\n📁 Project memories scanned: {}\n",
                        result.project_count
                    ));
                }
                if !result.warnings.is_empty() {
                    output.push_str(&format!("\n⚠️ Warnings ({}):\n", result.warnings.len()));
                    for warning in result.warnings.iter().take(5) {
                        output.push_str(&format!("├─ {warning}\n"));
                    }
                    if result.warnings.len() > 5 {
                        output
                            .push_str(&format!("└─ ... and {} more\n", result.warnings.len() - 5));
                    }
                }
                output.push_str(
                    "\n💡 Re-running this command is safe — existing SQLite rows are de-duplicated.",
                );
                notify(&context, output)?;
            }
        }
        Ok(CommandOutcome::Handled)
    }
}

fn render_insights(store: &HermesMemoryStore) -> Result<String, crate::store::StoreError> {
    let mut sections = vec![
        String::new(),
        "  ╔══════════════════════════════════════════════╗".to_string(),
        "  ║            🧠 Memory Insights                ║".to_string(),
        "  ╚══════════════════════════════════════════════╝".to_string(),
        String::new(),
        render_entries(
            "📋 MEMORY (your personal notes)",
            &store.entries(MemoryTarget::Memory)?,
        ),
        String::new(),
        render_entries("👤 USER PROFILE", &store.entries(MemoryTarget::User)?),
        String::new(),
    ];
    if let Some(name) = store.current_project_name() {
        sections.push(render_entries(
            &format!("📁 PROJECT MEMORY: {name}"),
            &store.entries(MemoryTarget::Project)?,
        ));
        sections.push(String::new());
    }
    Ok(sections.join("\n"))
}

fn render_entries(title: &str, entries: &[String]) -> String {
    let mut lines = vec![format!("  {title}"), format!("  {}", "─".repeat(44))];
    if entries.is_empty() {
        lines.push("  (empty)".to_string());
        return lines.join("\n");
    }
    lines.extend(
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| format!("  {}. {}", index + 1, preview(entry, 100))),
    );
    lines.join("\n")
}

fn render_preview(store: &HermesMemoryStore) -> Result<String, crate::store::StoreError> {
    let memory = store.legacy_global_context();
    Ok(format!(
        "Frozen memory context for this session (writes become visible next session):\n\n{memory}"
    ))
}

fn render_skills_list(
    skills: &[crate::skills::SkillDocument],
    project_name: Option<&str>,
    external: &[(String, String)],
) -> String {
    let mut lines = vec![
        String::new(),
        "  ╔═══════════════════════════════════════════════════════════╗".to_string(),
        "  ║                    🧠 Procedural Skills                  ║".to_string(),
        "  ╚═══════════════════════════════════════════════════════════╝".to_string(),
        "  Legend: [G] global · [P] project · [E] external (read-only)".to_string(),
        String::new(),
    ];
    if skills.is_empty() && external.is_empty() {
        lines.extend([
            "  (no skills found in this session)".to_string(),
            String::new(),
            "  Ask the agent to save a reusable procedure".to_string(),
            "  with the skill_manage tool when it is worth keeping.".to_string(),
        ]);
        return lines.join("\n");
    }
    for (scope, heading, marker) in [
        (crate::skills::SkillScope::Global, "Global Skills", "G"),
        (crate::skills::SkillScope::Project, "Project Skills", "P"),
    ] {
        let scoped = skills
            .iter()
            .filter(|skill| skill.scope == scope)
            .collect::<Vec<_>>();
        if scoped.is_empty() {
            continue;
        }
        lines.push(format!("  [{marker}] {heading}"));
        if scope == crate::skills::SkillScope::Project
            && let Some(project_name) = project_name
        {
            lines.push(format!("      Active project: {project_name}"));
        }
        lines.push("  ─────────────────".to_string());
        for skill in scoped {
            let display_name = skill.display_name.as_deref().unwrap_or(&skill.name);
            lines.push(format!(
                "  📄 {display_name} ({})",
                display_path(&skill.path)
            ));
            lines.push(format!(
                "     {}",
                if skill.description.is_empty() {
                    "(no description)"
                } else {
                    &skill.description
                }
            ));
            lines.push(format!("     id: {}", skill.id));
            lines.push(String::new());
        }
    }
    if !external.is_empty() {
        lines.extend([
            "  [E] External Skills (read-only)".to_string(),
            "  ───────────────────────────────".to_string(),
        ]);
        for (name, description) in external {
            lines.push(format!("  📄 {name} (loaded skill command)"));
            lines.push(format!(
                "     {}",
                if description.is_empty() {
                    "(no description)"
                } else {
                    description
                }
            ));
            lines.push(format!("     id: external:{name}"));
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

async fn manage_skills(
    context: &CommandContext,
    store: &HermesMemoryStore,
) -> Result<(), CommandError> {
    #[derive(Clone)]
    struct Row {
        id: String,
        scope: Option<crate::skills::SkillScope>,
        option: UiMultiSelectOption,
    }

    let mut retained_ids = HashSet::<String>::new();
    let mut retained_query = String::new();
    let mut retained_categories = Vec::new();
    let mut retained_sort_mode = 0usize;
    let mut summary_lines = vec![
        "Select skills with space, then move with g/p or delete with d. Press s to change sort and f for filters."
            .to_string(),
    ];
    loop {
        context
            .signal()
            .check()
            .map_err(|_| CommandError::Aborted)?;
        let skills = store.list_skills().map_err(execution)?;
        let managed_names = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let external = context
            .session
            .commands()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|command| {
                let name = command.name.strip_prefix("skill:")?.to_string();
                (!managed_names.contains(name.as_str())).then_some((name, command.description))
            })
            .collect::<Vec<_>>();

        if !context.ui.is_available().unwrap_or(false) {
            notify(
                context,
                render_skills_list(&skills, store.current_project_name().as_deref(), &external),
            )?;
            return Ok(());
        }

        let mut rows = skills
            .iter()
            .map(|skill| {
                let marker = if skill.scope == crate::skills::SkillScope::Global {
                    "G"
                } else {
                    "P"
                };
                let label = skill.display_name.as_deref().unwrap_or(&skill.name);
                let shown_path = display_path(&skill.path);
                let recency = if skill.updated.is_empty() {
                    &skill.created
                } else {
                    &skill.updated
                };
                Row {
                    id: skill.id.clone(),
                    scope: Some(skill.scope),
                    option: UiMultiSelectOption {
                        label: format!("[{marker}] {label} ({shown_path})"),
                        search_text: format!(
                            "{label} {} {} {} {shown_path}",
                            skill.name,
                            skill.description,
                            skill.path.display()
                        ),
                        category: Some(marker.to_string()),
                        detail_lines: vec![
                            skill.description.clone(),
                            skill.id.clone(),
                            shown_path.clone(),
                        ],
                        read_only: false,
                        sort_values: vec![
                            vec![recency.clone(), skill.created.clone()],
                            vec![skill.created.clone(), recency.clone()],
                            vec![label.to_lowercase()],
                        ],
                    },
                }
            })
            .collect::<Vec<_>>();
        rows.extend(external.iter().map(|(name, description)| Row {
            id: format!("external:{name}"),
            scope: None,
            option: UiMultiSelectOption {
                label: format!("[E] {name} (loaded skill command)"),
                search_text: format!("{name} {description} loaded skill command"),
                category: Some("E".to_string()),
                detail_lines: vec![
                    description.clone(),
                    format!("external:{name}"),
                    "loaded skill command".to_string(),
                ],
                read_only: true,
                sort_values: vec![Vec::new(), Vec::new(), vec![name.to_lowercase()]],
            },
        }));
        let initially_selected = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| retained_ids.contains(&row.id).then_some(index))
            .collect();
        let response = match context
            .ui
            .multi_select(UiMultiSelectRequest {
                title: "Pi Hermes Memory — Procedural Skills".to_string(),
                options: rows.iter().map(|row| row.option.clone()).collect(),
                actions: vec![
                    UiMultiSelectAction {
                        id: "move-global".to_string(),
                        key: 'g',
                        label: "global".to_string(),
                        enabled: true,
                        confirmation: None,
                    },
                    UiMultiSelectAction {
                        id: "move-project".to_string(),
                        key: 'p',
                        label: "project".to_string(),
                        enabled: store.current_project_name().is_some(),
                        confirmation: None,
                    },
                    UiMultiSelectAction {
                        id: "delete".to_string(),
                        key: 'd',
                        label: "delete".to_string(),
                        enabled: true,
                        confirmation: Some(
                            "Delete {count} selected skill{plural}? This cannot be undone. Press y to confirm or n to cancel.{read_only_note}"
                                .to_string(),
                        ),
                    },
                ],
                categories: vec![
                    ("G".to_string(), "Global [G]".to_string()),
                    ("P".to_string(), "Project [P]".to_string()),
                    ("E".to_string(), "External [E] (read-only)".to_string()),
                ],
                sort_modes: vec![
                    ("Updated".to_string(), true),
                    ("Created".to_string(), true),
                    ("Name".to_string(), false),
                ],
                initially_selected,
                initial_query: retained_query.clone(),
                initial_active_categories: retained_categories.clone(),
                initial_sort_mode: retained_sort_mode,
                summary_lines: summary_lines.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(_) => {
                let latest_skills = store.list_skills().map_err(execution)?;
                let latest_names = latest_skills
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .collect::<HashSet<_>>();
                let latest_external = context
                    .session
                    .commands()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|command| {
                        let name = command.name.strip_prefix("skill:")?.to_string();
                        (!latest_names.contains(name.as_str()))
                            .then_some((name, command.description))
                    })
                    .collect::<Vec<_>>();
                notify_level(
                    context,
                    NoticeLevel::Warning,
                    "Interactive skills manager unavailable in this runtime; showing read-only list fallback.",
                )?;
                notify(
                    context,
                    render_skills_list(
                        &latest_skills,
                        store.current_project_name().as_deref(),
                        &latest_external,
                    ),
                )?;
                return Ok(());
            }
        };
        let Some(response) = response else {
            return Ok(());
        };
        retained_query = response.query.clone();
        retained_categories = response.active_categories.clone();
        retained_sort_mode = response.sort_mode;
        let selected_rows = response
            .selected
            .into_iter()
            .filter_map(|index| rows.get(index))
            .collect::<Vec<_>>();
        let mutable = selected_rows
            .iter()
            .filter(|row| row.scope.is_some())
            .copied()
            .collect::<Vec<_>>();
        let external_ids = selected_rows
            .iter()
            .filter(|row| row.scope.is_none())
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        retained_ids.clear();

        match response.action_id.as_str() {
            "move-global" | "move-project" => {
                let target = if response.action_id == "move-global" {
                    crate::skills::SkillScope::Global
                } else {
                    crate::skills::SkillScope::Project
                };
                let mut moved = 0usize;
                let mut unchanged = 0usize;
                let mut blocked = Vec::new();
                for row in mutable {
                    if row.scope == Some(target) {
                        unchanged += 1;
                        continue;
                    }
                    match store.move_skill(&row.id, target) {
                        Ok(_) => moved += 1,
                        Err(error) => {
                            retained_ids.insert(row.id.clone());
                            blocked.push((row.id.clone(), error.to_string()));
                        }
                    }
                }
                for id in external_ids {
                    retained_ids.insert(id.clone());
                    blocked.push((id, "external skills are read-only".to_string()));
                }
                summary_lines = vec![format!(
                    "Moved {moved} skill{} to {}.",
                    if moved == 1 { "" } else { "s" },
                    target.as_str()
                )];
                if unchanged > 0 {
                    summary_lines.push(format!("{unchanged} already matched the target scope."));
                }
                append_blocked_summary(&mut summary_lines, &blocked);
            }
            "delete" => {
                let mutable_ids = mutable.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
                let blocked_external_count = external_ids.len();
                if mutable_ids.is_empty() {
                    retained_ids.extend(external_ids);
                    summary_lines = vec![format!(
                        "Blocked {blocked_external_count} external skill{}: read-only (delete unavailable).",
                        if blocked_external_count == 1 { "" } else { "s" }
                    )];
                    continue;
                }
                let mut deleted = 0usize;
                let mut blocked = Vec::new();
                for id in mutable_ids {
                    match store.delete_skill(&id) {
                        Ok(_) => deleted += 1,
                        Err(error) => {
                            retained_ids.insert(id.clone());
                            blocked.push((id, error.to_string()));
                        }
                    }
                }
                for id in external_ids {
                    retained_ids.insert(id.clone());
                    blocked.push((id, "external skills are read-only".to_string()));
                }
                summary_lines = vec![format!(
                    "Deleted {deleted} skill{}.",
                    if deleted == 1 { "" } else { "s" }
                )];
                append_blocked_summary(&mut summary_lines, &blocked);
            }
            _ => {}
        }
    }
}

fn append_blocked_summary(summary: &mut Vec<String>, blocked: &[(String, String)]) {
    if blocked.is_empty() {
        return;
    }
    summary.push(format!(
        "Blocked {} skill{}:",
        blocked.len(),
        if blocked.len() == 1 { "" } else { "s" }
    ));
    summary.extend(
        blocked
            .iter()
            .take(4)
            .map(|(id, error)| format!("- {id}: {error}")),
    );
    if blocked.len() > 4 {
        summary.push(format!("- …and {} more", blocked.len() - 4));
    }
}

fn display_path(path: &std::path::Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return path.display().to_string();
    };
    match path.strip_prefix(&home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

async fn learn(context: &CommandContext) -> Result<(), CommandError> {
    if !context.ui.is_available().unwrap_or(false) {
        notify(context, LEARN_GUIDE)?;
        return Ok(());
    }
    let options = LEARN_SECTION_LABELS
        .iter()
        .map(ToString::to_string)
        .collect();
    if let Some(index) = context.ui.select("Pi Hermes Memory Guide", options).await?
        && let Some(section) = LEARN_SECTIONS.get(index)
    {
        notify(context, *section)?;
    }
    Ok(())
}

fn pin(
    context: &CommandContext,
    store: &HermesMemoryStore,
    arguments: &str,
) -> Result<(), CommandError> {
    let standing = store
        .standing()
        .ok_or_else(|| execution("standing instructions are disabled"))?;
    let mut words = arguments.split_whitespace();
    let head = words.next().unwrap_or("list").to_ascii_lowercase();
    match head.as_str() {
        "list" => notify(context, standing_list(standing)),
        "clear" => match standing.clear() {
            Ok(message) => notify(context, format!("📌 {message}")),
            Err(error) => notify_level(context, NoticeLevel::Warning, format!("❌ {error}")),
        },
        "remove" => {
            let Some(position) = words.next().and_then(|value| value.parse::<usize>().ok()) else {
                return notify_level(
                    context,
                    NoticeLevel::Warning,
                    "❌ Position must be a valid instruction number.",
                );
            };
            match standing.remove(position) {
                Ok(message) => notify(
                    context,
                    format!("📌 {message}\n\n{}", standing_list(standing)),
                ),
                Err(error) => notify_level(context, NoticeLevel::Warning, format!("❌ {error}")),
            }
        }
        _ => match standing.add(arguments) {
            Ok(message) => notify(
                context,
                format!(
                    "📌 {message}\n\n  This is now injected into every session, in all memory modes.\n  It takes effect from your next message.\n"
                ),
            ),
            Err(error) => notify_level(context, NoticeLevel::Warning, format!("❌ {error}")),
        },
    }
}

fn standing_list(standing: &crate::standing::StandingInstructions) -> String {
    let entries = standing.list();
    let mut lines = vec![
        String::new(),
        "  ╔══════════════════════════════════════════════╗".to_string(),
        "  ║          📌 Standing Instructions            ║".to_string(),
        "  ╚══════════════════════════════════════════════╝".to_string(),
        String::new(),
    ];
    if entries.is_empty() {
        lines.extend([
            "  (none pinned)".to_string(),
            String::new(),
            "  Pin a rule that must hold in every session:".to_string(),
            "    /memory-pin never run find / or other root-wide searches".to_string(),
            String::new(),
        ]);
        return lines.join("\n");
    }
    let render = standing.render();
    lines.extend(
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| format!("  {}. {entry}", index + 1)),
    );
    lines.extend([
        String::new(),
        format!(
            "  {}/{} entries · {}/{} chars",
            entries.len(),
            STANDING_MAX_ENTRIES,
            char_len(&entries.join("\n")),
            STANDING_MAX_CHARS,
        ),
        format!("  Injected into every session: {}", render.injected_count),
    ]);
    if render.omitted_count > 0 {
        lines.push(format!(
            "  ⚠️ {} over budget and NOT injected — remove or shorten entries.",
            render.omitted_count
        ));
    }
    lines.extend([
        format!("  File: {}", standing.path().display()),
        String::new(),
        "  /memory-pin remove <n> · /memory-pin clear".to_string(),
        String::new(),
    ]);
    lines.join("\n")
}

fn preview(value: &str, limit: usize) -> String {
    if char_len(value) <= limit {
        value.to_string()
    } else {
        format!("{}...", char_prefix(value, limit))
    }
}

fn notify(context: &CommandContext, message: impl Into<String>) -> Result<(), CommandError> {
    notify_level(context, NoticeLevel::Info, message)
}

fn notify_level(
    context: &CommandContext,
    level: NoticeLevel,
    message: impl Into<String>,
) -> Result<(), CommandError> {
    context.ui.notify(level, message.into())?;
    Ok(())
}

fn execution(error: impl std::fmt::Display) -> CommandError {
    CommandError::Execution(error.to_string())
}

const INTERVIEW_PROMPT: &str = r#"You are conducting a brief onboarding interview with a new user. Your goal is to pre-fill their USER PROFILE so future sessions start with context instead of a blank slate.

Ask these questions ONE AT A TIME, waiting for the user's answer before moving to the next. Be conversational and adapt follow-ups based on their answers — don't firehose all questions at once.

1. What should I call you? (name or nickname)
2. What timezone are you in?
3. What programming languages and tools do you use most?
4. What's your preferred editor or IDE?
5. How do you like me to communicate? (concise vs detailed, show code vs explain, etc.)
6. Anything about your work style I should know? (action-first vs plan-first, specific workflows, pet peeves)
7. Is there anything else you want me to always remember?

After EACH answer, immediately save it to the 'user' target using memory(action='add'). If you're updating something they already told you, use memory(action='replace').

If the user already has entries in their USER PROFILE, acknowledge them and ask whether they'd like to update, add to, or skip the existing profile before starting the questions.

Keep it light. This should feel like a friendly chat, not a form."#;

const LEARN_GUIDE: &str = r#"Pi Hermes Memory

Stores durable facts in MEMORY.md, USER.md, failures.md, and per-project MEMORY.md files. The SQLite database mirrors those files and indexes prior sessions for memory_search and session_search.

Tools: memory(action='add'), memory(action='replace'), memory(action='remove'), memory_search, session_search, skill_manage.

Commands: /memory-insights, /memory-skills, /memory-consolidate, /memory-interview, /memory-switch-project, /memory-index-sessions, /memory-sync-markdown, /memory-preview-context, /memory-pin.

Save stable preferences, environment facts, corrections, conventions, failures, and reusable procedures. Do not save task progress, session outcomes, secrets, or temporary state."#;

const LEARN_SECTION_LABELS: [&str; 7] = [
    "📦 What Gets Saved",
    "🔧 Tools Available",
    "📋 Commands",
    "✅ Best Practices",
    "🔄 How Memory Flows",
    "🏗️ Architecture",
    "❓ Troubleshooting",
];

const LEARN_SECTIONS: [&str; 7] = [
    r#"
  ╔══════════════════════════════════════════════╗
  ║           📦 What Gets Saved                 ║
  ╚══════════════════════════════════════════════╝

  Type            │ File          │ Limit
  ────────────────┼───────────────┼────────────
  🧠 Memory       │ MEMORY.md     │ 2,200 chars
  👤 User Profile │ USER.md       │ 1,375 chars
  ⚠️  Failures     │ failures.md   │ 10,000 chars
  📚 Skills       │ Pi-native skill dirs │ Unlimited
  💾 Extended     │ sessions.db   │ Unlimited

  Memory:   Facts — env details, project conventions, tool quirks
  User:     Who you are — name, preferences, communication style
  Failures: What didn't work — corrections, failures, insights
  Skills:   Procedures — how to debug, deploy, test
  Extended: SQLite search mirror for Markdown memory + backfill

  Memory Categories:
  ─────────────────
  [failure]      What was tried but didn't work
  [correction]   User corrected the agent
  [insight]      Learning from experience
  [preference]   User preference
  [convention]   Project convention
  [tool-quirk]   Tool-specific knowledge"#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║           🔧 Tools Available                 ║
  ╚══════════════════════════════════════════════╝

  memory (add/replace/remove)
    Save, update, or delete memories
    Targets: memory, user

  skill_manage (create/view/patch/update/delete)
    Save reusable procedures

  session_search
    Search past conversations across all sessions

  memory_search
    Search the SQLite-backed memory mirror/store
    Filters: project, target, category
    Categories: failure, correction, insight, preference, convention, tool-quirk"#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║             📋 Commands                      ║
  ╚══════════════════════════════════════════════╝

  /memory-insights      Show everything stored in memory
  /memory-skills        List all saved skills
  /memory-consolidate   Manually trigger memory cleanup
  /memory-interview     Answer questions to pre-fill profile
  /memory-switch-project List all project memories
  /memory-index-sessions Import past sessions for search
  /memory-sync-markdown Backfill Markdown memories into SQLite
  /memory-preview-context Show this session's frozen MEMORY/USER prompt"#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║           ✅ Best Practices                  ║
  ╚══════════════════════════════════════════════╝

  ✅ DO save:
     • User preferences ("prefers pnpm", "uses vim")
     • Environment facts ("macOS M1", "Node 20")
     • Corrections ("don't use npm — use pnpm")
     • Project conventions ("monorepo with turborepo")
     • Failures ("tried localStorage — XSS vulnerability")

  ❌ DON'T save:
     • Task progress ("finished implementing auth")
     • Session outcomes ("PR #42 was merged")
     • Temporary state ("currently debugging X")"#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║          🔄 How Memory Flows                 ║
  ╚══════════════════════════════════════════════╝

  1. Session starts      → Frozen MEMORY.md and USER.md are injected
  2. During conversation → Agent may read, search, and save useful facts
  3. Agent saves         → Atomic Markdown + derived SQLite search
  4. Every 10 requests   → Memory review becomes due
  5. Every 10 iterations → Skill review becomes due (skill_manage resets it)
  6. Request settles     → One private Agent performs due review
  7. When full           → Current Agent consolidates and retries
  8. New request/close   → Cancel background review

  No correction-regex writes or automatic final flush. Review can
  create skills and update agent-owned, unpinned skills after reading."#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║          🏗️ Two-Tier Architecture            ║
  ╚══════════════════════════════════════════════╝

  Frozen prompt: MEMORY.md and USER.md, refreshed for a new session.
  Search: memory_search and session_search retain historical data.
  Skill catalog: Pi-native SKILL.md documents; reload after mutation.
  Review: one in-process Agent fork, no child CLI or resume entry."#,
    r#"
  ╔══════════════════════════════════════════════╗
  ║          ❓ Troubleshooting                  ║
  ╚══════════════════════════════════════════════╝

  "Memory is full"
    → /memory-consolidate to merge entries
    → If it still fails, the save does NOT silently become SQLite-only

  "Can't find something"
    → memory_search to search the SQLite mirror/store
    → /memory-sync-markdown to import older Markdown entries

  "Agent forgot something"
    → Check /memory-insights, tell agent "remember X"

  "Want to edit manually"
    → Files at ~/.pi/agent/memory/ (plain markdown)"#,
];
