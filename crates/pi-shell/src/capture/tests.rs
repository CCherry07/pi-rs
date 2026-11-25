use super::*;
use pi_core::AbortHandle;
use std::path::Path;

fn request(directory: &Path, command: &str) -> ShellCaptureRequest {
    let (_, signal) = AbortHandle::new();
    ShellCaptureRequest {
        command: command.into(),
        cwd: directory.into(),
        shell_path: Some("/bin/sh".into()),
        timeout: Duration::from_secs(3),
        signal,
        stdout_limit: 64,
        stderr_limit: 64,
    }
}

#[tokio::test]
async fn separates_raw_streams_status_and_redacted_debug_without_files() {
    let dir = tempfile::tempdir().unwrap();
    let result = capture(request(
        dir.path(),
        "printf 'secret-out'; printf 'secret-err' >&2; exit 7",
    ))
    .await
    .unwrap();
    assert_eq!(result.stdout, b"secret-out");
    assert_eq!(result.stderr, b"secret-err");
    assert_eq!(result.status, ShellCaptureStatus::Exited);
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.stdout_truncated && !result.stderr_truncated);
    assert!(!format!("{result:?}").contains("secret"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let result = capture(request(dir.path(), "printf '\\377'"))
        .await
        .unwrap();
    assert_eq!(
        result.stdout,
        [255],
        "decoding policy belongs to the caller"
    );
}

#[tokio::test]
async fn caps_each_stream_exactly_and_reports_overflow_without_persistence() {
    let dir = tempfile::tempdir().unwrap();
    for (command, out, err, status, truncated) in [
        (
            "printf 1234; printf 1234 >&2",
            4,
            4,
            ShellCaptureStatus::Exited,
            (false, false),
        ),
        (
            "printf 12345; sleep 30",
            4,
            4,
            ShellCaptureStatus::OutputLimit,
            (true, false),
        ),
        (
            "printf 12345 >&2; sleep 30",
            4,
            4,
            ShellCaptureStatus::OutputLimit,
            (false, true),
        ),
        (
            "printf x",
            0,
            0,
            ShellCaptureStatus::OutputLimit,
            (true, false),
        ),
    ] {
        let mut input = request(dir.path(), command);
        input.stdout_limit = out;
        input.stderr_limit = err;
        let result = tokio::time::timeout(Duration::from_secs(2), capture(input))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.status, status);
        assert_eq!(
            (result.stdout_truncated, result.stderr_truncated),
            truncated
        );
        assert!(result.stdout.len() <= out && result.stderr.len() <= err);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn pre_cancel_and_zero_timeout_never_start_a_process() {
    let dir = tempfile::tempdir().unwrap();
    let (abort, signal) = AbortHandle::new();
    abort.abort();
    let mut input = request(dir.path(), "touch forbidden");
    input.signal = signal;
    assert_eq!(
        capture(input).await.unwrap().status,
        ShellCaptureStatus::Cancelled
    );
    let mut input = request(dir.path(), "touch forbidden");
    input.timeout = Duration::ZERO;
    assert_eq!(
        capture(input).await.unwrap().status,
        ShellCaptureStatus::TimedOut
    );
    assert!(!dir.path().join("forbidden").exists());
}

async fn wait_for_heartbeat(directory: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !directory.join("heartbeat").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("descendant started");
}

async fn assert_descendant_stopped(directory: &Path) {
    // Allow a write already in flight when SIGKILL arrived, then prove the writer has stopped.
    tokio::time::sleep(Duration::from_millis(40)).await;
    let before = std::fs::read(directory.join("heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(std::fs::read(directory.join("heartbeat")).unwrap(), before);
    assert!(
        !directory.join("continued").exists(),
        "cancel must not advance a waiting shell"
    );
}

const DESCENDANT: &str = "(while :; do printf x >> heartbeat; sleep 0.01; done) & ";

#[tokio::test]
async fn timeout_cancel_overflow_and_dropped_future_stop_the_process_tree() {
    for mode in ["timeout", "cancel", "overflow", "drop", "parent-exited"] {
        let dir = tempfile::tempdir().unwrap();
        let (abort, signal) = AbortHandle::new();
        let suffix = match mode {
            "parent-exited" => "exit 0",
            "overflow" => {
                "while [ ! -f heartbeat ]; do sleep 0.01; done; printf 12345; wait; touch continued"
            }
            _ => "wait; touch continued",
        };
        let mut input = request(dir.path(), &format!("{DESCENDANT}{suffix}"));
        input.signal = signal;
        input.stdout_limit = 4;
        if matches!(mode, "timeout" | "parent-exited") {
            input.timeout = Duration::from_millis(250);
        }
        let running = tokio::spawn(capture(input));
        wait_for_heartbeat(dir.path()).await;
        match mode {
            "cancel" => abort.abort(),
            "drop" => running.abort(),
            _ => {}
        }
        let result = tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap();
        if mode == "drop" {
            assert!(result.unwrap_err().is_cancelled());
        } else {
            let result = result.unwrap().unwrap();
            assert_eq!(
                result.status,
                match mode {
                    "cancel" => ShellCaptureStatus::Cancelled,
                    "overflow" => ShellCaptureStatus::OutputLimit,
                    _ => ShellCaptureStatus::TimedOut,
                }
            );
        }
        assert_descendant_stopped(dir.path()).await;
    }
}
