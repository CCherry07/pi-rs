//! Local MCP product plugin and management command; explicit client pools stay tool-only.
mod manager;

use crate::{McpExposure, McpLibrary, McpScope, McpServerRow, McpServerState, McpToolSet};
use async_trait::async_trait;
use pi_core::PluginId;
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Command, CommandContext,
    CommandError, CommandOutcome, CommandSpec, NoticeLevel, Plugin, PluginError, RegisterContext,
};
use std::sync::Arc;

pub(crate) struct McpProductPlugin {
    pub(crate) library: McpLibrary,
    pub(crate) rows: Vec<McpServerRow>,
    pub(crate) pool: McpToolSet,
    pub(crate) auto_enable_codemode: bool,
    pub(crate) needs_codemode: bool,
    pub(crate) needs_tool_search: bool,
}
#[pi_plugin::plugin]
impl Plugin for McpProductPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("mcp")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        self.pool
            .plugin_with_tool_search_registration(self.library.register_tool_search)
            .register(context)?;
        let command = Arc::new(McpCommand {
            library: self.library.clone(),
            rows: self.rows.clone(),
            pool: self.pool.clone(),
        });
        let completions = command.argument_completions();
        context.register_command_with_completions(command, completions)
    }

    async fn session_start(
        &self,
        context: &pi_plugin::SessionPluginContext,
        _event: &pi_plugin::SessionStartEvent,
    ) -> Result<(), PluginError> {
        // Publication is irrevocable before any fallible UI/activation callback.
        self.library.publish_generation();
        if let Some(diagnostic) = self.library.selection_diagnostic() {
            context.ui.notify(NoticeLevel::Warning, diagnostic)?;
        }
        for name in self.library.pending_direct_servers() {
            context.ui.notify(NoticeLevel::Info, format!("MCP server {name:?} is still connecting; its direct tools are unavailable in this generation and can appear after the next idle refresh."))?;
        }
        for pending in self.pool.authorization_required() {
            context
                .ui
                .notify(NoticeLevel::Warning, pending.to_string())?;
        }
        if !self.needs_codemode && !self.needs_tool_search {
            return Ok(());
        }
        let has_codemode = context.session.tool_reaches_tools("codemode")?;
        let has_search = context.session.tool_reaches_tools("tool_search")?;
        let mut active = context.session.active_tools()?;
        let before = active.len();
        for (name, activate) in [
            (
                "codemode",
                self.needs_codemode && self.auto_enable_codemode && has_codemode,
            ),
            ("tool_search", self.needs_tool_search && has_search),
        ] {
            if activate && !active.iter().any(|tool| tool == name) {
                active.push(name.into());
            }
        }
        if active.len() != before {
            // This getter is effective-only. Merge live desired absent intent only
            // when adding a helper, so a no-op startup never writes selection and
            // an earlier hook's explicit deselection cannot be resurrected.
            self.library
                .merge_preserved_tool_selection(&mut active)
                .map_err(PluginError::Failure)?;
            context.session.set_active_tools(active.clone())?;
        }
        if !(has_codemode && active.iter().any(|name| name == "codemode")
            || has_search && active.iter().any(|name| name == "tool_search"))
        {
            let reason = if self.needs_codemode && has_codemode && !self.auto_enable_codemode {
                " (autoEnableCodemode is false)"
            } else {
                ""
            };
            context.ui.notify(NoticeLevel::Warning, format!(
                "MCP tools are only reachable from the codemode or tool_search tool, but neither is active{reason}; they cannot be called."
            ))?;
        }
        Ok(())
    }

    async fn session_shutdown(
        &self,
        _context: &pi_plugin::SessionPluginContext,
        event: &pi_plugin::SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.library
            .shutdown_generation(event.reason == pi_plugin::SessionShutdownReason::Reload)
            .await
            .map_err(|error| PluginError::Failure(error.to_string()))
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        Ok(match self.library.supervised_directory() {
            Some(section) => crate::directory::prompt_patch(event, Some(section)),
            None => self.pool.prompt_patch(event),
        })
    }
}
struct McpCommand {
    library: McpLibrary,
    rows: Vec<McpServerRow>,
    pool: McpToolSet,
}

