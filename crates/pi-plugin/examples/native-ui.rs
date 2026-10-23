//! Build with `cargo build -p pi-plugin --example native-ui --features native`.
//! Load the resulting library with `pi --native-plugin /absolute/path/to/libnative_ui.dylib`
//! (use `.so` on Linux or `.dll` on Windows), then run `/rust-ui`.

use pi_plugin::prelude::*;
use std::sync::Arc;

#[derive(Default)]
struct NativeUiPlugin;

#[pi_plugin::native_plugin]
impl Plugin for NativeUiPlugin {
    fn register(&self, context: &mut RegisterContext<'_>) -> Result<()> {
        context.register_command(Arc::new(PanelCommand))?;
        Ok(())
    }
}

struct PanelCommand;

#[async_trait]
impl Command for PanelCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "rust-ui".into(),
            description: "Open a native component on the shared UI surface".into(),
            argument_hint: None,
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        _: String,
    ) -> std::result::Result<CommandOutcome, CommandError> {
        if let Some(value) = context
            .ui
            .custom(Counter(0), context.signal().clone())
            .await?
        {
            context
                .ui
                .notify(NoticeLevel::Info, format!("Native UI result: {value}"))?;
        }
        Ok(CommandOutcome::Handled)
    }
}

struct Counter(u32);

impl UiComponent for Counter {
    type Output = u32;

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        Ok(UiSurfaceFrame {
            regions: vec![UiRegion {
                id: "counter".into(),
                placement: UiRegionPlacement::Overlay,
                root: UiNode::new(
                    "panel",
                    UiNodeKind::Panel {
                        title: "Rust UI counter".into(),
                        child: Box::new(UiNode::new(
                            "content",
                            UiNodeKind::Column {
                                gap: 1,
                                children: vec![
                                    UiNode::text("value", format!("Count: {}", self.0)),
                                    UiNode::new(
                                        "increment",
                                        UiNodeKind::Button {
                                            text: "Increment".into(),
                                        },
                                    ),
                                    UiNode::new(
                                        "accept",
                                        UiNodeKind::Button {
                                            text: "Accept".into(),
                                        },
                                    ),
                                ],
                            },
                        )),
                    },
                ),
                overlay: Some(UiOverlayOptions {
                    width: Some(48),
                    ..Default::default()
                }),
            }],
            ..Default::default()
        })
    }

    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<u32>> {
        if let UiSurfaceEvent::Interaction { node_id, event, .. } = event {
            match event {
                UiInteraction::Activate if node_id == "increment" => self.0 += 1,
                UiInteraction::Activate if node_id == "accept" => {
                    return Ok(UiComponentUpdate::Complete(self.0));
                }
                UiInteraction::Cancel => return Ok(UiComponentUpdate::Cancel),
                _ => {}
            }
        }
        Ok(UiComponentUpdate::Continue(Vec::new()))
    }
}
