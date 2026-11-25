//! Semantic MCP navigation. All writes go through the library, then a whole-generation reload.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Inspect,
    Tools,
    Reconnect,
    Enable(McpScope),
    Disable(McpScope),
    Exposure,
    SignIn,
    SignOut,
}

impl McpCommand {
    pub(super) fn argument_completions(&self) -> Vec<pi_plugin::UiCompletion> {
        let actions = ["login", "logout", "reconnect"];
        let mut completions = actions
            .iter()
            .map(|action| pi_plugin::UiCompletion {
                value: format!("{action} "),
                label: (*action).into(),
                description: None,
            })
            .collect::<Vec<_>>();
        for action in actions {
            completions.extend(
                self.rows
                    .iter()
                    .filter(|row| row.enabled && (action == "reconnect" || row.supports_oauth))
                    .map(|row| pi_plugin::UiCompletion {
                        value: format!("{action} {}", row.name),
                        label: row.name.clone(),
                        description: Some(self.server_state(row).as_str().into()),
                    }),
            );
        }
        completions
    }

    pub(super) fn manager_actions(&self, row: &McpServerRow) -> Vec<(Action, String)> {
        let mut actions = vec![(Action::Inspect, "Inspect".into())];
        let project_override =
            row.scope == McpScope::Global && self.library.path(McpScope::Project).is_ok();
        if !row.enabled {
            actions.push((Action::Enable(row.scope), "Enable".into()));
            if project_override {
                actions.push((
                    Action::Enable(McpScope::Project),
                    "Enable in this project".into(),
                ));
            }
            return actions;
        }
        if self.server_state(row) == McpServerState::NeedsAuth && row.supports_oauth {
            actions.push((Action::SignIn, "Sign in".into()));
        }
        if self.server_state(row) == McpServerState::Connected {
            actions.push((Action::Tools, "Tools".into()));
            if row.supports_oauth {
                actions.push((Action::SignOut, "Sign out".into()));
            }
        }
        actions.push((Action::Reconnect, "Reconnect".into()));
        actions.push((Action::Exposure, "Change exposure".into()));
        actions.push((Action::Disable(row.scope), "Disable".into()));
        if project_override {
            actions.push((
                Action::Disable(McpScope::Project),
                "Disable in this project".into(),
            ));
        }
        actions
    }

    pub(super) fn eligible_server(
        &self,
        name: &str,
        oauth: bool,
    ) -> Result<&McpServerRow, CommandError> {
        let row = self
            .rows
            .iter()
            .find(|row| row.name == name)
            .ok_or_else(|| CommandError::Execution(self.library.unknown_server_message(name)))?;
        if !row.enabled {
            return Err(CommandError::Execution(format!(
                "MCP server {name:?} is disabled."
            )));
        }
        if oauth && !row.supports_oauth {
            return Err(CommandError::Execution(match &row.auth_provider {
                Some(provider) => format!(
                    "This MCP server uses provider credentials; use pi auth login/logout {provider}"
                ),
                None => "Only HTTP servers without an Authorization header use MCP OAuth.".into(),
            }));
        }
        Ok(row)
    }

