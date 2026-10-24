//! Native components use the same declarative UI protocol as JavaScript plugins.
//!
//! Components describe content and respond to semantic events. Rust owns layout, focus, editing
//! state and rendering. Mount components from an existing plugin callback using
//! [`UiContext::custom`](crate::UiContext::custom); there is no separate renderer registry.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use crate::{
    AbortSignal, PluginContextError, PluginContextHandle, PluginContextResult, PresentationMode,
    UiCommand, UiSurfaceCommand, UiSurfaceEvent, UiSurfaceFrame,
};

/// Result of a component event. Effects execute in order before the next frame is published.
pub enum UiComponentUpdate<T> {
    Continue(Vec<UiCommand>),
    Complete(T),
    Cancel,
}

/// A stateful, generation-owned native component. Implement `Drop` for resource cleanup.
///
/// The driver assigns document ownership and monotonically increasing revisions. Components
/// receive semantic control events; normal editing keys never require a plugin round trip.
pub trait UiComponent: Send {
    type Output: Send;

    /// Retain the waker when background state or animation needs to invalidate this component.
    fn mount(&mut self, _waker: UiComponentWaker) -> PluginContextResult<()> {
        Ok(())
    }

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame>;

    /// Dismissal cancels the component; other unhandled semantic events leave it mounted.
    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<Self::Output>> {
        Ok(
            if matches!(
                event,
                UiSurfaceEvent::Interaction {
                    event: crate::UiInteraction::Cancel,
                    ..
                }
            ) {
                UiComponentUpdate::Cancel
            } else {
                UiComponentUpdate::Continue(Vec::new())
            },
        )
    }
}

struct Lifetime {
    handle: PluginContextHandle,
    owner: String,
    closed: AtomicBool,
}

/// A cloneable invalidation handle. It cannot mount a surface or keep one alive after close.
#[derive(Clone)]
pub struct UiComponentWaker {
    lifetime: Arc<Lifetime>,
}

impl UiComponentWaker {
    pub fn request_render(&self) -> PluginContextResult<()> {
        if self.lifetime.closed.load(Ordering::Acquire) {
            return Err(PluginContextError::Unavailable(
                "UI component closed".into(),
            ));
        }
        self.lifetime.handle.access_for_adapter()?.ui_command(
            self.lifetime.owner.clone(),
            UiCommand::Surface(UiSurfaceCommand::Wake),
        )
    }
}

struct SurfaceLease(Arc<Lifetime>);

impl Drop for SurfaceLease {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        if let Ok(access) = self.0.handle.access_for_adapter() {
            let _ = access.ui_command(
                self.0.owner.clone(),
                UiCommand::Surface(UiSurfaceCommand::Close),
            );
        }
    }
}

fn callback<T>(name: &str, f: impl FnOnce() -> PluginContextResult<T>) -> PluginContextResult<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|error| {
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        PluginContextError::Failed(format!("UI component {name} panicked: {message}"))
    })?
}

pub(crate) async fn run_component<C: UiComponent>(
    handle: PluginContextHandle,
    mut component: C,
    signal: AbortSignal,
) -> PluginContextResult<Option<C::Output>> {
    let access = handle.access_for_adapter()?;
    if signal.is_aborted() || access.mode()? != PresentationMode::Tui || !access.has_ui()? {
        return Ok(None);
    }
    let lease = SurfaceLease(Arc::new(Lifetime {
        handle,
        owner: format!("native-ui:{}", uuid::Uuid::new_v4()),
        closed: AtomicBool::new(false),
    }));
    callback("mount", || {
        component.mount(UiComponentWaker {
            lifetime: lease.0.clone(),
        })
    })?;
    let mut revision = 0;
    let mut document = None;
    loop {
        // Recheck after every await: stale callbacks must never run against the next generation.
        let access = lease.0.handle.access_for_adapter()?;
        if signal.is_aborted() {
            return Ok(None);
        }
        let mut frame = callback("render", || component.render())?;
        frame.owner = lease.0.owner.clone();
        frame.revision = revision;
        frame.validate().map_err(PluginContextError::Failed)?;
        if document.as_ref() != Some(&frame) {
            revision += 1;
            frame.revision = revision;
            document = Some(frame.clone());
        }
        let mut cancelled = pin!(signal.wait());
        let mut exchange = pin!(access.ui_surface_exchange(frame));
        let event = poll_fn(|cx| {
            if cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            exchange.as_mut().poll(cx).map(Some)
        })
        .await;
        let Some(event) = event else {
            return Ok(None);
        };
        lease.0.handle.access_for_adapter()?;
        let event = event?;
        match &event {
            UiSurfaceEvent::Closed => return Ok(None),
            UiSurfaceEvent::Interaction {
                revision: event_revision,
                ..
            } if *event_revision != revision => continue,
            _ => {}
        }
        match callback("handle_event", || component.handle_event(event))? {
            UiComponentUpdate::Complete(value) => return Ok(Some(value)),
            UiComponentUpdate::Cancel => return Ok(None),
            UiComponentUpdate::Continue(actions) => {
                for action in actions {
                    if matches!(action, UiCommand::Surface(UiSurfaceCommand::Close)) {
                        return Ok(None);
                    }
                    lease
                        .0
                        .handle
                        .access_for_adapter()?
                        .ui_command(lease.0.owner.clone(), action)?;
                }
            }
        }
    }
}

