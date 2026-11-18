//! Custom execution backends share the shell's streaming capture and output limits.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::AbortSignal;
use tokio::sync::mpsc;

use super::{OutputCapture, ShellError, ShellRequest, ShellResult, ShellStream, record_chunk};

pub struct ShellOperationsRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub signal: AbortSignal,
    pub on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
}

#[async_trait]
pub trait ShellOperations: Send + Sync {
    async fn execute(&self, request: ShellOperationsRequest) -> Result<Option<i32>, ShellError>;
}

/// Runs an injected backend while retaining ordinary shell capture, streaming and cancellation.
pub async fn execute_with_operations(
    request: ShellRequest,
    operations: &dyn ShellOperations,
) -> Result<ShellResult, ShellError> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let execution = operations.execute(ShellOperationsRequest {
        command: request.command,
        cwd: request.cwd,
        signal: request.abort_signal.clone(),
        on_data: Arc::new(move |bytes| {
            let _ = sender.send(bytes);
        }),
    });
    let mut execution = std::pin::pin!(execution);
    let mut capture = OutputCapture::default();
    let mut chunks_open = true;
    let result = loop {
        tokio::select! {
            biased;
            chunk = receiver.recv(), if chunks_open => match chunk {
                Some(bytes) => record_chunk(&mut capture, request.on_chunk.as_ref(), ShellStream::Stdout, &bytes).await?,
                None => chunks_open = false,
            },
            result = &mut execution => break result,
        }
    };
    while let Ok(bytes) = receiver.try_recv() {
        record_chunk(
            &mut capture,
            request.on_chunk.as_ref(),
            ShellStream::Stdout,
            &bytes,
        )
        .await?;
    }
    let cancelled = request.abort_signal.is_aborted();
    let exit_code = if cancelled { None } else { result? };
    capture.finish().await?;
    let truncation = capture.truncation();
    let output = truncation
        .as_ref()
        .map_or_else(|| capture.tail_text(), |value| value.content.clone());
    Ok(ShellResult {
        output,
        exit_code,
        cancelled,
        timed_out: false,
        shell_exit_after: None,
        truncated: truncation.is_some(),
        truncation,
        full_output_path: capture.full_output_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LargeOutput(String);

    #[async_trait]
    impl ShellOperations for LargeOutput {
        async fn execute(
            &self,
            request: ShellOperationsRequest,
        ) -> Result<Option<i32>, ShellError> {
            assert_eq!(request.cwd, PathBuf::from("/remote/virtual"));
            (request.on_data)(self.0.as_bytes().to_vec());
            Ok(Some(0))
        }
    }

    #[tokio::test]
    async fn custom_operations_capture_full_output_and_return_the_limited_tail() {
        let source = (0..3_000)
            .map(|line| format!("remote line {line}\n"))
            .collect::<String>();
        let (_, signal) = pi_core::AbortHandle::new();
        let result = execute_with_operations(
            ShellRequest {
                command: "remote".into(),
                cwd: "/remote/virtual".into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            &LargeOutput(source.clone()),
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.truncated);
        assert_eq!(
            result.output.lines().count(),
            super::super::MAX_OUTPUT_LINES
        );
        assert!(result.output.ends_with("remote line 2999"));
        let path = result.full_output_path.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), source);
        tokio::fs::remove_file(path).await.unwrap();
    }
}
