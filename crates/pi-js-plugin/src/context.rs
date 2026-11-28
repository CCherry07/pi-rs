use std::path::PathBuf;

use pi_core::{CustomMessageContent, CustomMessageInput, ModelId, ProviderId, ThinkingLevel};
use pi_plugin::{
    CompactOptions, ForkOptions, ForkPosition, NavigateTreeOptions, NewSessionOptions, NoticeLevel,
    SendMessageOptions, SendUserMessageOptions,
};
pub use pi_plugin::{
    MessageDelivery, PluginContextError, PluginContextHandle, PluginContextReplacement,
    PluginContextScope, PresentationMode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// JavaScript-only wire query decoded at the NAPI boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextQuery {
    Mode,
    HasUi,
    UiSnapshot,
    Cwd,
    Workspace,
    IsProjectTrusted,
    Model,
    ScopedModels,
    Models,
    AvailableModels,
    ProviderDisplayName { provider: String },
    ThinkingLevel,
    IsIdle,
    HasPendingMessages,
    ContextUsage,
    SystemPrompt,
    SystemPromptOptions,
    RenderSystemPrompt { options: Value },
    SessionCwd,
    SessionDir,
    SessionId,
    SessionFile,
    SessionLeafId,
    SessionLeafEntry,
    SessionEntry { id: String },
    SessionLabel { id: String },
    SessionBranch { from_id: Option<String> },
    SessionContextEntries,
    SessionHeader,
    SessionEntries,
    SessionTree,
    SessionName,
    ActiveTools,
    AllTools,
    Commands,
}

/// JavaScript-only fire-and-forget operation decoded at the NAPI boundary.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextNotification {
    UiNotification {
        notification: pi_plugin::UiNotification,
    },
    UiCommand {
        owner: String,
        command: pi_plugin::UiCommand,
    },
    Abort,
    Compact {
        #[serde(default)]
        custom_instructions: Option<String>,
    },
    Shutdown,
    UiNotify {
        message: String,
        level: NoticeLevel,
    },
    SendMessage {
        message: CustomMessageInput,
        #[serde(default)]
        options: SendMessageOptions,
    },
    SendUserMessage {
        content: CustomMessageContent,
        #[serde(default)]
        options: SendUserMessageOptions,
    },
    AppendEntry {
        custom_type: String,
        #[serde(default)]
        data: Option<Value>,
    },
    SetSessionName {
        name: String,
    },
    SetLabel {
        entry_id: String,
        #[serde(default)]
        label: Option<String>,
    },
    SetActiveTools {
        tool_names: Vec<String>,
    },
    SetThinkingLevel {
        level: String,
    },
    RegisterProvider {
        name: String,
        config: Value,
    },
    UnregisterProvider {
        name: String,
    },
    RegisterMcpServer {
        extension_path: String,
        name: String,
        config: Value,
    },
    UnregisterMcpServer {
        extension_path: String,
        name: String,
    },
}

/// JavaScript-only awaited operation decoded at the NAPI boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExtensionContextRequest {
    UiDialog {
        request: pi_plugin::UiDialogRequest,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    UiConfirm {
        title: String,
        message: String,
    },
    UiExchange {
        frame: pi_plugin::UiSurfaceFrame,
    },
    WaitForIdle,
    SendMessage {
        message: CustomMessageInput,
        #[serde(default)]
        options: SendMessageOptions,
    },
    SendUserMessage {
        content: CustomMessageContent,
        #[serde(default)]
        options: SendUserMessageOptions,
    },
    NewSession {
        #[serde(default)]
        parent_session: Option<String>,
    },
    Fork {
        entry_id: String,
        #[serde(default)]
        position: ForkPosition,
    },
    NavigateTree {
        target_id: String,
        #[serde(default)]
        summarize: bool,
        #[serde(default)]
        custom_instructions: Option<String>,
        #[serde(default)]
        replace_instructions: bool,
        #[serde(default)]
        label: Option<String>,
    },
    SwitchSession {
        session_path: PathBuf,
    },
    Reload,
    SetModel {
        provider: String,
        model_id: String,
    },
}

