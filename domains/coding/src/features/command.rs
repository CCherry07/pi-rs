//! Coding-owned feature selection; the frontend renders ordinary semantic UI nodes.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::PluginId;
use pi_plugin::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel, Plugin,
    PluginContextError, PluginContextResult, PresentationMode, RegisterContext, UiComponent,
    UiComponentUpdate, UiInteraction, UiNode, UiNodeKind, UiOverlayOptions, UiRegion,
    UiRegionPlacement, UiShortcut, UiSurfaceEvent, UiSurfaceFrame, UiTone,
};
use pi_settings::{
    FeatureSettings, FeatureSettingsPatch, SettingsContext, SettingsManager, SettingsScope,
};

use super::Features;

#[derive(Clone)]
pub(crate) struct FeaturesPlugin {
    settings: SettingsManager,
    settings_context: SettingsContext,
    allowed: Features,
}

impl FeaturesPlugin {
    pub(crate) fn new(
        settings: SettingsManager,
        settings_context: SettingsContext,
        allowed: Features,
    ) -> Self {
        Self {
            settings,
            settings_context,
            allowed,
        }
    }
}

#[pi_plugin::plugin]
impl Plugin for FeaturesPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("features")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_command(Arc::new(self.clone()))
    }
}

#[async_trait]
impl Command for FeaturesPlugin {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "features".into(),
            description: "Choose optional capabilities and reload the session".into(),
            argument_hint: Some("[global|project]".into()),
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        let result = self.select_and_apply(&context, &arguments).await;
        if let Err(error) = &result
            && !matches!(error, CommandError::Aborted)
        {
            // Keep failures in the visible transcript, including when the footer is clipped.
            // A retired context must not replace the original error with a notice failure.
            let _ = context.ui.notify(NoticeLevel::Error, error.to_string());
        }
        result
    }
}

impl FeaturesPlugin {
    async fn select_and_apply(
        &self,
        context: &CommandContext,
        arguments: &str,
    ) -> Result<CommandOutcome, CommandError> {
        if context.signal().is_aborted() {
            return Err(CommandError::Aborted);
        }
        let requested_scope =
            match arguments.trim() {
                "" => None,
                "global" => Some(SettingsScope::Global),
                "project" if self.settings_context.project_trusted => Some(SettingsScope::Project),
                "project" => return Err(CommandError::Execution(
                    "Project feature settings require project trust; use /trust and /reload first"
                        .into(),
                )),
                _ => {
                    return Err(CommandError::InvalidArguments(
                        "usage: /features [global|project]".into(),
                    ));
                }
            };
        if context.ui.mode()? != PresentationMode::Tui || !context.ui.is_available()? {
            return Err(CommandError::Execution(
                "/features requires an interactive TUI; edit the features object in settings.json and reload to apply it".into()
            ));
        }
        let scope = match requested_scope {
            Some(scope) => scope,
            None if !self.settings_context.project_trusted => SettingsScope::Global,
            None => {
                let selected = tokio::select! {
                    selected = context.ui.select("Features — save scope", vec![
                        "Global defaults".into(), "This project".into()
                    ]) => selected?,
                    () = context.signal().wait() => return Err(CommandError::Aborted),
                };
                match selected {
                    Some(0) => SettingsScope::Global,
                    Some(_) => SettingsScope::Project,
                    None => return Ok(CommandOutcome::Handled),
                }
            }
        };
        let snapshot = self.settings.load(&self.settings_context);
        let (selection, path) = match scope {
            SettingsScope::Global => (
                snapshot.global().features,
                self.settings.agent_dir().join("settings.json"),
            ),
            SettingsScope::Project => (
                snapshot.effective().features,
                self.settings_context.cwd.join(".pi/settings.json"),
            ),
        };
        let project_overrides = scope == SettingsScope::Global
            && snapshot
                .raw_project()
                .get("features")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|fields| !fields.is_empty());
        let editor = FeaturesEditor::new(selection, self.allowed, scope, project_overrides);
        let Some(patch) = context.ui.custom(editor, context.signal().clone()).await? else {
            return Ok(CommandOutcome::Handled);
        };
        if patch == FeatureSettingsPatch::default() {
            context
                .ui
                .notify(NoticeLevel::Info, "Feature selection unchanged")?;
            return Ok(CommandOutcome::Handled);
        }
        if context.signal().is_aborted() {
            return Err(CommandError::Aborted);
        }
        // Check the generation before entering the blocking, locked file transaction.
        context.session.id()?;
        let settings = self.settings.clone();
        let settings_context = self.settings_context.clone();
        tokio::task::spawn_blocking(move || {
            settings.patch_features(&settings_context, scope, patch)
        })
        .await
        .map_err(|error| CommandError::Execution(format!("Feature settings task failed: {error}")))?
        .map_err(|error| CommandError::Execution(error.to_string()))?;
        if context.signal().is_aborted() {
            return Err(CommandError::Execution(format!(
                "Features saved to {}, but reload was cancelled; run /reload to apply them",
                path.display()
            )));
        }
        let replacement = context.session.reload().await.map_err(|error| CommandError::Execution(format!(
            "Features saved to {}, but reload failed: {error}. The current session remains active; fix the configuration and run /reload to retry",
            path.display()
        )))?;
        replacement.ui.notify(
            NoticeLevel::Info,
            format!(
                "Features saved to {} and reloaded{}",
                path.display(),
                if project_overrides {
                    "; project feature overrides still apply"
                } else {
                    ""
                }
            ),
        )?;
        Ok(CommandOutcome::Handled)
    }
}

