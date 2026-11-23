//! User shell backends share streaming sanitization, capture and output limits.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::AbortSignal;
use tokio::sync::mpsc;

use super::{
    CapturePolicy, ChunkDecoder, OutputCapture, ShellError, ShellRequest, ShellResult, ShellStream,
    finish_chunk_stream, record_chunk,
};

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

/// Runs a user-shell backend with the same sanitized capture/streaming as local
/// user shell shorthand. Built-in tool execution retains its separate raw path.
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
    let mut capture = OutputCapture::new(CapturePolicy::UserShell, "pi-bash-".into());
    let mut chunk_decoders = [
        ChunkDecoder::new(CapturePolicy::UserShell),
        ChunkDecoder::new(CapturePolicy::UserShell),
    ];
    let mut chunks_open = true;
    let result = loop {
        tokio::select! {
            biased;
            chunk = receiver.recv(), if chunks_open => match chunk {
                Some(bytes) => record_chunk(
                    &mut capture,
                    &mut chunk_decoders,
                    request.on_chunk.as_ref(),
                    ShellStream::Stdout,
                    &bytes,
                ).await?,
                None => chunks_open = false,
            },
            result = &mut execution => break result,
        }
    };
    while let Ok(bytes) = receiver.try_recv() {
        record_chunk(
            &mut capture,
            &mut chunk_decoders,
            request.on_chunk.as_ref(),
            ShellStream::Stdout,
            &bytes,
        )
        .await?;
    }
    finish_chunk_stream(&mut capture, &mut chunk_decoders, request.on_chunk.as_ref()).await?;
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

    struct ChunkedOutput {
        chunks: Vec<Vec<u8>>,
        abort: Option<pi_core::AbortHandle>,
    }

    #[async_trait]
    impl ShellOperations for ChunkedOutput {
        async fn execute(
            &self,
            request: ShellOperationsRequest,
        ) -> Result<Option<i32>, ShellError> {
            for chunk in &self.chunks {
                (request.on_data)(chunk.clone());
            }
            if let Some(abort) = &self.abort {
                abort.abort();
                return Err(ShellError::Process("aborted".into()));
            }
            Ok(Some(0))
        }
    }

    #[tokio::test]
    async fn custom_operations_decode_utf8_split_across_chunks() {
        let euro = "€\n".as_bytes();
        let operations = ChunkedOutput {
            chunks: vec![euro[..1].to_vec(), euro[1..].to_vec()],
            abort: None,
        };
        let streamed = Arc::new(std::sync::Mutex::new(String::new()));
        let callback_output = Arc::clone(&streamed);
        let (_, signal) = pi_core::AbortHandle::new();
        let result = execute_with_operations(
            ShellRequest {
                command: "remote".into(),
                cwd: "/remote/virtual".into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: Some(Arc::new(move |chunk| {
                    callback_output.lock().unwrap().push_str(&chunk.text);
                })),
            },
            &operations,
        )
        .await
        .unwrap();

        assert_eq!(result.output, "€\n");
        assert_eq!(*streamed.lock().unwrap(), "€\n");
        assert!(!result.output.contains('\u{fffd}'));
    }

    #[tokio::test]
    async fn custom_operations_sanitize_user_output_before_capture_and_callbacks() {
        let operations = ChunkedOutput {
            chunks: vec![
                b"\x1b[31mred\x1b[0m\r\n\t\0a\x07b\x08".to_vec(),
                "\u{fff9}c\u{fffa}\u{fffb}\r\n".as_bytes().to_vec(),
            ],
            abort: None,
        };
        let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callback_chunks = Arc::clone(&chunks);
        let (_, signal) = pi_core::AbortHandle::new();
        let result = execute_with_operations(
            ShellRequest {
                command: "remote".into(),
                cwd: "/remote/virtual".into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: Some(Arc::new(move |chunk| {
                    callback_chunks.lock().unwrap().push(chunk);
                })),
            },
            &operations,
        )
        .await
        .unwrap();

        assert_eq!(result.output, "red\n\tabc\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.truncated);
        assert!(result.full_output_path.is_none());
        let chunks = chunks.lock().unwrap();
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.stream == ShellStream::Stdout)
        );
        let streamed = chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        assert_eq!(streamed, "red\n\tabc\n");
    }

    #[tokio::test]
    async fn discarded_chunks_are_persisted_for_success_and_cancellation() {
        let mut first = "error: missing build dependency\n".to_string();
        first.push_str(&"x".repeat(64 * 1024 - first.len()));
        let mut last = "remaining build output\n".to_string();
        last.push_str(&"y".repeat(44 * 1024 - last.len()));
        for cancelled in [false, true] {
            let (abort, signal) = pi_core::AbortHandle::new();
            let operations = ChunkedOutput {
                chunks: vec![first.as_bytes().to_vec(), last.as_bytes().to_vec()],
                abort: cancelled.then_some(abort),
            };
            let result = execute_with_operations(
                ShellRequest {
                    command: "cat build.log".into(),
                    cwd: "/remote/virtual".into(),
                    timeout: None,
                    shell_path: None,
                    abort_signal: signal,
                    on_chunk: None,
                },
                &operations,
            )
            .await
            .unwrap();

            assert_eq!(result.cancelled, cancelled);
            assert_eq!(result.output, last);
            assert!(result.truncated);
            let path = result.full_output_path.unwrap();
            assert_eq!(
                tokio::fs::read_to_string(&path).await.unwrap(),
                first.clone() + &last
            );
            tokio::fs::remove_file(path).await.unwrap();
        }
    }

    #[tokio::test]
    async fn short_custom_output_is_not_truncated_or_persisted() {
        let (_, signal) = pi_core::AbortHandle::new();
        let result = execute_with_operations(
            ShellRequest {
                command: "echo done".into(),
                cwd: "/remote/virtual".into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            &ChunkedOutput {
                chunks: vec![b"done\n".to_vec()],
                abort: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.output, "done\n");
        assert!(!result.truncated);
        assert!(result.full_output_path.is_none());
    }
}