pub fn execute_context_query(
    context: &PluginContextHandle,
    query: ExtensionContextQuery,
) -> Result<Value, PluginContextError> {
    let access = context.access_for_adapter()?;
    match query {
        ExtensionContextQuery::Mode => value(access.mode()?),
        ExtensionContextQuery::HasUi => value(access.has_ui()?),
        ExtensionContextQuery::UiSnapshot => value(access.ui_surface_snapshot()?),
        ExtensionContextQuery::Cwd => value(access.cwd()?),
        ExtensionContextQuery::Workspace => value(
            context
                .workspace()
                .map(|workspace| workspace.spec().clone())
                .unwrap_or(pi_core::WorkspaceSpec::from_cwd(access.cwd()?)),
        ),
        ExtensionContextQuery::IsProjectTrusted => value(access.is_project_trusted()?),
        ExtensionContextQuery::Model => value(access.model()?),
        ExtensionContextQuery::ScopedModels => value(access.scoped_models()?),
        ExtensionContextQuery::Models => value(access.models()?),
        ExtensionContextQuery::AvailableModels => value(access.available_models()?),
        ExtensionContextQuery::ProviderDisplayName { provider } => {
            value(access.provider_display_name(&ProviderId::new(provider))?)
        }
        ExtensionContextQuery::ThinkingLevel => value(access.thinking_level()?),
        ExtensionContextQuery::IsIdle => value(access.is_idle()?),
        ExtensionContextQuery::HasPendingMessages => value(access.has_pending_messages()?),
        ExtensionContextQuery::ContextUsage => value(access.context_usage()?),
        ExtensionContextQuery::SystemPrompt => value(access.system_prompt()?),
        ExtensionContextQuery::SystemPromptOptions => access.system_prompt_options(context.scope()),
        ExtensionContextQuery::RenderSystemPrompt { options } => {
            value(access.render_system_prompt(options)?)
        }
        ExtensionContextQuery::SessionCwd => value(access.session_cwd()?),
        ExtensionContextQuery::SessionDir => value(access.session_dir()?),
        ExtensionContextQuery::SessionId => value(access.session_id()?),
        ExtensionContextQuery::SessionFile => value(access.session_file()?),
        ExtensionContextQuery::SessionLeafId => value(access.session_leaf_id()?),
        ExtensionContextQuery::SessionLeafEntry => value(access.session_leaf_entry()?),
        ExtensionContextQuery::SessionEntry { id } => value(access.session_entry(&id)?),
        ExtensionContextQuery::SessionLabel { id } => value(access.session_label(&id)?),
        ExtensionContextQuery::SessionBranch { from_id } => {
            value(access.session_branch(from_id.as_deref())?)
        }
        ExtensionContextQuery::SessionContextEntries => value(access.session_context_entries()?),
        ExtensionContextQuery::SessionHeader => access.session_header(),
        ExtensionContextQuery::SessionEntries => value(access.session_entries()?),
        ExtensionContextQuery::SessionTree => value(access.session_tree()?),
        ExtensionContextQuery::SessionName => value(access.session_name()?),
        ExtensionContextQuery::ActiveTools => value(access.active_tools()?),
        ExtensionContextQuery::AllTools => Ok(Value::Array(
            access
                .all_tools()?
                .into_iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "label": tool.label,
                        "description": tool.description,
                        "parameters": tool.parameters,
                        "promptSnippet": tool.prompt_snippet,
                        "promptGuidelines": tool.prompt_guidelines,
                    })
                })
                .collect(),
        )),
        ExtensionContextQuery::Commands => Ok(Value::Array(
            access
                .commands()?
                .into_iter()
                .map(|command| {
                    json!({
                        "name": command.name,
                        "description": command.description,
                        "argumentHint": command.argument_hint,
                    })
                })
                .collect(),
        )),
    }
}