const LABELS: [&str; 6] = [
    "Memory — persistent recall and session search",
    "Subagents — delegate work to other agents",
    "Schedule — run tasks at scheduled times",
    "Skills — discover and invoke skills",
    "Prompt templates — reusable prompt commands",
    "Session transfer — export, import and share",
];

struct FeaturesEditor {
    initial: [bool; 6],
    selected: [bool; 6],
    allowed: [bool; 6],
    focused: usize,
    scope: SettingsScope,
    project_overrides: bool,
}

impl FeaturesEditor {
    fn new(
        settings: FeatureSettings,
        allowed: Features,
        scope: SettingsScope,
        project_overrides: bool,
    ) -> Self {
        let initial = [
            settings.memory,
            settings.subagents,
            settings.schedule,
            settings.skills,
            settings.prompt_templates,
            settings.session_transfer,
        ];
        Self {
            initial,
            selected: initial,
            allowed: [
                allowed.memory,
                allowed.subagents,
                allowed.schedule,
                allowed.skills,
                allowed.prompt_templates,
                allowed.session_transfer,
            ],
            focused: 0,
            scope,
            project_overrides,
        }
    }

    fn patch(&self) -> FeatureSettingsPatch {
        let changed = |index: usize| {
            (self.allowed[index] && self.selected[index] != self.initial[index])
                .then_some(self.selected[index])
        };
        FeatureSettingsPatch {
            memory: changed(0),
            subagents: changed(1),
            schedule: changed(2),
            skills: changed(3),
            prompt_templates: changed(4),
            session_transfer: changed(5),
        }
    }
}

impl UiComponent for FeaturesEditor {
    type Output = FeatureSettingsPatch;

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        let options = LABELS
            .iter()
            .enumerate()
            .map(|(index, label)| {
                format!(
                    "[{}] {label}{}",
                    if self.allowed[index] && self.selected[index] {
                        "x"
                    } else {
                        " "
                    },
                    if self.allowed[index] {
                        ""
                    } else {
                        " (disabled by host)"
                    }
                )
            })
            .collect::<Vec<_>>();
        let description = match self.scope {
            SettingsScope::Global if self.project_overrides => {
                "Choose global defaults. This project's overrides still apply."
            }
            SettingsScope::Global => "Choose global defaults for optional capabilities.",
            SettingsScope::Project => {
                "Choose this project's capabilities. Unset flags inherit global defaults."
            }
        };
        Ok(UiSurfaceFrame {
            shortcuts: vec![UiShortcut {
                key: "space".into(),
            }],
            regions: vec![UiRegion {
                id: "features".into(),
                placement: UiRegionPlacement::Overlay,
                overlay: Some(UiOverlayOptions {
                    width: Some(86),
                    ..Default::default()
                }),
                root: UiNode::new(
                    "features-panel",
                    UiNodeKind::Panel {
                        title: format!(
                            "Features — {}",
                            if self.scope == SettingsScope::Global {
                                "global"
                            } else {
                                "project"
                            }
                        ),
                        child: Box::new(UiNode::new(
                            "features-body",
                            UiNodeKind::Column {
                                gap: 1,
                                children: vec![
                                    UiNode::text("description", description),
                                    UiNode::new(
                                        "feature-options",
                                        UiNodeKind::Select {
                                            options,
                                            initial_selected: 0,
                                        },
                                    ),
                                    UiNode::new(
                                        "hint",
                                        UiNodeKind::Text {
                                            text:
                                                "↑↓ move · Space toggle · Enter save · Esc cancel"
                                                    .into(),
                                            tone: UiTone::Muted,
                                        },
                                    ),
                                ],
                            },
                        )),
                    },
                ),
            }],
            ..Default::default()
        })
    }

    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<Self::Output>> {
        match event {
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Cancel,
                ..
            } => Ok(UiComponentUpdate::Cancel),
            UiSurfaceEvent::Interaction {
                node_id,
                event: UiInteraction::Change { value },
                ..
            } if node_id == "feature-options" => {
                let index = value
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < LABELS.len())
                    .ok_or_else(|| {
                        PluginContextError::Failed("Invalid feature selection".into())
                    })?;
                self.focused = index;
                Ok(UiComponentUpdate::Continue(Vec::new()))
            }
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Shortcut { key },
                ..
            } if key == "space" => {
                if self.allowed[self.focused] {
                    self.selected[self.focused] = !self.selected[self.focused];
                }
                Ok(UiComponentUpdate::Continue(Vec::new()))
            }
            UiSurfaceEvent::Interaction {
                node_id,
                event: UiInteraction::Submit { .. },
                ..
            } if node_id == "feature-options" => Ok(UiComponentUpdate::Complete(self.patch())),
            _ => Ok(UiComponentUpdate::Continue(Vec::new())),
        }
    }
}