    pub(super) async fn pick_server(
        &self,
        context: &CommandContext,
        oauth: bool,
    ) -> Result<Option<&McpServerRow>, CommandError> {
        let candidates = self
            .rows
            .iter()
            .filter(|row| row.enabled && (!oauth || row.supports_oauth))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            context.ui.notify(NoticeLevel::Info, if oauth { "No enabled MCP server uses OAuth. Only HTTP servers without provider auth or an Authorization header do." } else { "No enabled MCP servers." })?;
            return Ok(None);
        }
        if candidates.len() == 1 {
            return Ok(Some(candidates[0]));
        }
        if oauth {
            let preferred = candidates
                .iter()
                .filter(|row| self.server_state(row) == McpServerState::NeedsAuth)
                .collect::<Vec<_>>();
            if preferred.len() == 1 {
                return Ok(Some(*preferred[0]));
            }
        }
        if !context.ui.is_available()? {
            return Err(CommandError::InvalidArguments(format!(
                "Name an MCP server. Configured: {}",
                candidates
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        tokio::select! {
            biased;
            () = context.signal().wait() => Err(CommandError::Aborted),
            selected = context.ui.select("MCP server", candidates.iter().map(|row| row.name.clone()).collect()) => Ok(selected?.and_then(|index| candidates.get(index).copied())),
        }
    }

    async fn show_tools(
        &self,
        context: &CommandContext,
        row: &McpServerRow,
    ) -> Result<(), CommandError> {
        let options = tool_menu_options(self.pool.offered_tools(&row.name), row.exposure);
        if options.is_empty() {
            context
                .ui
                .notify(NoticeLevel::Info, "The server offers no tools.")?;
        } else {
            context
                .ui
                .select(
                    format!(
                        "Tools of {} ({}; read-only)",
                        row.name,
                        row.exposure.as_str()
                    ),
                    options,
                )
                .await?;
        }
        Ok(())
    }

    pub(super) async fn manage(
        &self,
        context: CommandContext,
    ) -> Result<CommandOutcome, CommandError> {
        if self.rows.is_empty() {
            context.ui.notify(
                NoticeLevel::Info,
                "No MCP servers configured. Use `pi mcp add` or edit mcp.json.",
            )?;
            return Ok(CommandOutcome::Handled);
        }
        loop {
            let options = self
                .rows
                .iter()
                .map(|row| {
                    format!(
                        "{} · {} · {} · {}",
                        row.name,
                        self.server_state(row).as_str(),
                        row.exposure.as_str(),
                        scope_label(row.scope)
                    )
                })
                .collect();
            let Some(row) = context
                .ui
                .select("MCP servers", options)
                .await?
                .and_then(|index| self.rows.get(index))
            else {
                return Ok(CommandOutcome::Handled);
            };
            loop {
                let actions = self.manager_actions(row);
                let Some(action) = context
                    .ui
                    .select(
                        format!("MCP server {}", row.name),
                        actions.iter().map(|(_, label)| label.clone()).collect(),
                    )
                    .await?
                    .and_then(|index| actions.get(index).map(|(action, _)| *action))
                else {
                    break;
                };
                match action {
                    Action::Inspect => {
                        let source = self
                            .library
                            .path(row.defining_scope)
                            .map_err(CommandError::Execution)?;
                        let override_note = if row.scope != row.defining_scope {
                            format!(
                                "\nproject override: {}",
                                self.library
                                    .path(row.scope)
                                    .map_err(CommandError::Execution)?
                                    .display()
                            )
                        } else {
                            String::new()
                        };
                        let provider = row
                            .auth_provider
                            .as_ref()
                            .map(|provider| {
                                format!("\nprovider: {provider} (use pi auth login {provider})")
                            })
                            .unwrap_or_default();
                        let connection = self
                            .connection_diagnostic(&row.name)
                            .map(|error| format!("\n{error}"))
                            .unwrap_or_default();
                        context.ui.notify(NoticeLevel::Info, format!("{}\ntransport: {}\nsource: {}\nstate: {}\nexposure: {}\ntools: {}{override_note}{provider}{}{connection}", row.name, row.transport, source.display(), self.server_state(row).as_str(), row.exposure.as_str(), self.pool.offered_tools(&row.name).len(), if self.pool.has_resources(&row.name) { "\nresources: available" } else { "" }))?;
                    }
                    Action::Tools => self.show_tools(&context, row).await?,
                    Action::Reconnect => {
                        self.reconnect_and_reload(&context, &row.name).await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    Action::Enable(scope) | Action::Disable(scope) => {
                        let enabled = matches!(action, Action::Enable(_));
                        self.library
                            .set_enabled(scope, &row.name, enabled)
                            .map_err(CommandError::Execution)?;
                        self.reload_after_change(
                            &context,
                            format!(
                                "{} MCP server {:?} in {} configuration",
                                if enabled { "Enabled" } else { "Disabled" },
                                row.name,
                                scope_label(scope)
                            ),
                        )
                        .await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    Action::Exposure => {
                        let exposures = [
                            McpExposure::Codemode,
                            McpExposure::Deferred,
                            McpExposure::Direct,
                            McpExposure::Hidden,
                        ];
                        let Some(exposure) = context
                            .ui
                            .select(
                                format!("Exposure for {}", row.name),
                                exposures
                                    .iter()
                                    .map(|exposure| exposure.as_str().to_string())
                                    .collect(),
                            )
                            .await?
                            .and_then(|index| exposures.get(index).copied())
                        else {
                            continue;
                        };
                        if exposure == row.exposure {
                            continue;
                        }
                        self.library
                            .set_exposure(row.scope, &row.name, exposure)
                            .map_err(CommandError::Execution)?;
                        self.reload_after_change(
                            &context,
                            format!(
                                "Set MCP server {:?} exposure to {}",
                                row.name,
                                exposure.as_str()
                            ),
                        )
                        .await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    Action::SignIn => {
                        self.login(&context, &row.name, None).await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    Action::SignOut => {
                        let message = self
                            .library
                            .logout(&row.name, None)
                            .map_err(CommandError::Execution)?;
                        self.reload_after_change(&context, message).await?;
                        return Ok(CommandOutcome::Handled);
                    }
                }
            }
        }
    }
}