/// Standard selectors are ordinary generation-owned components, shared with custom UI.
pub(crate) struct Selection {
    pub title: String,
    pub options: Vec<String>,
}

/// The rich multi-selector is an ordinary shared semantic node, not a product bridge side channel.
pub(crate) struct MultiSelection {
    pub request: crate::UiMultiSelectRequest,
}

pub(crate) struct TextEntry {
    pub title: String,
    pub prefill: String,
    pub placeholder: String,
    pub multiline: bool,
}

impl UiComponent for TextEntry {
    type Output = String;

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        use crate::{UiNode, UiNodeKind, UiRegion, UiRegionPlacement, UiTone};
        let entry = if self.multiline {
            UiNodeKind::Editor {
                initial_value: self.prefill.clone(),
                placeholder: self.placeholder.clone(),
                height: 6,
                completions: Vec::new(),
            }
        } else {
            UiNodeKind::Input {
                initial_value: self.prefill.clone(),
                placeholder: self.placeholder.clone(),
                completions: Vec::new(),
            }
        };
        Ok(UiSurfaceFrame {
            regions: vec![UiRegion {
                id: "dialog".into(),
                placement: UiRegionPlacement::Overlay,
                overlay: None,
                root: UiNode::new(
                    "dialog",
                    UiNodeKind::Column {
                        gap: 0,
                        children: vec![
                            UiNode::new(
                                "title",
                                UiNodeKind::Text {
                                    text: self.title.clone(),
                                    tone: UiTone::Accent,
                                },
                            ),
                            UiNode::new("value", entry),
                        ],
                    },
                ),
            }],
            ..Default::default()
        })
    }

    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<String>> {
        Ok(match event {
            UiSurfaceEvent::Interaction {
                node_id,
                event: crate::UiInteraction::Submit { value },
                ..
            } if node_id == "value" => UiComponentUpdate::Complete(
                value
                    .as_str()
                    .ok_or_else(|| {
                        PluginContextError::Failed("UI text result is not a string".into())
                    })?
                    .to_owned(),
            ),
            UiSurfaceEvent::Interaction {
                event: crate::UiInteraction::Cancel,
                ..
            } => UiComponentUpdate::Cancel,
            _ => UiComponentUpdate::Continue(Vec::new()),
        })
    }
}

impl UiComponent for MultiSelection {
    type Output = crate::UiMultiSelectResponse;

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        use crate::{UiNode, UiNodeKind, UiRegion, UiRegionPlacement};
        Ok(UiSurfaceFrame {
            regions: vec![UiRegion {
                id: "multi-select-dialog".into(),
                placement: UiRegionPlacement::Overlay,
                overlay: None,
                root: UiNode::new(
                    "multi-select",
                    UiNodeKind::MultiSelect {
                        request: Box::new(self.request.clone()),
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
        Ok(match event {
            UiSurfaceEvent::Interaction {
                node_id,
                event: crate::UiInteraction::Submit { value },
                ..
            } if node_id == "multi-select" => {
                UiComponentUpdate::Complete(serde_json::from_value(value).map_err(|error| {
                    PluginContextError::Failed(format!("invalid multi-selection result: {error}"))
                })?)
            }
            UiSurfaceEvent::Interaction {
                event: crate::UiInteraction::Cancel,
                ..
            } => UiComponentUpdate::Cancel,
            _ => UiComponentUpdate::Continue(Vec::new()),
        })
    }
}

impl UiComponent for Selection {
    type Output = usize;

    fn render(&mut self) -> PluginContextResult<UiSurfaceFrame> {
        use crate::{UiNode, UiNodeKind, UiRegion, UiRegionPlacement, UiTone};
        Ok(UiSurfaceFrame {
            regions: vec![UiRegion {
                id: "dialog".into(),
                placement: UiRegionPlacement::Overlay,
                overlay: None,
                root: UiNode::new(
                    "dialog",
                    UiNodeKind::Column {
                        gap: 0,
                        children: vec![
                            UiNode::new(
                                "title",
                                UiNodeKind::Text {
                                    text: self.title.clone(),
                                    tone: UiTone::Accent,
                                },
                            ),
                            UiNode::new(
                                "value",
                                UiNodeKind::Select {
                                    options: self.options.clone(),
                                    initial_selected: 0,
                                },
                            ),
                        ],
                    },
                ),
            }],
            ..Default::default()
        })
    }

    fn handle_event(
        &mut self,
        event: UiSurfaceEvent,
    ) -> PluginContextResult<UiComponentUpdate<usize>> {
        use crate::UiInteraction;
        Ok(match event {
            UiSurfaceEvent::Interaction {
                node_id,
                event: UiInteraction::Submit { value },
                ..
            } if node_id == "value" => {
                let selected = value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|index| *index < self.options.len())
                    .ok_or_else(|| {
                        PluginContextError::Failed(
                            "UI selection result is not a valid option index".into(),
                        )
                    })?;
                UiComponentUpdate::Complete(selected)
            }
            UiSurfaceEvent::Interaction {
                event: UiInteraction::Cancel,
                ..
            } => UiComponentUpdate::Cancel,
            _ => UiComponentUpdate::Continue(Vec::new()),
        })
    }
}
