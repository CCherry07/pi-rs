use pi_core::{
    ContentBlock, ContextCategory, ContextCategoryUsage, ContextRequestUsage, ContextUsageItem,
    CustomMessageContent, Message, PromptContribution,
};
use pi_plugin::{FrozenRegistries, ProviderRequest, ToolOrigin};

fn tokens(bytes: usize) -> u64 {
    u64::try_from(bytes.div_ceil(4)).unwrap_or(u64::MAX)
}

fn content_bytes(content: &[ContentBlock]) -> usize {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.len(),
            ContentBlock::Thinking(thinking) => thinking.thinking.len(),
            // Never tokenize base64 data as text. Image usage is model-specific.
            ContentBlock::Image(_) => 4_800,
            ContentBlock::ToolCall(call) => call.name.len() + call.arguments.to_string().len(),
        })
        .sum()
}

fn message_tokens(message: &Message) -> u64 {
    tokens(match message {
        Message::System(message) => message.render().len(),
        Message::User(message) => content_bytes(&message.content),
        Message::Assistant(message) => content_bytes(&message.content),
        Message::ToolResult(message) => content_bytes(&message.content),
        Message::Custom(message) => match &message.content {
            CustomMessageContent::Text(text) => text.len(),
            CustomMessageContent::Blocks(blocks) => content_bytes(blocks),
        },
    })
}

pub(crate) fn estimate_request(
    request: &ProviderRequest,
    registries: &FrozenRegistries,
    contributions: &[PromptContribution],
) -> ContextRequestUsage {
    use ContextCategory::{McpTools, MemoryFiles, Messages, Skills, SystemPrompt, SystemTools};
    let mut categories = [
        Messages,
        McpTools,
        SystemTools,
        SystemPrompt,
        Skills,
        MemoryFiles,
    ]
    .map(|category| ContextCategoryUsage {
        category,
        tokens: 0,
        items: Vec::new(),
    });
    let mut add = |category, name: String, tokens| {
        let usage = categories
            .iter_mut()
            .find(|usage| usage.category == category)
            .unwrap();
        usage.tokens = usage.tokens.saturating_add(tokens);
        usage.items.push(ContextUsageItem { name, tokens });
    };
    add(
        Messages,
        format!("{} messages", request.messages.len()),
        request.messages.iter().map(message_tokens).sum(),
    );
    for tool in &request.tools {
        let category = if registries
            .tool_discovery(&tool.name)
            .is_some_and(|info| info.origin == ToolOrigin::Mcp)
        {
            McpTools
        } else {
            SystemTools
        };
        // Count model-facing schema fields only, never execution or UI metadata.
        let schema = serde_json::json!({"name": tool.name, "description": tool.description, "parameters": tool.parameters});
        add(
            category,
            tool.name.clone(),
            tokens(schema.to_string().len()),
        );
    }
    let mut namespace_sources = Vec::new();
    let mut namespaces = std::collections::HashSet::new();
    for tool in &request.tools {
        if let Some(info) = registries.tool_discovery(&tool.name)
            && let Some(namespace) = &info.namespace
            && namespaces.insert(&namespace.name)
            && let Some(instructions) = namespace
                .instructions
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
        {
            namespace_sources.push(PromptContribution {
                category: if info.origin == ToolOrigin::Mcp {
                    McpTools
                } else {
                    SystemTools
                },
                name: format!("{} instructions", namespace.name),
                text: format!(
                    "\n\nInstructions for tools in namespace `{}`:\n{instructions}",
                    namespace.name
                ),
            });
        }
    }
    let mut claimed = Vec::<std::ops::Range<usize>>::new();
    for source in contributions
        .iter()
        .chain(namespace_sources.iter())
        .filter(|source| !source.text.is_empty())
    {
        let occurrence = request
            .system_prompt
            .match_indices(&source.text)
            .map(|(start, text)| start..start + text.len())
            .find(|range| {
                !claimed
                    .iter()
                    .any(|other| range.start < other.end && other.start < range.end)
            });
        if let Some(range) = occurrence {
            add(source.category, source.name.clone(), tokens(range.len()));
            claimed.push(range);
        }
    }
    let attributed_bytes: usize = claimed.iter().map(std::ops::Range::len).sum();
    add(
        SystemPrompt,
        "System prompt".into(),
        tokens(request.system_prompt.len().saturating_sub(attributed_bytes)),
    );
    let mut undeclared_mcp_tools = registries
        .tool_discoveries()
        .filter(|(name, info)| {
            info.origin == ToolOrigin::Mcp && !request.tools.iter().any(|tool| tool.name == *name)
        })
        .map(|(name, _)| name.to_string())
        .collect::<Vec<_>>();
    undeclared_mcp_tools.sort();
    ContextRequestUsage {
        model: request.model.to_string(),
        estimated_tokens: categories.iter().map(|usage| usage.tokens).sum(),
        categories: categories.into(),
        undeclared_mcp_tools,
    }
}
