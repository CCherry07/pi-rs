//! Presentation-neutral accounting for an effective model request.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextCategory {
    Messages,
    McpTools,
    SystemTools,
    SystemPrompt,
    Skills,
    MemoryFiles,
}

/// A source-owned fragment. Only non-overlapping occurrences in the final prompt
/// are attributed; replaced or omitted fragments contribute nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptContribution {
    pub category: ContextCategory,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageItem {
    pub name: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCategoryUsage {
    pub category: ContextCategory,
    pub tokens: u64,
    pub items: Vec<ContextUsageItem>,
}

/// Local estimates of semantic request inputs, not provider billing or wire
/// tokenization. Responses and later unsent input are not part of this snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRequestUsage {
    pub model: String,
    pub estimated_tokens: u64,
    pub categories: Vec<ContextCategoryUsage>,
    /// Inventory only: no schema tokens are charged for undeclared tools.
    pub undeclared_mcp_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBudget {
    pub request: ContextRequestUsage,
    pub context_window: Option<u64>,
    pub reserve_tokens: u64,
    pub free_tokens: Option<u64>,
}