struct SemanticOAuthPrompt(pi_plugin::UiContext);
#[async_trait]
impl crate::McpOAuthPrompt for SemanticOAuthPrompt {
    fn show_authorization_url(&self, url: &str) -> Result<(), String> {
        self.0
            .notify(
                NoticeLevel::Info,
                format!("Authorize MCP access in your browser:\n{url}"),
            )
            .map_err(|error| error.to_string())
    }
    fn notify(&self, message: &str) -> Result<(), String> {
        self.0
            .notify(NoticeLevel::Info, message)
            .map_err(|error| error.to_string())
    }
    async fn redirect_url(&self, signal: pi_core::AbortSignal) -> Result<Option<String>, String> {
        tokio::select! {
            biased;
            () = signal.wait() => Ok(None),
            result = self.0.input("Waiting for OAuth callback — or paste the complete redirect URL", "http://127.0.0.1/...?...&state=...") => result.map_err(|error| error.to_string()),
        }
    }
}

impl McpCommand {
    fn server_state(&self, row: &McpServerRow) -> McpServerState {
        if !row.enabled {
            McpServerState::Disabled
        } else if let Some(readiness) = self.library.snapshot_readiness(&row.name) {
            if matches!(readiness, crate::McpServerReadiness::Ready)
                && self.pool.connection_error(&row.name).is_some()
            {
                McpServerState::Disconnected
            } else {
                readiness.state()
            }
        } else if self
            .pool
            .authorization_required()
            .iter()
            .any(|required| required.server == row.name)
        {
            McpServerState::NeedsAuth
        } else if self.pool.connection_error(&row.name).is_some() {
            McpServerState::Disconnected
        } else {
            McpServerState::Connected
        }
    }
    fn connection_diagnostic(&self, name: &str) -> Option<String> {
        self.pool.connection_error(name).or_else(|| {
            self.library
                .snapshot_readiness(name)
                .and_then(crate::McpServerReadiness::diagnostic)
        })
    }

    async fn test_connection(
        &self,
        context: &CommandContext,
        name: &str,
    ) -> Result<Vec<String>, CommandError> {
        if name.is_empty() {
            return Err(CommandError::InvalidArguments(mcp_usage()));
        }
        tokio::select! {
            result = self.library.test(name) => result.map_err(CommandError::Execution),
            () = context.signal().wait() => Err(CommandError::Aborted),
        }
    }

    async fn reconnect_and_reload(
        &self,
        context: &CommandContext,
        name: &str,
    ) -> Result<(), CommandError> {
        if let Some(result) = self.library.reconnect_active(name) {
            result.map_err(CommandError::Execution)?;
            // The only probe is the supervised attempt; refresh the whole frozen generation.
            return self
                .reload_after_change(context, format!("Reconnecting MCP server {name:?}"))
                .await;
        }
        let names = self.test_connection(context, name).await?;
        self.reload_after_change(
            context,
            format!("Reconnected to MCP server {name:?} ({} tools)", names.len()),
        )
        .await
    }

    async fn reload_after_change(
        &self,
        context: &CommandContext,
        message: String,
    ) -> Result<(), CommandError> {
        // Reload prepares and validates the complete next generation before publication. A failed
        // candidate leaves this command's current generation active.
        let replacement = context.session.reload().await.map_err(|error| {
            CommandError::Execution(format!(
                "{error}; the previous generation remains active. Configuration or credential changes remain on disk; retry /mcp reload or revert them."
            ))
        })?;
        replacement.ui.notify(NoticeLevel::Info, message)?;
        Ok(())
    }

    async fn login(
        &self,
        context: &CommandContext,
        name: &str,
        scope: Option<McpScope>,
    ) -> Result<(), CommandError> {
        if !context.ui.is_available()? {
            return Err(CommandError::Execution(
                "MCP login requires an interactive callback prompt; use pi mcp login in a terminal"
                    .into(),
            ));
        }
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            self.library.login(
                name,
                scope,
                Arc::new(SemanticOAuthPrompt(context.ui.clone())),
                context.signal().clone(),
            ),
        )
        .await
        .map_err(|_| CommandError::Execution("MCP login timed out; retry /mcp login".into()))?
        .map_err(|error| match error {
            crate::McpOAuthError::Cancelled => CommandError::Aborted,
            error => CommandError::Execution(error.to_string()),
        })?;
        self.reload_after_change(
            context,
            format!("Signed in to MCP server {name:?} ({count} tools)."),
        )
        .await
    }
}

