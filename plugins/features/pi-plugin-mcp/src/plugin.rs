//! Local MCP product plugin and management command; explicit client pools stay tool-only.
use crate::{McpLibrary, McpScope, McpServerRow, McpToolSet};
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
        self.pool.plugin().register(context)?;
        context.register_command(Arc::new(McpCommand {
            library: self.library.clone(),
            rows: self.rows.clone(),
            pool: self.pool.clone(),
        }))
    }

    async fn session_start(
        &self,
        context: &pi_plugin::SessionPluginContext,
        _event: &pi_plugin::SessionStartEvent,
    ) -> Result<(), PluginError> {
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

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        Ok(self.pool.prompt_patch(event))
    }
}
struct McpCommand {
    library: McpLibrary,
    rows: Vec<McpServerRow>,
    pool: McpToolSet,
}
#[async_trait]
impl Command for McpCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "mcp".into(),
            description: "Inspect MCP servers, test connections or reload configuration".into(),
            argument_hint: Some("[status|paths|test <name>|reload]".into()),
        }
    }
    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        let arguments = arguments.trim();
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
                                    "{direct} direct tools · {deferred} deferred tools · {codemode} codemode tools{}",
                                    if self.pool.has_resources(&row.name) {
                                        " · resources available"
                                    } else {
                                        ""
                                    }
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
                let names = tokio::select! {
                    result = self.library.test(arguments[5..].trim()) => result.map_err(CommandError::Execution)?,
                    () = context.signal().wait() => return Err(CommandError::Aborted),
                };
                format!(
                    "MCP connection succeeded: {} tools\n{}",
                    names.len(),
                    names.join("\n")
                )
            }
            _ => {
                return Err(CommandError::InvalidArguments(
                    "Use /mcp [status|paths|test <name>|reload]".into(),
                ));
            }
        };
        context.ui.notify(NoticeLevel::Info, output)?;
        Ok(CommandOutcome::Handled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_plugin::{
        ModelsContextAccess, PluginContextEpoch, PluginContextResult, SessionContextAccess,
        SessionExecutionOrigin, SessionIdentity, SessionPluginContext, SessionStartEvent,
        SessionStartReason, UiContextAccess,
    };
    use std::sync::Mutex;

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