pub fn execute_context_notification(
    context: &PluginContextHandle,
    notification: ExtensionContextNotification,
) -> Result<(), PluginContextError> {
    let access = context.access_for_adapter()?;
    match notification {
        ExtensionContextNotification::UiNotification { notification } => {
            access.ui_notification(notification)
        }
        ExtensionContextNotification::UiCommand { owner, command } => {
            access.ui_command(owner, command)
        }
        ExtensionContextNotification::Abort => access.abort(),
        ExtensionContextNotification::Compact {
            custom_instructions,
        } => access.compact(CompactOptions {
            custom_instructions,
        }),
        ExtensionContextNotification::Shutdown => access.shutdown(),
        ExtensionContextNotification::UiNotify { message, level } => {
            access.ui_notify(level, message)
        }
        ExtensionContextNotification::SendMessage { message, options } => {
            access.send_message(message, options)
        }
        ExtensionContextNotification::SendUserMessage { content, options } => {
            access.send_user_message(content, options)
        }
        ExtensionContextNotification::AppendEntry { custom_type, data } => {
            access.append_entry(custom_type, data)
        }
        ExtensionContextNotification::SetSessionName { name } => access.set_session_name(name),
        ExtensionContextNotification::SetLabel { entry_id, label } => {
            access.set_label(entry_id, label)
        }
        ExtensionContextNotification::SetActiveTools { tool_names } => {
            access.set_active_tools(tool_names)
        }
        ExtensionContextNotification::SetThinkingLevel { level } => access.set_thinking_level(
            level
                .parse::<ThinkingLevel>()
                .map_err(PluginContextError::Invalid)?,
        ),
        ExtensionContextNotification::RegisterProvider { name, config } => {
            access.register_provider(name, config)
        }
        ExtensionContextNotification::UnregisterProvider { name } => {
            access.unregister_provider(name)
        }
        ExtensionContextNotification::RegisterMcpServer {
            extension_path,
            name,
            config,
        } => access.register_mcp_server(extension_path, name, config),
        ExtensionContextNotification::UnregisterMcpServer {
            extension_path,
            name,
        } => access.unregister_mcp_server(extension_path, name),
    }
}

pub struct ExtensionContextRequestOutput {
    pub value: Value,
    pub replacement: Option<PluginContextHandle>,
}

pub async fn execute_context_request(
    context: &PluginContextHandle,
    request: ExtensionContextRequest,
) -> Result<ExtensionContextRequestOutput, PluginContextError> {
    let access = context.access_for_adapter()?;
    match request {
        ExtensionContextRequest::UiDialog {
            request,
            timeout_ms,
        } => Ok(request_output(value(
            access.ui_dialog(request, timeout_ms).await?,
        )?)),
        ExtensionContextRequest::UiConfirm { title, message } => Ok(request_output(value(
            access.ui_confirm(title, message).await?.unwrap_or(false),
        )?)),
        ExtensionContextRequest::UiExchange { frame } => Ok(request_output(value(
            access.ui_surface_exchange(frame).await?,
        )?)),
        ExtensionContextRequest::WaitForIdle => {
            access.wait_for_idle(context.scope()).await?;
            Ok(request_output(Value::Null))
        }
        ExtensionContextRequest::SendMessage { message, options } => {
            access
                .send_message_and_wait(context.scope(), message, options)
                .await?;
            Ok(request_output(Value::Null))
        }
        ExtensionContextRequest::SendUserMessage { content, options } => {
            access
                .send_user_message_and_wait(context.scope(), content, options)
                .await?;
            Ok(request_output(Value::Null))
        }
        ExtensionContextRequest::NewSession { parent_session } => replacement_output(
            access
                .new_session(context.scope(), NewSessionOptions { parent_session })
                .await?,
        ),
        ExtensionContextRequest::Fork { entry_id, position } => replacement_output(
            access
                .fork(context.scope(), entry_id, ForkOptions { position })
                .await?,
        ),
        ExtensionContextRequest::NavigateTree {
            target_id,
            summarize,
            custom_instructions,
            replace_instructions,
            label,
        } => Ok(request_output(json!({
            "cancelled": access
                .navigate_tree(
                    context.scope(),
                    target_id,
                    NavigateTreeOptions {
                        summarize,
                        custom_instructions,
                        replace_instructions,
                        label,
                    },
                )
                .await?,
        }))),
        ExtensionContextRequest::SwitchSession { session_path } => {
            replacement_output(access.switch_session(context.scope(), session_path).await?)
        }
        ExtensionContextRequest::Reload => reload_output(access.reload(context.scope()).await?),
        ExtensionContextRequest::SetModel { provider, model_id } => Ok(request_output(value(
            access
                .set_model(
                    context.scope(),
                    ProviderId::new(provider),
                    ModelId::new(model_id),
                )
                .await?,
        )?)),
    }
}

fn request_output(value: Value) -> ExtensionContextRequestOutput {
    ExtensionContextRequestOutput {
        value,
        replacement: None,
    }
}

fn replacement_output(
    replacement: PluginContextReplacement,
) -> Result<ExtensionContextRequestOutput, PluginContextError> {
    Ok(ExtensionContextRequestOutput {
        value: json!({ "cancelled": replacement.cancelled }),
        replacement: replacement.context,
    })
}

