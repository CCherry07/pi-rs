use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::AbortHandle;
use pi_shell::{
    ShellChunk, ShellError, ShellOperations, ShellOperationsRequest, ShellRequest, ShellResult,
    ShellStream, execute_with_operations,
};

struct UserChunks {
    chunks: Vec<Vec<u8>>,
    abort: Option<AbortHandle>,
}

#[async_trait]
impl ShellOperations for UserChunks {
    async fn execute(&self, request: ShellOperationsRequest) -> Result<Option<i32>, ShellError> {
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

async fn capture(chunks: Vec<Vec<u8>>, cancelled: bool) -> (ShellResult, Vec<ShellChunk>) {
    let (abort, signal) = AbortHandle::new();
    let streamed = Arc::new(Mutex::new(Vec::new()));
    let callback_streamed = Arc::clone(&streamed);
    let result = execute_with_operations(
        ShellRequest {
            command: "user-shell".into(),
            cwd: "/remote/virtual".into(),
            timeout: None,
            shell_path: None,
            abort_signal: signal,
            on_chunk: Some(Arc::new(move |chunk| {
                callback_streamed.lock().unwrap().push(chunk);
            })),
        },
        &UserChunks {
            chunks,
            abort: cancelled.then_some(abort),
        },
    )
    .await
    .unwrap();
    let chunks = streamed.lock().unwrap().clone();
    (result, chunks)
}

// Expected values follow the actual ANSI grammar in utils/ansi.ts and the
// sanitizeBinaryOutput/CR rules in core/bash-executor.ts, not a generic terminal
// emulator. Run the real public user-operations adapter at every byte split.
#[tokio::test]
async fn sanitizes_split_terminal_sequences_and_utf8() {
    for (input, expected) in [
        ("plain\t🙂 €\n", "plain\t🙂 €\n"),
        ("\x1b[31mred\x1b[0m\r\n", "red\n"),
        (
            "a\x1b]8;;https://example.com\x07link\x1b]8;;\x07z",
            "alinkz",
        ),
        ("a\x1b]title\x1b\\b", "ab"),
        ("a\x1b]title\u{9c}b", "ab"),
        ("a\u{9b}31mred\u{9b}0mz", "aredz"),
        ("\x1b[<35;59;1Mstep 1", "step 1"),
        ("a\x1b[>0;276;0cb", "ab"),
        ("a\x1b[>cb", "ab"),
        ("a\x1b[=5hb", "ab"),
        ("a\u{9b}<35;59;1Mb", "ab"),
        ("\x1b[?1h\x1b=123 tests passed", "123 tests passed"),
        ("\x1b[?1l\x1b>42 files", "42 files"),
        ("\x1b<10 items", "10 items"),
        ("\x1bcdone", "done"),
        ("a\x1b[38:2::1:2:3mb", "ab"),
        (
            "a\0\x07\x08\x0b\x0c\x0e\x1f\t\u{fff9}b\u{fffa}\u{fffb}\r\nc\r",
            "a\tb\nc",
        ),
        (
            "\u{feff}first\u{feff}\x7f\u{90}\u{9d}",
            "first\u{feff}\x7f\u{90}\u{9d}",
        ),
        ("\x1b", ""),
        ("\x1b[", "["),
        ("\x1b[31", ""),
        ("x\x1b]unterminated", "xnterminated"),
        ("x\x1b]window title", "x]window title"),
        ("x\u{9b}", "x\u{9b}"),
    ] {
        let bytes = input.as_bytes();
        for split in 0..=bytes.len() {
            let (result, chunks) = capture(
                vec![bytes[..split].to_vec(), bytes[split..].to_vec()],
                false,
            )
            .await;
            assert_eq!(result.output, expected, "input={input:?}, split={split}");
            assert_eq!(result.exit_code, Some(0));
            assert!(!result.cancelled);
            assert!(!result.timed_out);
            assert!(!result.truncated);
            assert!(result.full_output_path.is_none());
            assert!(
                chunks
                    .iter()
                    .all(|chunk| chunk.stream == ShellStream::Stdout && !chunk.text.is_empty())
            );
            let streamed = chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            assert_eq!(
                streamed, expected,
                "callback: input={input:?}, split={split}"
            );
        }
    }
}

#[tokio::test]
async fn holds_only_a_bounded_ansi_suffix_measured_in_utf16_units() {
    let pending = format!("before\x1b]{}", "🙂".repeat(127));
    let (result, chunks) = capture(vec![pending.into_bytes(), b"\x07after".to_vec()], false).await;
    assert_eq!(result.output, "beforeafter");
    assert_eq!(chunks[0].text, "before");
    assert_eq!(chunks[1].text, "after");

    // With 257 UTF-16 units after ESC, Pi releases the unfinished sequence
    // rather than growing pending state without bound. Only ESC/BEL disappear.
    let pending = format!("before\x1b]{}", "x".repeat(255));
    let (result, _) = capture(vec![pending.into_bytes(), b"\x07after".to_vec()], false).await;
    assert_eq!(result.output, format!("before]{}after", "x".repeat(255)));
}

#[tokio::test]
async fn flushes_incomplete_utf8_and_ansi_on_success_and_cancellation() {
    for cancelled in [false, true] {
        let (result, chunks) =
            capture(vec![b"ready\r\n\x1b[".to_vec(), vec![0xe2]], cancelled).await;
        assert_eq!(result.output, "ready\n[�");
        assert_eq!(result.cancelled, cancelled);
        assert_eq!(result.exit_code, if cancelled { None } else { Some(0) });
        assert!(!result.truncated);
        let streamed = chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        assert_eq!(streamed, "ready\n[�");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn local_user_cancellation_keeps_sanitized_output_without_an_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    let (abort, signal) = AbortHandle::new();
    let streamed = Arc::new(Mutex::new(String::new()));
    let callback_streamed = Arc::clone(&streamed);
    let result = pi_shell::execute_user_shell(ShellRequest {
        command: "printf '\\033[31mready\\033[0m\\r\\n'; sleep 30".into(),
        cwd: directory.path().into(),
        timeout: Some(std::time::Duration::from_secs(3)),
        shell_path: None,
        abort_signal: signal,
        on_chunk: Some(Arc::new(move |chunk| {
            let mut output = callback_streamed.lock().unwrap();
            output.push_str(&chunk.text);
            if output.as_str() == "ready\n" {
                abort.abort();
            }
        })),
    })
    .await
    .unwrap();
    assert_eq!(result.output, "ready\n");
    assert_eq!(*streamed.lock().unwrap(), "ready\n");
    assert!(result.cancelled);
    assert!(!result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(!result.truncated);
}

#[tokio::test]
async fn raw_byte_spill_threshold_persists_only_sanitized_user_text() {
    let (result, chunks) = capture(
        vec![
            b"visible\r\n".to_vec(),
            "\x1b[31m\x1b[0m".repeat(6_000).into_bytes(),
            b"tail\r\n".to_vec(),
        ],
        false,
    )
    .await;
    assert_eq!(result.output, "visible\ntail\n");
    assert!(!result.truncated);
    assert!(result.truncation.is_none());
    let streamed = chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<String>();
    assert_eq!(streamed, result.output);
    let path = result.full_output_path.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "visible\ntail\n");
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn user_rolling_buffer_counts_utf16_units_without_dropping_retained_unicode() {
    // 55,000 UTF-16 units fit the executor's 100 Ki-unit rolling buffer even
    // though their 115,000 UTF-8 bytes do not. The 50 KiB model tail retains
    // 25,000 ASCII bytes plus 8,733 complete three-byte CJK characters.
    let first = "一".repeat(30_000);
    let last = "a".repeat(25_000);
    let (result, _) = capture(
        vec![first.as_bytes().to_vec(), last.as_bytes().to_vec()],
        false,
    )
    .await;
    assert_eq!(result.output, format!("{}{}", "一".repeat(8_733), last));
    assert!(result.truncated);
    let path = result.full_output_path.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first + &last);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn truncated_user_output_and_full_files_stay_sanitized_after_cancellation() {
    let expected = (0..3_000)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let chunks = (0..3_000)
        .map(|line| format!("\x1b[32mline {line}\x1b[0m\0\r\n").into_bytes())
        .collect::<Vec<_>>();
    for cancelled in [false, true] {
        let (result, streamed) = capture(chunks.clone(), cancelled).await;
        assert_eq!(result.cancelled, cancelled);
        assert!(result.truncated);
        assert_eq!(result.output.lines().count(), 2_000);
        assert!(result.output.starts_with("line 1000\n"));
        assert!(result.output.ends_with("line 2999"));
        assert!(!result.output.contains(['\x1b', '\0', '\r']));
        let streamed = streamed
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        assert_eq!(streamed, expected);
        let path = result.full_output_path.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        std::fs::remove_file(path).unwrap();
    }
}
