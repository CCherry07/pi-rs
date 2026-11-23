//! Final Coding policy hook for images produced by tools and extensions.

use pi_core::{ContentBlock, PluginId};
use pi_media::image::{ImagePolicy, normalize_tool_result_images};
use pi_plugin::{AgentPluginContext, Plugin, PluginError, ToolResultEvent, ToolResultPatch};

pub(crate) const PLUGIN_ID: &str = "coding-tool-result-image-normalizer";

pub(crate) struct ToolResultImageNormalizer {
    auto_resize: bool,
}

impl ToolResultImageNormalizer {
    pub(crate) fn new(auto_resize: bool) -> Self {
        Self { auto_resize }
    }
}

#[pi_plugin::plugin]
impl Plugin for ToolResultImageNormalizer {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn tool_result(
        &self,
        context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        if !event
            .result
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_)))
        {
            return Ok(ToolResultPatch::default());
        }
        let resize_options = context
            .models
            .current()?
            .and_then(|model| model.input_limits)
            .and_then(|limits| limits.images)
            .and_then(|images| images.resize)
            .unwrap_or_default();
        let policy = ImagePolicy {
            auto_resize: self.auto_resize,
            ..ImagePolicy::default()
        }
        .with_resize_options(resize_options);
        let content = event.result.content;
        let structured_content = event.result.structured_content;
        let normalized =
            tokio::task::spawn_blocking(move || normalize_tool_result_images(&content, &policy))
                .await;

        match normalized {
            Ok(Some(content)) => Ok(ToolResultPatch {
                content: Some(content),
                // A content patch normally invalidates structured output. Normalization only
                // changes the representation of image blocks, so retain the surviving value
                // produced by the tool and all earlier extension hooks.
                structured_content,
                ..ToolResultPatch::default()
            }),
            Ok(None) => Ok(ToolResultPatch::default()),
            Err(error) => {
                // A failed media worker cannot be allowed to erase a tool's already-produced
                // output. This mirrors per-image conversion/resize failures in pi-media.
                context.report_hook_error(
                    "tool_result",
                    format!("image normalization worker failed: {error}"),
                );
                Ok(ToolResultPatch::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_plugin::{AgentHook, AgentHookInterests};

    #[test]
    fn normalizer_is_a_hook_only_generation_plugin() {
        let plugin = ToolResultImageNormalizer::new(true);
        assert_eq!(plugin.id().as_str(), PLUGIN_ID);
        assert_eq!(
            plugin.hook_interests(),
            AgentHookInterests::from_hooks(&[AgentHook::ToolResult])
        );
    }
}
