//! Provider-specific user maintenance commands.

mod render;

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    RegisterContext,
};

use crate::runtime::LocalMemoryRuntime;
use crate::{FastEmbedModelState, RecallQuery};

const COMMAND_LIMIT: usize = 50;

pub(crate) fn register(
    context: &mut RegisterContext<'_>,
    runtime: &LocalMemoryRuntime,
) -> pi_core::Result<()> {
    for kind in LocalMemoryCommandKind::ALL {
        context.register_command(Arc::new(LocalMemoryCommand {
            runtime: runtime.clone(),
            kind,
        }))?;
    }
    Ok(())
}

struct LocalMemoryCommand {
    runtime: LocalMemoryRuntime,
    kind: LocalMemoryCommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMemoryCommandKind {
    Status,
    List,
    Search,
    Rebuild,
    ModelStatus,
    ModelInstall,
    ModelBackfill,
}

impl LocalMemoryCommandKind {
    const ALL: [Self; 7] = [
        Self::Status,
        Self::List,
        Self::Search,
        Self::Rebuild,
        Self::ModelStatus,
        Self::ModelInstall,
        Self::ModelBackfill,
    ];

    fn spec(self) -> CommandSpec {
        let (name, description, argument_hint) = match self {
            Self::Status => (
                "memory-local-status",
                "Inspect local semantic-memory storage and model health",
                None,
            ),
            Self::List => (
                "memory-local-list",
                "List local memories in the current scopes",
                Some("[query]"),
            ),
            Self::Search => (
                "memory-local-search",
                "Search local memories in the current scopes",
                Some("<query>"),
            ),
            Self::Rebuild => (
                "memory-local-rebuild",
                "Rebuild the local memory index from session journals",
                None,
            ),
            Self::ModelStatus => (
                "memory-local-model-status",
                "Inspect the local embedding model",
                None,
            ),
            Self::ModelInstall => (
                "memory-local-model-install",
                "Install and activate the local embedding model",
                None,
            ),
            Self::ModelBackfill => (
                "memory-local-model-backfill",
                "Backfill missing vectors in the local memory index",
                None,
            ),
        };
        CommandSpec {
            name: name.to_string(),
            description: description.to_string(),
            argument_hint: argument_hint.map(str::to_string),
        }
    }