fn reload_output(
    replacement: PluginContextReplacement,
) -> Result<ExtensionContextRequestOutput, PluginContextError> {
    if replacement.cancelled {
        return Err(PluginContextError::Failed(
            "session reload unexpectedly reported cancellation".to_string(),
        ));
    }
    let context = replacement.context.ok_or_else(|| {
        PluginContextError::Failed("session reload did not return a product context".to_string())
    })?;
    Ok(ExtensionContextRequestOutput {
        value: Value::Null,
        replacement: Some(context),
    })
}

fn value<T: Serialize>(value: T) -> Result<Value, PluginContextError> {
    serde_json::to_value(value).map_err(|error| PluginContextError::Failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use pi_plugin::{
        ModelsContextAccess, PluginContext, PluginContextEpoch, SessionContextAccess,
        UiContextAccess,
    };

    use super::*;

    struct ReplacementContext {
        old: Mutex<Option<PluginContextEpoch>>,
    }

    struct ReplacementTarget;

    impl ModelsContextAccess for ReplacementContext {}
    impl UiContextAccess for ReplacementContext {}

    #[async_trait]
    impl SessionContextAccess for ReplacementContext {
        async fn new_session(
            &self,
            scope: PluginContextScope,
            _options: NewSessionOptions,
        ) -> Result<PluginContextReplacement, PluginContextError> {
            self.old.lock().unwrap().take().unwrap().retire();
            let target: Arc<dyn PluginContext> = Arc::new(ReplacementTarget);
            Ok(PluginContextReplacement {
                cancelled: false,
                context: Some(PluginContextEpoch::new(target).handle(scope)),
            })
        }

        async fn reload(
            &self,
            scope: PluginContextScope,
        ) -> Result<PluginContextReplacement, PluginContextError> {
            self.old.lock().unwrap().take().unwrap().retire();
            let target: Arc<dyn PluginContext> = Arc::new(ReplacementTarget);
            Ok(PluginContextReplacement {
                cancelled: false,
                context: Some(PluginContextEpoch::new(target).handle(scope)),
            })
        }
    }

    impl ModelsContextAccess for ReplacementTarget {}

    #[async_trait]
    impl SessionContextAccess for ReplacementTarget {}

    impl UiContextAccess for ReplacementTarget {
        fn mode(&self) -> Result<PresentationMode, PluginContextError> {
            Ok(PresentationMode::Json)
        }
    }

    #[test]
    fn context_operations_keep_the_pi_javascript_tags() {
        assert_eq!(
            serde_json::to_value(ExtensionContextQuery::SessionEntry {
                id: "entry-1".to_string(),
            })
            .unwrap(),
            json!({ "type": "sessionEntry", "id": "entry-1" })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextRequest::SwitchSession {
                session_path: PathBuf::from("session.jsonl"),
            })
            .unwrap(),
            json!({ "type": "switchSession", "sessionPath": "session.jsonl" })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextNotification::UiNotify {
                message: "Extension notice".to_string(),
                level: NoticeLevel::Warning,
            })
            .unwrap(),
            json!({
                "type": "uiNotify",
                "message": "Extension notice",
                "level": "warning",
            })
        );
        assert_eq!(
            serde_json::to_value(ExtensionContextNotification::RegisterMcpServer {
                extension_path: "/extensions/example.ts".to_string(),
                name: "docs".to_string(),
                config: json!({"command":"fixture"}),
            })
            .unwrap(),
            json!({
                "type": "registerMcpServer",
                "extensionPath": "/extensions/example.ts",
                "name": "docs",
                "config": {"command":"fixture"},
            })
        );
    }

    struct DialogContext {
        requests: Mutex<Vec<(pi_plugin::UiDialogRequest, Option<u64>)>>,
    }

    impl ModelsContextAccess for DialogContext {}
    impl SessionContextAccess for DialogContext {}

    #[async_trait]
    impl UiContextAccess for DialogContext {
        async fn ui_dialog(
            &self,
            request: pi_plugin::UiDialogRequest,
            timeout_ms: Option<u64>,
        ) -> Result<Option<pi_plugin::UiDialogResponse>, PluginContextError> {
            self.requests
                .lock()
                .unwrap()
                .push((request.clone(), timeout_ms));
            Ok(Some(match request {
                pi_plugin::UiDialogRequest::Confirm { .. } => {
                    pi_plugin::UiDialogResponse::Confirmed(true)
                }
                pi_plugin::UiDialogRequest::Select { options, .. } => {
                    pi_plugin::UiDialogResponse::Value(options[1].clone())
                }
                pi_plugin::UiDialogRequest::Input { .. } => {
                    pi_plugin::UiDialogResponse::Value(String::new())
                }
                pi_plugin::UiDialogRequest::Editor { .. } => pi_plugin::UiDialogResponse::Cancelled,
            }))
        }
    }

    #[tokio::test]
    async fn semantic_dialog_requests_keep_typed_results_across_the_javascript_boundary() {
        let access = Arc::new(DialogContext {
            requests: Mutex::default(),
        });
        let epoch = PluginContextEpoch::new(access.clone());
        let handle = epoch.handle(PluginContextScope::Base);
        for (request, expected) in [
            (
                json!({"method":"confirm","title":"Continue?","message":"x"}),
                json!({"type":"confirmed","value":true}),
            ),
            (
                json!({"method":"select","title":"Pick","options":["one","two"]}),
                json!({"type":"value","value":"two"}),
            ),
            (
                json!({"method":"input","title":"Name","placeholder":"optional"}),
                json!({"type":"value","value":""}),
            ),
            (
                json!({"method":"editor","title":"Notes","prefill":"line one\nline two"}),
                json!({"type":"cancelled"}),
            ),
        ] {
            let decoded: ExtensionContextRequest = serde_json::from_value(json!({
                "type":"uiDialog", "request":request, "timeoutMs":250
            }))
            .unwrap();
            let result = execute_context_request(&handle, decoded).await.unwrap();
            assert_eq!(result.value, expected);
            assert!(result.replacement.is_none());
        }
        assert_eq!(access.requests.lock().unwrap().len(), 4);
        assert!(
            access
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(_, timeout)| *timeout == Some(250))
        );

        let unsupported = PluginContextEpoch::new(Arc::new(ReplacementTarget));
        let request = ExtensionContextRequest::UiDialog {
            request: pi_plugin::UiDialogRequest::Input {
                title: "Name".into(),
                placeholder: None,
            },
            timeout_ms: None,
        };
        assert_eq!(
            execute_context_request(
                &unsupported.handle(PluginContextScope::Base),
                request.clone()
            )
            .await
            .unwrap()
            .value,
            Value::Null
        );
        epoch.retire();
        assert!(matches!(
            execute_context_request(&handle, request).await,
            Err(PluginContextError::Retired)
        ));
    }

    #[tokio::test]
    async fn replacement_requests_return_the_new_runtime_context_handle() {
        let access = Arc::new(ReplacementContext {
            old: Mutex::new(None),
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let old = PluginContextEpoch::new(plugin_access);
        *access.old.lock().unwrap() = Some(old.clone());
        let handle = old.handle(PluginContextScope::Command);

        let output = execute_context_request(
            &handle,
            ExtensionContextRequest::NewSession {
                parent_session: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.value, json!({ "cancelled": false }));
        assert!(matches!(
            execute_context_query(&handle, ExtensionContextQuery::Mode),
            Err(PluginContextError::Retired)
        ));
        assert_eq!(
            execute_context_query(&output.replacement.unwrap(), ExtensionContextQuery::Mode,)
                .unwrap(),
            json!("json")
        );
    }

    #[tokio::test]
    async fn reload_requests_return_the_new_runtime_context_handle() {
        let access = Arc::new(ReplacementContext {
            old: Mutex::new(None),
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let old = PluginContextEpoch::new(plugin_access);
        *access.old.lock().unwrap() = Some(old.clone());
        let handle = old.handle(PluginContextScope::Command);

        let output = execute_context_request(&handle, ExtensionContextRequest::Reload)
            .await
            .unwrap();

        assert_eq!(output.value, Value::Null);
        assert!(matches!(
            execute_context_query(&handle, ExtensionContextQuery::Mode),
            Err(PluginContextError::Retired)
        ));
        assert_eq!(
            execute_context_query(
                &output
                    .replacement
                    .expect("reload must hand off the replacement context"),
                ExtensionContextQuery::Mode,
            )
            .unwrap(),
            json!("json")
        );
    }
}
