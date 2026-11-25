use super::*;
use rmcp::model::LoggingLevel;
use serde_json::json;

#[test]
fn format_matches_pinned_timestamp_level_logger_and_multiline_contract() {
    let mut params =
        LoggingMessageNotificationParam::new(LoggingLevel::Warning, json!("one\r\ntwo\nthree"));
    params.logger = Some("worker".into());
    let now = OffsetDateTime::from_unix_timestamp(1_700_000_000)
        .unwrap()
        .replace_millisecond(123)
        .unwrap();
    assert_eq!(
        format_message("example", &params, now),
        "2023-11-14T22:13:20.123Z [example] warning worker: one\n    two\n    three\n"
    );
    params.data = json!({"ready":true});
    params.logger = Some(String::new());
    assert!(format_message("example", &params, now).ends_with("warning {\"ready\":true}\n"));
}

#[test]
fn appends_rotates_only_above_limit_and_protects_both_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.log");
    let log = ServerLog::new(path.clone(), "test".into(), Redactor::default());
    let file = private_file(&path, true).unwrap();
    file.set_len(MAX_LOG_BYTES).unwrap();
    log.write(LoggingMessageNotificationParam::new(
        LoggingLevel::Info,
        json!("first"),
    ));
    assert!(!dir.path().join("mcp.log.1").exists());
    log.write(LoggingMessageNotificationParam::new(
        LoggingLevel::Error,
        json!("second"),
    ));
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .ends_with("[test] error second\n")
    );
    let rotated = dir.path().join("mcp.log.1");
    assert!(
        fs::read_to_string(&rotated)
            .unwrap()
            .ends_with("[test] info first\n")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&path, &rotated, &dir.path().join("mcp.log.lock")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn concurrent_library_writers_keep_each_multiline_record_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.log");
    std::thread::scope(|scope| {
        for index in 0..8 {
            let log = ServerLog::new(path.clone(), format!("server-{index}"), Redactor::default());
            scope.spawn(move || {
                for line in 0..30 {
                    log.write(LoggingMessageNotificationParam::new(
                        LoggingLevel::Info,
                        json!(format!("{line}\ncontinuation-{index}-{line}")),
                    ));
                }
            });
        }
    });
    let text = fs::read_to_string(path).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 8 * 30 * 2);
    for pair in lines.as_chunks::<2>().0 {
        let (_, message) = pair[0].split_once("[server-").unwrap();
        let (server, line) = message.split_once("] info ").unwrap();
        assert_eq!(pair[1], format!("    continuation-{server}-{line}"));
    }
}

#[test]
fn unavailable_sink_and_busy_cross_process_lock_are_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("not-a-directory");
    fs::write(&parent, "preserve").unwrap();
    let params = LoggingMessageNotificationParam::new(LoggingLevel::Info, json!("hello"));
    ServerLog::new(parent.join("mcp.log"), "test".into(), Redactor::default())
        .write(params.clone());
    assert_eq!(fs::read_to_string(parent).unwrap(), "preserve");
    let lock = private_file(&dir.path().join("mcp.log.lock"), false).unwrap();
    lock.lock_exclusive().unwrap();
    ServerLog::new(
        dir.path().join("mcp.log"),
        "test".into(),
        Redactor::default(),
    )
    .write(params);
    assert!(!dir.path().join("mcp.log").exists());
}

#[test]
fn log_redacts_configured_credentials_and_terminal_controls() {
    let transport = McpTransport::Http {
        url: "https://invalid.example/mcp?key=hidden".into(),
        headers: std::collections::BTreeMap::from([(
            "Authorization".into(),
            "Bearer fake-secret".into(),
        )]),
    };
    let redactor = Redactor::new(&transport, None);
    assert_eq!(
        redactor.text("\u{1b}fake-secret Bearer fake-secret"),
        "[REDACTED] [REDACTED]"
    );
}
