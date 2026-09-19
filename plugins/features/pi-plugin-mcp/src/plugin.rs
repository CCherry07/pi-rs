//! Local MCP product plugin and management command; explicit client pools stay tool-only.
use crate::{McpLibrary, McpScope, McpServerRow, McpToolSet};
use async_trait::async_trait;
use pi_core::PluginId;
use pi_plugin::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel, Plugin,
    RegisterContext,
};
use std::sync::Arc;

pub(crate) struct McpProductPlugin {
    pub(crate) library: McpLibrary,
    pub(crate) rows: Vec<McpServerRow>,
    pub(crate) pool: McpToolSet,
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
                let tools = self.pool.tools();
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
                                format!(
                                    "{} tools discovered",
                                    tools.iter().filter(|t| t.server_name == row.name).count()
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