    fn usage(self) -> String {
        let spec = self.spec();
        match spec.argument_hint {
            Some(arguments) => format!("usage: /{} {arguments}", spec.name),
            None => format!("usage: /{}", spec.name),
        }
    }
}

#[async_trait]
impl Command for LocalMemoryCommand {
    fn spec(&self) -> CommandSpec {
        self.kind.spec()
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        ensure_not_aborted(&context)?;
        let arguments = arguments.trim();
        match self.kind {
            LocalMemoryCommandKind::Status => {
                reject_arguments(self.kind, arguments)?;
                let health = self.runtime.health().await.map_err(execution_error)?;
                ensure_not_aborted(&context)?;
                let model = self.runtime.embedding_model_status();
                let model_degraded = model.runtime_error.is_some()
                    || matches!(&model.model.state, FastEmbedModelState::Invalid { .. });
                let level =
                    if health.is_healthy() && health.recovered_from.is_none() && !model_degraded {
                        NoticeLevel::Info
                    } else {
                        NoticeLevel::Warning
                    };
                context.ui.notify(level, render::health(&health, &model))?;
            }
            LocalMemoryCommandKind::List | LocalMemoryCommandKind::Search => {
                let query = query_argument(self.kind, arguments)?;
                let session_id = context.session.id()?;
                let result = self
                    .runtime
                    .recall(RecallQuery {
                        text: query,
                        scopes: self.runtime.scopes(&session_id),
                        limit: COMMAND_LIMIT,
                    })
                    .await
                    .map_err(execution_error)?;
                ensure_not_aborted(&context)?;
                context
                    .ui
                    .notify(NoticeLevel::Info, render::hits(&result.hits))?;
            }
            LocalMemoryCommandKind::Rebuild => {
                reject_arguments(self.kind, arguments)?;
                let receipt = self.runtime.rebuild().await.map_err(execution_error)?;
                ensure_not_aborted(&context)?;
                // A new in-memory session may not have a materialized JSONL
                // file yet, so reconcile the live snapshot after the global
                // scan before reporting success.
                self.runtime
                    .reconcile_snapshot(context.session.snapshot()?)
                    .await
                    .map_err(execution_error)?;
                let embedding = self
                    .runtime
                    .backfill_embeddings_if_active()
                    .await
                    .map_err(execution_error)?;
                context.ui.notify(
                    NoticeLevel::Info,
                    format!(
                        "Memory index rebuilt\n\nFiles: {} scanned, {} skipped\nSessions: {}\nSession entries: {}\nMutations: {} applied, {} duplicate{}",
                        receipt.source_files,
                        receipt.skipped_files,
                        receipt.sessions,
                        receipt.session_entries,
                        receipt.mutations,
                        receipt.duplicate_mutations,
                        render::optional_backfill(embedding.as_ref()),
                    ),
                )?;
            }
            LocalMemoryCommandKind::ModelStatus => {
                reject_arguments(self.kind, arguments)?;
                let status = self.runtime.embedding_model_status();
                let level = if status.runtime_error.is_some()
                    || matches!(&status.model.state, FastEmbedModelState::Invalid { .. })
                {
                    NoticeLevel::Warning
                } else {
                    NoticeLevel::Info
                };
                context.ui.notify(level, render::model_status(&status))?;
            }
            LocalMemoryCommandKind::ModelInstall => {
                reject_arguments(self.kind, arguments)?;
                context.ui.notify(
                    NoticeLevel::Info,
                    "Installing the pinned multilingual embedding model. The verified download is about 465 MiB and may take several minutes.",
                )?;
                let installed = self
                    .runtime
                    .install_embedding_model()
                    .await
                    .map_err(execution_error)?;
                ensure_not_aborted(&context)?;
                let replacement = context.session.reload().await.map_err(|error| {
                    execution_error(format!(
                        "model assets were installed and {} records were indexed, but the runtime reload failed: {error}",
                        installed.backfill.indexed
                    ))
                })?;
                replacement
                    .ui
                    .notify(NoticeLevel::Info, render::model_install(&installed))?;
            }
            LocalMemoryCommandKind::ModelBackfill => {
                reject_arguments(self.kind, arguments)?;
                let backfill = self
                    .runtime
                    .backfill_embeddings()
                    .await
                    .map_err(execution_error)?;
                ensure_not_aborted(&context)?;
                context.ui.notify(
                    NoticeLevel::Info,
                    format!(
                        "Embedding backfill complete\n\nAttempted: {}\nIndexed: {}\nRemaining: {}",
                        backfill.attempted, backfill.indexed, backfill.remaining
                    ),
                )?;
            }
        }
        Ok(CommandOutcome::Handled)
    }
}

fn reject_arguments(kind: LocalMemoryCommandKind, arguments: &str) -> Result<(), CommandError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(CommandError::InvalidArguments(kind.usage()))
    }
}

fn query_argument(kind: LocalMemoryCommandKind, arguments: &str) -> Result<String, CommandError> {
    if kind == LocalMemoryCommandKind::Search && arguments.is_empty() {
        return Err(CommandError::InvalidArguments(kind.usage()));
    }
    Ok(arguments.to_string())
}

fn ensure_not_aborted(context: &CommandContext) -> Result<(), CommandError> {
    if context.signal().is_aborted() {
        Err(CommandError::Aborted)
    } else {
        Ok(())
    }
}

fn execution_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::Execution(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_commands_are_provider_specific_and_complete() {
        let specs = LocalMemoryCommandKind::ALL.map(LocalMemoryCommandKind::spec);
        let names = specs.map(|spec| spec.name);
        assert_eq!(
            names,
            [
                "memory-local-status",
                "memory-local-list",
                "memory-local-search",
                "memory-local-rebuild",
                "memory-local-model-status",
                "memory-local-model-install",
                "memory-local-model-backfill",
            ]
        );
    }

    #[test]
    fn only_list_accepts_an_empty_query() {
        assert_eq!(
            query_argument(LocalMemoryCommandKind::List, "").unwrap(),
            ""
        );
        assert_eq!(
            query_argument(LocalMemoryCommandKind::Search, "rust").unwrap(),
            "rust"
        );
        assert!(query_argument(LocalMemoryCommandKind::Search, "").is_err());
        assert!(reject_arguments(LocalMemoryCommandKind::Status, "extra").is_err());
    }
}
