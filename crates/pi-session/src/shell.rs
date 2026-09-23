use async_trait::async_trait;
use pi_shell::{ShellError, ShellRequest, ShellResult};

/// Optional execution adapter supplied by the product generation.
///
/// The session owns cancellation, operation ordering, events and persistence.
/// The adapter owns how a submitted command executes and its product defaults.
/// Opening or replaying a session never calls this adapter.
#[async_trait]
pub trait SessionShellExecutor: Send + Sync {
    async fn execute(&self, request: ShellRequest) -> Result<ShellResult, ShellError>;
}