fn tool_menu_options(tools: &[crate::client::McpOfferedTool], default: McpExposure) -> Vec<String> {
    tools
        .iter()
        .map(|tool| {
            let mut line = tool.name.clone();
            if tool.exposure != default {
                line.push_str(&format!(" [{}]", tool.exposure.as_str()));
            }
            let description = tool.description.lines().next().unwrap_or_default();
            if !description.is_empty() {
                line.push_str(&format!(" — {description}"));
            }
            line
        })
        .collect()
}

fn parse_scoped_name(arguments: &str) -> Result<(&str, McpScope), CommandError> {
    let mut parts = arguments.split_whitespace();
    let name = parts.next().unwrap_or_default();
    let scope = match (parts.next(), parts.next()) {
        (None, None) => McpScope::Global,
        (Some("--local" | "-l"), None) => McpScope::Project,
        _ => return Err(CommandError::InvalidArguments(mcp_usage())),
    };
    if name.is_empty() {
        return Err(CommandError::InvalidArguments(mcp_usage()));
    }
    Ok((name, scope))
}

fn scope_label(scope: McpScope) -> &'static str {
    match scope {
        McpScope::Global => "global",
        McpScope::Project => "project",
    }
}

fn mcp_usage() -> String {
    "Use /mcp [status|paths|login [name]|logout [name]|reconnect [name]|enable <name> [--local]|disable <name> [--local]|exposure <name> <direct|codemode|deferred|hidden> [--local]|reload]".into()
}

#[async_trait]
impl Command for McpCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "mcp".into(),
            description: "Manage MCP servers: inspect, reconnect, enable or disable, and change exposure".into(),
            argument_hint: Some("[status|paths|login [name]|logout [name]|reconnect [name]|enable <name>|disable <name>|exposure <name> <mode>|reload]".into()),
        }
    }
    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        if context.signal().is_aborted() {
            return Err(CommandError::Aborted);
        }
        let arguments = arguments.trim();
        if arguments.is_empty() && context.ui.is_available().unwrap_or(false) {
            let signal = context.signal().clone();
            return tokio::select! {
                biased;
                () = signal.wait() => Err(CommandError::Aborted),
                result = self.manage(context) => result,
            };
        }
        if matches!(arguments, "login" | "logout" | "reconnect") {
            if let Some(row) = self.pick_server(&context, arguments != "reconnect").await? {
                match arguments {
                    "login" => self.login(&context, &row.name, None).await?,
                    "logout" => {
                        let message = self
                            .library
                            .logout(&row.name, None)
                            .map_err(CommandError::Execution)?;
                        self.reload_after_change(&context, message).await?;
                    }
                    _ => self.reconnect_and_reload(&context, &row.name).await?,
                }
            }
            return Ok(CommandOutcome::Handled);
        }
        let output = match arguments {
            "reload" => {
                let replacement = context.session.reload().await?;
                replacement
                    .ui
                    .notify(NoticeLevel::Info, "MCP configuration reloaded")?;
                return Ok(CommandOutcome::Handled);
            }
            "paths" => format!(
                "Global: {}\nProject: {}\nEdit mcp.json, then /mcp reload. Project entries replace global entries by name.",
                self.library
                    .path(McpScope::Global)
                    .map_err(CommandError::Execution)?
                    .display(),
                self.library
                    .path(McpScope::Project)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| e)
            ),
            "" | "status" => {
                let rows = self
                    .rows
                    .iter()
                    .map(|row| {
                        format!(
                            "{} · {} · {:?} · {}",
                            row.name,
                            row.transport,
                            row.scope,
                            if row.enabled {
                                let (direct, deferred, codemode) = self.pool.tool_exposure_counts(&row.name);
                                format!(
                                    "{} · {direct} direct tools · {deferred} deferred tools · {codemode} codemode tools{}{}",
                                    self.server_state(row).as_str(),
                                    if self.pool.has_resources(&row.name) {
                                        " · resources available"
                                    } else {
                                        ""
                                    },
                                    self.connection_diagnostic(&row.name)
                                        .map(|error| format!("\n{error}"))
                                        .unwrap_or_default()
                                )
                            } else {
                                "disabled".into()
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "MCP generation snapshot\n{}\n/mcp paths · /mcp test <name> · /mcp reload",
                    if rows.is_empty() {
                        "No configured servers"
                    } else {
                        &rows
                    }
                )
            }
            _ if arguments.starts_with("test ") => {
                let names = self
                    .test_connection(&context, arguments[5..].trim())
                    .await?;
                format!(
                    "MCP connection succeeded: {} tools\n{}",
                    names.len(),
                    names.join("\n")
                )
            }
            _ if arguments.starts_with("reconnect ") => {
                let name = arguments[10..].trim();
                self.eligible_server(name, false)?;
                self.reconnect_and_reload(&context, name).await?;
                return Ok(CommandOutcome::Handled);
            }
            _ if arguments.starts_with("enable ") || arguments.starts_with("disable ") => {
                let enabled = arguments.starts_with("enable ");
                let rest = arguments
                    .strip_prefix(if enabled { "enable " } else { "disable " })
                    .unwrap_or_default();
                let (name, scope) = parse_scoped_name(rest)?;
                self.library
                    .set_enabled(scope, name, enabled)
                    .map_err(CommandError::Execution)?;
                self.reload_after_change(
                    &context,
                    format!(
                        "{} MCP server {name:?} in {} configuration",
                        if enabled { "Enabled" } else { "Disabled" },
                        scope_label(scope)
                    ),
                )
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            _ if arguments.starts_with("exposure ") => {
                let mut parts = arguments[9..].split_whitespace();
                let name = parts.next().unwrap_or_default();
                let exposure = parts
                    .next()
                    .ok_or_else(|| CommandError::InvalidArguments(mcp_usage()))?
                    .parse::<McpExposure>()
                    .map_err(CommandError::InvalidArguments)?;
                let scope = match (parts.next(), parts.next()) {
                    (None, None) => McpScope::Global,
                    (Some("--local" | "-l"), None) => McpScope::Project,
                    _ => return Err(CommandError::InvalidArguments(mcp_usage())),
                };
                if name.is_empty() {
                    return Err(CommandError::InvalidArguments(mcp_usage()));
                }
                self.library
                    .set_exposure(scope, name, exposure)
                    .map_err(CommandError::Execution)?;
                self.reload_after_change(
                    &context,
                    format!(
                        "Set MCP server {name:?} exposure to {} in {} configuration",
                        exposure.as_str(),
                        scope_label(scope)
                    ),
                )
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            _ if arguments.starts_with("login ") || arguments.starts_with("logout ") => {
                let login = arguments.starts_with("login ");
                let rest = arguments
                    .strip_prefix(if login { "login " } else { "logout " })
                    .unwrap_or_default();
                let (name, scope) = parse_scoped_name(rest)?;
                self.eligible_server(name, true)?;
                let scope = (scope == McpScope::Project).then_some(scope);
                if login {
                    self.login(&context, name, scope).await?;
                } else {
                    let message = self
                        .library
                        .logout(name, scope)
                        .map_err(CommandError::Execution)?;
                    self.reload_after_change(&context, message).await?;
                }
                return Ok(CommandOutcome::Handled);
            }
            _ => return Err(CommandError::InvalidArguments(mcp_usage())),
        };
        context.ui.notify(NoticeLevel::Info, output)?;
        Ok(CommandOutcome::Handled)
    }
}

#[cfg(test)]
mod tests {
    mod auth;
    mod diagnostics;
    mod inventory;
    mod management;
    use super::*;
    use pi_plugin::{
        ModelsContextAccess, PluginContextEpoch, PluginContextResult, SessionContextAccess,
        SessionExecutionOrigin, SessionIdentity, SessionPluginContext, SessionStartEvent,
        SessionStartReason, UiContextAccess,
    };
    use std::sync::Mutex;

    struct ManagerAccess {
        answers: Mutex<std::collections::VecDeque<Option<usize>>>,
        dialogs: Mutex<Vec<pi_plugin::UiDialogRequest>>,
        notices: Mutex<Vec<String>>,
        old: Mutex<Option<PluginContextEpoch>>,
        next: Option<PluginContextEpoch>,
        reloads: std::sync::atomic::AtomicUsize,
    }

    impl ManagerAccess {
        fn new(answers: Vec<Option<usize>>, next: Option<PluginContextEpoch>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                dialogs: Mutex::new(vec![]),
                notices: Mutex::new(vec![]),
                old: Mutex::new(None),
                next,
                reloads: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }
    impl ModelsContextAccess for ManagerAccess {}
    #[async_trait]
    impl UiContextAccess for ManagerAccess {
        fn has_ui(&self) -> PluginContextResult<bool> {
            Ok(true)
        }
        fn ui_notify(&self, _: NoticeLevel, message: String) -> PluginContextResult<()> {
            self.notices.lock().unwrap().push(message);
            Ok(())
        }
        async fn ui_dialog(
            &self,
            request: pi_plugin::UiDialogRequest,
            _: Option<u64>,
        ) -> PluginContextResult<Option<pi_plugin::UiDialogResponse>> {
            let pi_plugin::UiDialogRequest::Select { options, .. } = &request else {
                panic!("expected selector")
            };
            let answer = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("expected dialog")
                .map(|index| pi_plugin::UiDialogResponse::Value(options[index].clone()))
                .unwrap_or(pi_plugin::UiDialogResponse::Cancelled);
            self.dialogs.lock().unwrap().push(request);
            Ok(Some(answer))
        }
    }
    #[async_trait]
    impl SessionContextAccess for ManagerAccess {
        async fn reload(
            &self,
            scope: pi_plugin::PluginContextScope,
        ) -> PluginContextResult<pi_plugin::PluginContextReplacement> {
            self.reloads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self
                .next
                .as_ref()
                .ok_or_else(|| pi_plugin::PluginContextError::Failed("candidate failed".into()))?;
            self.old.lock().unwrap().take().unwrap().retire();
            Ok(pi_plugin::PluginContextReplacement {
                cancelled: false,
                context: Some(next.handle(scope)),
            })
        }
    }

    #[test]
    fn tool_menu_retains_remote_names_and_shows_effective_exposure_read_only() {
        let tools = [
            crate::client::McpOfferedTool {
                name: "a-b".into(),
                description: "First line\nNot in menu".into(),
                exposure: McpExposure::Hidden,
            },
            crate::client::McpOfferedTool {
                name: "a_b".into(),
                description: String::new(),
                exposure: McpExposure::Codemode,
            },
        ];
        assert_eq!(
            tool_menu_options(&tools, McpExposure::Codemode),
            ["a-b [hidden] — First line", "a_b"]
        );
    }

    #[tokio::test]
    async fn semantic_manager_cancels_inspects_and_hands_off_after_policy_reload() {
        for (answers, expected_exposure, changed) in [
            (vec![None], "codemode", false),
            (vec![Some(0), None, None], "codemode", false),
            (vec![Some(0), Some(0), None, None], "codemode", false),
            (vec![Some(0), Some(1), None, None], "codemode", false),
            (vec![Some(0), Some(3), None, None, None], "codemode", false),
            (
                vec![Some(0), Some(3), Some(0), None, None],
                "codemode",
                false,
            ),
            (vec![Some(0), Some(3), Some(2)], "direct", true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("mcp.json");
            let original = r#"{"future":true,"mcpServers":{"docs":{"command":"unused"}}}"#;
            std::fs::write(&path, original).unwrap();
            let library = McpLibrary::new(directory.path(), None, false);
            let command = McpCommand {
                rows: library.read(McpScope::Global).unwrap().servers,
                library,
                pool: McpToolSet::connect_with_policies(vec![]).await.unwrap(),
            };
            let next_access = Arc::new(ManagerAccess::new(vec![], None));
            let next = PluginContextEpoch::new(next_access.clone());
            let access = Arc::new(ManagerAccess::new(answers, Some(next)));
            let epoch = PluginContextEpoch::new(access.clone());
            *access.old.lock().unwrap() = Some(epoch.clone());
            let (_, signal) = pi_core::AbortHandle::new();
            let context = CommandContext::with_plugin_context(
                directory.path().into(),
                signal,
                epoch.command_context(),
            );
            command
                .execute(context.clone(), String::new())
                .await
                .unwrap();
            assert_eq!(
                access.reloads.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(changed)
            );
            let content = std::fs::read_to_string(&path).unwrap();
            if changed {
                assert!(matches!(
                    context.ui.is_available(),
                    Err(pi_plugin::PluginContextError::Retired)
                ));
                assert_eq!(next_access.notices.lock().unwrap().len(), 1);
                let value: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(value["mcpServers"]["docs"]["exposure"], expected_exposure);
                assert_eq!(value["future"], true);
            } else {
                assert_eq!(content, original);
            }
            access.old.lock().unwrap().take();
        }
    }

    #[tokio::test]
    async fn policy_reload_failure_keeps_old_context_and_explains_saved_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"docs":{"command":"unused","enabled":false}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(directory.path(), None, false);
        let command = McpCommand {
            rows: library.read(McpScope::Global).unwrap().servers,
            library,
            pool: McpToolSet::connect_with_policies(vec![]).await.unwrap(),
        };
        let access = Arc::new(ManagerAccess::new(vec![], None));
        let epoch = PluginContextEpoch::new(access.clone());
        let (_, signal) = pi_core::AbortHandle::new();
        let context = CommandContext::with_plugin_context(
            directory.path().into(),
            signal,
            epoch.command_context(),
        );
        let error = command
            .execute(context.clone(), "enable docs".into())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("previous generation remains active")
        );
        assert!(context.ui.is_available().unwrap());
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            value["mcpServers"]["docs"]["enabled"]
                .as_bool()
                .unwrap_or(true)
        );
        assert!(command.library.read(McpScope::Global).unwrap().servers[0].enabled);
        assert!(access.notices.lock().unwrap().is_empty());
    }

    struct Access {
        active: Mutex<Vec<String>>,
        reaching: Vec<&'static str>,
        origin: SessionExecutionOrigin,
        notices: Mutex<Vec<(NoticeLevel, String)>>,
    }
    #[async_trait]
    impl SessionContextAccess for Access {
        fn execution_origin(&self) -> PluginContextResult<SessionExecutionOrigin> {
            Ok(self.origin)
        }
        fn active_tools(&self) -> PluginContextResult<Vec<String>> {
            Ok(self.active.lock().unwrap().clone())
        }
        fn tool_reaches_tools(&self, name: &str) -> PluginContextResult<bool> {
            Ok(self.reaching.contains(&name))
        }
        fn set_active_tools(&self, tools: Vec<String>) -> PluginContextResult<()> {
            *self.active.lock().unwrap() = tools;
            Ok(())
        }
    }
    impl ModelsContextAccess for Access {}
    impl UiContextAccess for Access {
        fn ui_notify(&self, level: NoticeLevel, message: String) -> PluginContextResult<()> {
            self.notices.lock().unwrap().push((level, message));
            Ok(())
        }
    }

    #[tokio::test]
    async fn startup_merges_only_live_desired_pending_intent_when_activating_an_entrypoint() {
        let echo = "mcp__docs__echo";
        let ambiguous = "mcp__docs__a_b";
        let other_lane = "mcp__docs__otherLane";
        for (entrypoint, initial, desired, expected, fails) in [
            (
                "codemode",
                vec!["read"],
                Some(Ok(vec![echo, ambiguous])),
                vec!["read", "codemode", echo, ambiguous],
                false,
            ),
            (
                "tool_search",
                vec!["read"],
                Some(Ok(vec![echo])),
                vec!["read", "tool_search", echo],
                false,
            ),
            // An earlier hook's explicit clear or replacement wins over the prepared snapshot.
            (
                "codemode",
                vec!["read"],
                Some(Ok(vec![])),
                vec!["read", "codemode"],
                false,
            ),
            (
                "codemode",
                vec!["read"],
                Some(Ok(vec![other_lane])),
                vec!["read", "codemode", other_lane],
                false,
            ),
            ("codemode", vec!["read"], None, vec!["read"], true),
            (
                "codemode",
                vec!["read"],
                Some(Err("credential-shaped-reader-error")),
                vec!["read"],
                true,
            ),
            // Ordinary no-op startup must not read or mutate the selection at all.
            (
                "codemode",
                vec!["read", "codemode"],
                None,
                vec!["read", "codemode"],
                false,
            ),
            (
                "codemode",
                vec!["read", "codemode"],
                Some(Err("must not read")),
                vec!["read", "codemode"],
                false,
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("mcp.json"),
                serde_json::json!({
                    "mcpServers": {"docs": {
                        "url": "http://127.0.0.1:9/mcp",
                        "exposure": if entrypoint == "codemode" { "codemode" } else { "deferred" },
                        "toolExposure": {"a-b": "hidden"}
                    }}
                })
                .to_string(),
            )
            .unwrap();
            let mut library = McpLibrary::new(directory.path(), None, false)
                .with_preserved_tool_selection(vec![echo.into(), ambiguous.into()])
                .with_tool_selection_candidates(vec![other_lane.into()]);
            if let Some(desired) = desired {
                library = library.with_tool_selection_reader(Arc::new(move || {
                    desired
                        .clone()
                        .map(|names| names.into_iter().map(String::from).collect())
                        .map_err(String::from)
                }));
            }
            let supervisor = crate::McpSupervisor::new();
            let prepared = library
                .prepare_supervised_generation(supervisor.clone(), Default::default())
                .await
                .unwrap();
            assert_eq!(prepared.unresolved_tool_selection, [ambiguous]);
            let access = Arc::new(Access {
                active: Mutex::new(initial.into_iter().map(String::from).collect()),
                reaching: vec![entrypoint],
                origin: SessionExecutionOrigin::User,
                notices: Mutex::new(vec![]),
            });
            let epoch = PluginContextEpoch::new(access.clone());
            let parts = epoch.context();
            let mut context = SessionPluginContext::unavailable_for_testing(
                PluginId::new("mcp"),
                1,
                SessionIdentity {
                    id: "selection".into(),
                    path: directory.path().join("session.jsonl"),
                    cwd: directory.path().into(),
                    parent_session_id: None,
                },
            );
            context.session = parts.session;
            context.ui = parts.ui;
            let result = prepared
                .plugin
                .session_start(
                    &context,
                    &SessionStartEvent {
                        reason: SessionStartReason::Startup,
                        previous_session_file: None,
                    },
                )
                .await;
            assert_eq!(result.is_err(), fails);
            if let Err(error) = result {
                assert!(error.to_string().contains("selection was left unchanged"));
                assert!(!error.to_string().contains("credential-shaped"));
            }
            assert_eq!(*access.active.lock().unwrap(), expected);
            assert!(!directory.path().join("session.jsonl").exists());
            supervisor.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn startup_requires_reaching_capability_and_warns_only_when_no_entry_point_is_active() {
        let directory = tempfile::tempdir().unwrap();
        let mut plugin = McpProductPlugin {
            library: McpLibrary::new(directory.path(), None, false),
            rows: vec![],
            pool: McpToolSet::connect(vec![]).await.unwrap(),
            auto_enable_codemode: true,
            needs_codemode: true,
            needs_tool_search: false,
        };
        for (active, reaching, auto_enable, origin, expected, warning) in [
            // An unrelated extension with the same name does not provide discovery.
            (
                vec!["read", "codemode"],
                vec![],
                true,
                SessionExecutionOrigin::User,
                vec!["read", "codemode"],
                Some("neither is active;"),
            ),
            (
                vec!["read"],
                vec![],
                true,
                SessionExecutionOrigin::User,
                vec!["read"],
                Some("neither is active;"),
            ),
            (
                vec!["read"],
                vec!["codemode"],
                false,
                SessionExecutionOrigin::User,
                vec!["read"],
                Some("(autoEnableCodemode is false)"),
            ),
            (
                vec!["read", "tool_search"],
                vec!["tool_search"],
                true,
                SessionExecutionOrigin::User,
                vec!["read", "tool_search"],
                None,
            ),
            (
                vec!["read", "codemode"],
                vec!["codemode"],
                true,
                SessionExecutionOrigin::User,
                vec!["read", "codemode"],
                None,
            ),
            // Child sessions follow the same rule within their registered tool scope.
            (
                vec!["read"],
                vec!["codemode"],
                true,
                SessionExecutionOrigin::Subagent,
                vec!["read", "codemode"],
                None,
            ),
        ] {
            let access = Arc::new(Access {
                active: Mutex::new(active.into_iter().map(String::from).collect()),
                reaching,
                origin,
                notices: Mutex::new(vec![]),
            });
            let epoch = PluginContextEpoch::new(access.clone());
            let parts = epoch.context();
            let mut context = SessionPluginContext::unavailable_for_testing(
                PluginId::new("mcp"),
                1,
                SessionIdentity {
                    id: "test".into(),
                    path: directory.path().join("session.jsonl"),
                    cwd: directory.path().into(),
                    parent_session_id: None,
                },
            );
            context.session = parts.session;
            context.ui = parts.ui;
            plugin.auto_enable_codemode = auto_enable;
            plugin
                .session_start(
                    &context,
                    &SessionStartEvent {
                        reason: SessionStartReason::Startup,
                        previous_session_file: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(*access.active.lock().unwrap(), expected);
            let notices = access.notices.lock().unwrap();
            match warning {
                Some(text) => {
                    assert_eq!(notices.len(), 1);
                    assert_eq!(notices[0].0, NoticeLevel::Warning);
                    assert!(notices[0].1.contains(text));
                }
                None => assert!(notices.is_empty()),
            }
        }
    }
}
