use std::path::{Path, PathBuf};

use pi_core::{AbortHandle, AbortSignal, ContentBlock, ToolCallId, ToolResult};
use pi_plugin::{Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_edit::EditTool;
use pi_plugin_write::WriteTool;
use pi_tool_support::with_file_mutation_queue;
use serde_json::{Value, json};

fn result_text(result: &ToolResult) -> &str {
    match &result.content[0] {
        ContentBlock::Text(value) => &value.text,
        other => panic!("expected text result, got {other:?}"),
    }
}

fn execution_message(error: &ToolError) -> &str {
    match error {
        ToolError::Execution(message) => message,
        other => panic!("expected execution error, got {other:?}"),
    }
}

async fn execute_edit_with_signal(
    cwd: PathBuf,
    signal: AbortSignal,
    input: Value,
) -> Result<ToolResult, ToolError> {
    let (updates, _) = ToolUpdateSink::channel();
    EditTool
        .execute(
            ToolContext::standalone(cwd, signal),
            ToolCallId::new("legacy-edit-parity"),
            input,
            updates,
        )
        .await
}

async fn execute_edit(cwd: &Path, input: Value) -> Result<ToolResult, ToolError> {
    let (_, signal) = AbortHandle::new();
    execute_edit_with_signal(cwd.to_path_buf(), signal, input).await
}

async fn execute_write_with_signal(
    cwd: PathBuf,
    signal: AbortSignal,
    input: Value,
) -> Result<ToolResult, ToolError> {
    let (updates, _) = ToolUpdateSink::channel();
    WriteTool
        .execute(
            ToolContext::standalone(cwd, signal),
            ToolCallId::new("legacy-write-parity"),
            input,
            updates,
        )
        .await
}

async fn execute_write(cwd: &Path, input: Value) -> Result<ToolResult, ToolError> {
    let (_, signal) = AbortHandle::new();
    execute_write_with_signal(cwd.to_path_buf(), signal, input).await
}

fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_source_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(text[start..=index].to_string());
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push(text[start..].to_string());
    }
    lines
}

fn parse_old_hunk_range(header: &str) -> (usize, usize) {
    let old = header
        .split_ascii_whitespace()
        .nth(1)
        .expect("old hunk range")
        .strip_prefix('-')
        .expect("old hunk prefix");
    let (start, count) = old
        .split_once(',')
        .map_or((old, "1"), |(start, count)| (start, count));
    (
        start.parse().expect("old hunk start"),
        count.parse().expect("old hunk count"),
    )
}

/// Independent, deliberately small unified-patch applier for validating the patch returned by the
/// public tool. It supports the ordinary two-file patches emitted by `similar`, including missing
/// final-newline markers.
fn apply_unified_patch(original: &str, patch: &str) -> String {
    let source = split_source_lines(original);
    let patch_lines = patch.split('\n').collect::<Vec<_>>();
    assert!(
        patch_lines
            .first()
            .is_some_and(|line| line.starts_with("--- "))
    );
    assert!(
        patch_lines
            .get(1)
            .is_some_and(|line| line.starts_with("+++ "))
    );

    let mut output = String::new();
    let mut source_index = 0;
    let mut index = 2;
    while index < patch_lines.len() {
        if patch_lines[index].is_empty() {
            index += 1;
            continue;
        }
        let header = patch_lines[index];
        assert!(
            header.starts_with("@@ "),
            "unexpected patch line: {header:?}"
        );
        let (old_start, old_count) = parse_old_hunk_range(header);
        let hunk_source_index = if old_count == 0 {
            old_start
        } else {
            old_start.saturating_sub(1)
        };
        assert!(hunk_source_index >= source_index);
        for line in &source[source_index..hunk_source_index] {
            output.push_str(line);
        }
        source_index = hunk_source_index;
        index += 1;

        while index < patch_lines.len() && !patch_lines[index].starts_with("@@ ") {
            let line = patch_lines[index];
            if line.is_empty() {
                break;
            }
            assert_ne!(line, "\\ No newline at end of file");
            let missing_newline = patch_lines
                .get(index + 1)
                .is_some_and(|next| *next == "\\ No newline at end of file");
            let value = if missing_newline {
                line[1..].to_string()
            } else {
                format!("{}\n", &line[1..])
            };
            match line.as_bytes()[0] {
                b' ' => {
                    assert_eq!(source.get(source_index), Some(&value));
                    output.push_str(&value);
                    source_index += 1;
                }
                b'-' => {
                    assert_eq!(source.get(source_index), Some(&value));
                    source_index += 1;
                }
                b'+' => output.push_str(&value),
                prefix => panic!("unexpected patch prefix {prefix:?} in {line:?}"),
            }
            index += if missing_newline { 2 } else { 1 };
        }
    }
    for line in &source[source_index..] {
        output.push_str(line);
    }
    output
}

#[tokio::test]
async fn write_overwrites_and_creates_parents_from_context_cwd() {
    let root = tempfile::tempdir().unwrap();
    let result = execute_write(
        root.path(),
        json!({"path":"nested/dir/file.txt", "content":"first"}),
    )
    .await
    .unwrap();
    assert_eq!(
        result_text(&result),
        "Successfully wrote to nested/dir/file.txt"
    );
    assert!(result.details.is_none());
    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/dir/file.txt")).unwrap(),
        "first"
    );

    execute_write(
        root.path(),
        json!({"path":"nested/dir/file.txt", "content":"overwritten via context cwd"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/dir/file.txt")).unwrap(),
        "overwritten via context cwd"
    );
}

#[tokio::test]
async fn single_edit_uses_context_cwd_and_returns_an_applicable_patch() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.txt");
    let original = "Hello, world!";
    std::fs::write(&path, original).unwrap();

    let result = execute_edit(
        root.path(),
        json!({"path":"file.txt", "edits":[{"oldText":"world", "newText":"testing"}]}),
    )
    .await
    .unwrap();

    assert_eq!(
        result_text(&result),
        "Successfully replaced 1 block(s) in file.txt."
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Hello, testing!");
    let details = result.details.as_ref().expect("edit details");
    let diff = details["diff"].as_str().unwrap();
    let patch = details["patch"].as_str().unwrap();
    assert!(diff.contains("testing"));
    assert!(patch.contains("--- file.txt"));
    assert!(patch.contains("+++ file.txt"));
    assert!(patch.contains("@@"));
    assert!(patch.contains("-Hello, world!"));
    assert!(patch.contains("+Hello, testing!"));
    assert_eq!(apply_unified_patch(original, patch), "Hello, testing!");
}

#[tokio::test]
async fn missing_duplicate_empty_and_overlapping_edits_are_rejected_atomically() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.txt");

    let cases = [
        (
            "alpha\nbeta\ngamma\n",
            json!({"path":"file.txt", "edits":[
                {"oldText":"alpha\n", "newText":"ALPHA\n"},
                {"oldText":"missing\n", "newText":"MISSING\n"}
            ]}),
            "Could not find",
        ),
        (
            "foo foo foo",
            json!({"path":"file.txt", "edits":[{"oldText":"foo", "newText":"bar"}]}),
            "Found 3 occurrences",
        ),
        (
            "one\ntwo\nthree\n",
            json!({"path":"file.txt", "edits":[
                {"oldText":"one\ntwo\n", "newText":"ONE\nTWO\n"},
                {"oldText":"two\nthree\n", "newText":"TWO\nTHREE\n"}
            ]}),
            "overlap",
        ),
        (
            "completely different content\n",
            json!({"path":"file.txt", "edits":[{"oldText":"this does not exist", "newText":"replacement"}]}),
            "Could not find the exact text",
        ),
    ];

    for (before, input, message) in cases {
        std::fs::write(&path, before).unwrap();
        let error = execute_edit(root.path(), input).await.unwrap_err();
        assert!(
            error.to_string().contains(message),
            "expected {message:?}, got {error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    let before = "hello\nworld\n";
    std::fs::write(&path, before).unwrap();
    let error = execute_edit(root.path(), json!({"path":"file.txt", "edits":[]}))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("edits must contain at least one replacement")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn missing_edit_target_reports_pi_enoent_wording() {
    let root = tempfile::tempdir().unwrap();
    let error = execute_edit(
        root.path(),
        json!({"path":"missing.txt", "edits":[{"oldText":"hello", "newText":"world"}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(
        execution_message(&error),
        "Could not edit file: missing.txt. Error code: ENOENT."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_edit_target_reports_pi_eacces_wording_without_mutation() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("readonly.txt");
    std::fs::write(&path, "hello\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let error = execute_edit(
        root.path(),
        json!({"path":"readonly.txt", "edits":[{"oldText":"hello", "newText":"world"}]}),
    )
    .await
    .unwrap_err();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        execution_message(&error),
        "Could not edit file: readonly.txt. Error code: EACCES."
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "hello\n");
}

#[cfg(unix)]
#[tokio::test]
async fn unknown_edit_access_errors_preserve_original_os_message() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("directory")).unwrap();

    let error = execute_edit(
        root.path(),
        json!({"path":"directory", "edits":[{"oldText":"hello", "newText":"world"}]}),
    )
    .await
    .unwrap_err();

    let message = execution_message(&error);
    assert!(message.starts_with("Could not edit file: directory. "));
    assert!(message.contains("Is a directory"), "{message}");
}

#[tokio::test]
async fn multi_edit_matches_original_regions_and_collapses_distant_diff_context() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("multi.txt");
    std::fs::write(&path, "foo\nbar\nbaz\n").unwrap();
    execute_edit(
        root.path(),
        json!({"path":"multi.txt", "edits":[
            {"oldText":"foo\n", "newText":"foo bar\n"},
            {"oldText":"bar\n", "newText":"BAR\n"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "foo bar\nBAR\nbaz\n"
    );

    let lines = (1..=600)
        .map(|line| format!("line {line:03}"))
        .collect::<Vec<_>>();
    let original = format!("{}\n", lines.join("\n"));
    std::fs::write(&path, &original).unwrap();
    let result = execute_edit(
        root.path(),
        json!({"path":"multi.txt", "edits":[
            {"oldText":"line 100\n", "newText":"LINE 100\n"},
            {"oldText":"line 300\n", "newText":"LINE 300\n"},
            {"oldText":"line 500\n", "newText":"LINE 500\n"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(
        result_text(&result),
        "Successfully replaced 3 block(s) in multi.txt."
    );
    let details = result.details.unwrap();
    let diff = details["diff"].as_str().unwrap();
    assert!(diff.contains("LINE 100"));
    assert!(diff.contains("LINE 300"));
    assert!(diff.contains("LINE 500"));
    assert!(diff.contains("..."));
    assert!(!diff.contains("line 250"));
    assert!(diff.lines().count() < 50, "diff was not bounded:\n{diff}");
    let expected = original
        .replace("line 100\n", "LINE 100\n")
        .replace("line 300\n", "LINE 300\n")
        .replace("line 500\n", "LINE 500\n");
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    assert_eq!(
        apply_unified_patch(&original, details["patch"].as_str().unwrap()),
        expected
    );
}

#[tokio::test]
async fn prepare_arguments_matches_legacy_and_stringified_input_contract() {
    let root = tempfile::tempdir().unwrap();
    let (_, signal) = AbortHandle::new();
    let context = ToolContext::standalone(root.path().to_path_buf(), signal);

    let schema = EditTool.spec().parameters;
    assert!(schema.pointer("/properties/oldText").is_none());
    assert!(schema.pointer("/properties/newText").is_none());
    assert!(schema.pointer("/properties/edits").is_some());

    let cases = [
        (
            json!({"path":"file.txt", "oldText":"before", "newText":"after"}),
            json!({"path":"file.txt", "edits":[{"oldText":"before", "newText":"after"}]}),
        ),
        (
            json!({
                "path":"file.txt",
                "edits":[{"oldText":"a", "newText":"b"}],
                "oldText":"c",
                "newText":"d"
            }),
            json!({"path":"file.txt", "edits":[
                {"oldText":"a", "newText":"b"},
                {"oldText":"c", "newText":"d"}
            ]}),
        ),
        (
            json!({"path":"file.txt", "edits":"[{\"oldText\":\"a\",\"newText\":\"b\"}]"}),
            json!({"path":"file.txt", "edits":[{"oldText":"a", "newText":"b"}]}),
        ),
        (
            json!({"path":"file.txt", "edits":"{\"oldText\":\"a\",\"newText\":\"b\"}"}),
            json!({"path":"file.txt", "edits":[{"oldText":"a", "newText":"b"}]}),
        ),
        (
            json!({"path":"file.txt", "edits":{"oldText":"a", "newText":"b"}}),
            json!({"path":"file.txt", "edits":[{"oldText":"a", "newText":"b"}]}),
        ),
        (
            json!({"path":"file.txt", "edits":"not json"}),
            json!({"path":"file.txt", "edits":"not json"}),
        ),
        (Value::Null, Value::Null),
        (
            Value::String("garbage".into()),
            Value::String("garbage".into()),
        ),
    ];
    for (input, expected) in cases {
        let prepared = EditTool.prepare_arguments(&context, input).await.unwrap();
        assert_eq!(prepared, expected);
    }

    let valid = json!({"path":"file.txt", "edits":[{"oldText":"a", "newText":"b"}]});
    assert_eq!(
        EditTool
            .prepare_arguments(&context, valid.clone())
            .await
            .unwrap(),
        valid
    );

    std::fs::write(root.path().join("legacy.txt"), "before\n").unwrap();
    let prepared = EditTool
        .prepare_arguments(
            &context,
            json!({"path":"legacy.txt", "oldText":"before", "newText":"after"}),
        )
        .await
        .unwrap();
    let result = execute_edit(root.path(), prepared).await.unwrap();
    assert_eq!(
        result_text(&result),
        "Successfully replaced 1 block(s) in legacy.txt."
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("legacy.txt")).unwrap(),
        "after\n"
    );
}

#[tokio::test]
async fn fuzzy_unicode_equivalence_matches_legacy_cases() {
    let root = tempfile::tempdir().unwrap();
    let cases = [
        (
            "trailing.txt",
            "line one   \nline two  \nline three\n",
            "line one\nline two\n",
            "replaced\n",
            "replaced\nline three\n",
        ),
        (
            "fullwidth-punctuation.txt",
            "你好，世界\n你好（世界）\n",
            "你好,世界\n你好(世界)\n",
            "你好，pi\n你好(pi)\n",
            "你好，pi\n你好(pi)\n",
        ),
        (
            "nfkc.txt",
            "ＡＢＣ１２３\ncafe\u{0301}\n",
            "ABC123\ncafé\n",
            "XYZ789\ncoffee\n",
            "XYZ789\ncoffee\n",
        ),
        (
            "smart-single.txt",
            "console.log(‘hello’);\n",
            "console.log('hello');",
            "console.log('world');",
            "console.log('world');\n",
        ),
        (
            "smart-double.txt",
            "const msg = “Hello World”;\n",
            "const msg = \"Hello World\";",
            "const msg = \"Goodbye\";",
            "const msg = \"Goodbye\";\n",
        ),
        (
            "dashes.txt",
            "range: 1–5\nbreak—here\n",
            "range: 1-5\nbreak-here",
            "range: 10-50\nbreak--here",
            "range: 10-50\nbreak--here\n",
        ),
        (
            "nbsp.txt",
            "hello\u{00a0}world\n",
            "hello world",
            "hello universe",
            "hello universe\n",
        ),
    ];

    for (name, before, old_text, new_text, expected) in cases {
        std::fs::write(root.path().join(name), before).unwrap();
        let result = execute_edit(
            root.path(),
            json!({"path":name, "edits":[{"oldText":old_text, "newText":new_text}]}),
        )
        .await
        .unwrap();
        assert!(result_text(&result).contains("Successfully replaced"));
        assert_eq!(
            std::fs::read_to_string(root.path().join(name)).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn fuzzy_matching_prefers_exact_but_counts_fuzzy_equivalent_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("exact.txt");
    std::fs::write(&path, "const x = 'exact';\nconst y = 'other';\n").unwrap();
    execute_edit(
        root.path(),
        json!({"path":"exact.txt", "edits":[{
            "oldText":"const x = 'exact';", "newText":"const x = 'changed';"
        }]}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "const x = 'changed';\nconst y = 'other';\n"
    );

    let before = "hello world   \nhello world\n";
    std::fs::write(&path, before).unwrap();
    let error = execute_edit(
        root.path(),
        json!({"path":"exact.txt", "edits":[{"oldText":"hello world", "newText":"replaced"}]}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("Found 2 occurrences"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn fuzzy_multi_edit_preserves_untouched_bytes_and_patch_target_occurrences() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fuzzy.txt");
    let original = [
        "keep before  ",
        "first target  ",
        "first after",
        "keep middle   ",
        "second target  ",
        "second after",
        "keep after  ",
        "",
    ]
    .join("\n");
    std::fs::write(&path, &original).unwrap();
    let result = execute_edit(
        root.path(),
        json!({"path":"fuzzy.txt", "edits":[
            {"oldText":"first target\nfirst after", "newText":"FIRST\nFIRST2"},
            {"oldText":"second target\nsecond after", "newText":"SECOND\nSECOND2"}
        ]}),
    )
    .await
    .unwrap();
    let expected = [
        "keep before  ",
        "FIRST",
        "FIRST2",
        "keep middle   ",
        "SECOND",
        "SECOND2",
        "keep after  ",
        "",
    ]
    .join("\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(
        apply_unified_patch(
            &normalize_lf(&original),
            result.details.as_ref().unwrap()["patch"].as_str().unwrap()
        ),
        normalize_lf(&expected)
    );

    let nearby = "replace me   \nafter   \n";
    std::fs::write(&path, nearby).unwrap();
    let result = execute_edit(
        root.path(),
        json!({"path":"fuzzy.txt", "edits":[{"oldText":"replace me\n", "newText":"after\n"}]}),
    )
    .await
    .unwrap();
    let expected = "after\nafter   \n";
    assert_eq!(std::fs::read_to_string(path).unwrap(), expected);
    assert_eq!(
        apply_unified_patch(
            &normalize_lf(nearby),
            result.details.as_ref().unwrap()["patch"].as_str().unwrap()
        ),
        expected
    );
}

#[tokio::test]
async fn crlf_lf_bom_and_mixed_endings_follow_the_replaced_line() {
    let root = tempfile::tempdir().unwrap();
    let cases = [
        (
            "crlf.txt",
            "first\r\nsecond\r\nthird\r\n",
            "second\n",
            "REPLACED\n",
            "first\r\nREPLACED\r\nthird\r\n",
        ),
        (
            "lf.txt",
            "first\nsecond\nthird\n",
            "second\n",
            "REPLACED\n",
            "first\nREPLACED\nthird\n",
        ),
        (
            "insert-crlf.txt",
            "first\r\nsecond\r\nthird",
            "second\nthird",
            "2a\r\n2b\nthird!",
            "first\r\n2a\r\n2b\r\nthird!",
        ),
        (
            "insert-lf.txt",
            "first\nsecond\nthird",
            "second\nthird",
            "2a\r\n2b\nthird!",
            "first\n2a\n2b\nthird!",
        ),
        (
            "lf-first.txt",
            "a\nb\r\nX = 1\r\n",
            "X = 1",
            "X = 2",
            "a\nb\r\nX = 2\r\n",
        ),
        ("crlf-first.txt", "a\r\nb\nc", "c", "C", "a\r\nb\nC"),
        (
            "lone-cr.txt",
            "a\rb\nX = 1\n",
            "X = 1",
            "X = 2",
            "a\rb\nX = 2\n",
        ),
        (
            "mixed-fuzzy.txt",
            "a\r\nfoo   \r\nb\n",
            "foo\n",
            "FOO\n",
            "a\r\nFOO\r\nb\n",
        ),
    ];

    for (name, before, old_text, new_text, expected) in cases {
        std::fs::write(root.path().join(name), before.as_bytes()).unwrap();
        execute_edit(
            root.path(),
            json!({"path":name, "edits":[{"oldText":old_text, "newText":new_text}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join(name)).unwrap(),
            expected.as_bytes()
        );
    }

    let bom_path = root.path().join("bom.txt");
    std::fs::write(&bom_path, "\u{feff}first\r\nsecond\r\nthird\r\nfourth\r\n").unwrap();
    execute_edit(
        root.path(),
        json!({"path":"bom.txt", "edits":[
            {"oldText":"second\n", "newText":"SECOND\n"},
            {"oldText":"fourth\n", "newText":"FOURTH\n"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(bom_path).unwrap(),
        "\u{feff}first\r\nSECOND\r\nthird\r\nFOURTH\r\n"
    );

    let mixed_path = root.path().join("mixed-multi.txt");
    std::fs::write(&mixed_path, "lf1\nlf2\ncr1\r\ncr2\r\n").unwrap();
    execute_edit(
        root.path(),
        json!({"path":"mixed-multi.txt", "edits":[
            {"oldText":"lf1\n", "newText":"A\nB\n"},
            {"oldText":"cr1\n", "newText":"C\nD\n"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(mixed_path).unwrap(),
        b"A\nB\nlf2\nC\r\nD\r\ncr2\r\n"
    );
}

#[tokio::test]
async fn crlf_and_lf_variants_count_as_fuzzy_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("duplicates.txt");
    let before = "hello\r\nworld\r\n---\r\nhello\nworld\n";
    std::fs::write(&path, before).unwrap();
    let error = execute_edit(
        root.path(),
        json!({"path":"duplicates.txt", "edits":[{"oldText":"hello\nworld\n", "newText":"replaced\n"}]}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("Found 2 occurrences"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[tokio::test]
async fn cr_lf_splice_seams_remain_two_logical_line_breaks_and_patch_applies() {
    let root = tempfile::tempdir().unwrap();
    let cases = [
        ("b", "\nB", "a\r\n\nB\nc", "a\n\nB\nc"),
        ("b", "", "a\r\n\nc", "a\n\nc"),
    ];
    for (index, (old_text, new_text, written, diffed)) in cases.into_iter().enumerate() {
        let name = format!("seam-{index}.txt");
        std::fs::write(root.path().join(&name), "a\rb\nc").unwrap();
        let result = execute_edit(
            root.path(),
            json!({"path":name, "edits":[{"oldText":old_text, "newText":new_text}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join(&name)).unwrap(),
            written
        );
        assert_eq!(
            apply_unified_patch(
                "a\nb\nc",
                result.details.as_ref().unwrap()["patch"].as_str().unwrap()
            ),
            diffed
        );
    }
}

#[tokio::test]
async fn deterministic_random_mixed_endings_normalize_to_patch_result() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("random.txt");
    let mut seed = 1_u64;
    let mut random = |n: usize| -> usize {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
        ((seed as u128 * n as u128) / 0x8000_0000_u128) as usize
    };
    let endings = ["\n", "\r\n", "\r", ""];
    let replacements = ["", "\n", "X\n", "\nY", "\r", "Z"];
    let mut successful = 0;

    for run in 0..500 {
        let mut original = String::new();
        for _ in 0..random(12) + 1 {
            original.push_str(&format!(
                "w{}{}{}",
                random(100),
                if random(5) == 0 { "  " } else { "" },
                endings[random(4)]
            ));
        }
        let normalized = normalize_lf(&original);
        let start = random(normalized.len());
        let length = 1 + random(12).min(normalized.len() - start - 1);
        let old_text = &normalized[start..start + length];
        let new_text = normalize_lf(replacements[random(replacements.len())]);
        let occurrences = normalized.match_indices(old_text).collect::<Vec<_>>();
        if occurrences.len() != 1 {
            continue;
        }
        // Pi's uniqueness count is non-overlapping (String.split/matches), and the selected
        // replacement is the first match. An overlapping second occurrence can therefore be the
        // randomly sampled slice without being the range the public edit tool replaces.
        let matched_start = occurrences[0].0;
        let mut expected = normalized.clone();
        expected.replace_range(matched_start..matched_start + old_text.len(), &new_text);
        if expected == normalized {
            continue;
        }

        std::fs::write(&path, &original).unwrap();
        let result = match execute_edit(
            root.path(),
            json!({"path":"random.txt", "edits":[{"oldText":old_text, "newText":new_text}]}),
        )
        .await
        {
            Ok(result) => result,
            Err(error) if error.to_string().contains("occurrences") => {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
                continue;
            }
            Err(error) => panic!("unexpected random-case error: {error}"),
        };
        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            normalize_lf(&actual),
            expected,
            "run {run}: original={original:?}, normalized={normalized:?}, sampled_start={start}, matched_start={matched_start}, old={old_text:?}, new={new_text:?}, actual={actual:?}"
        );
        let patch = result.details.as_ref().unwrap()["patch"].as_str().unwrap();
        assert_eq!(
            apply_unified_patch(&normalized, patch),
            expected,
            "patch mismatch in run {run}: old={old_text:?}, new={new_text:?}"
        );
        successful += 1;
    }
    assert!(
        successful > 250,
        "too few generated non-ambiguous cases: {successful}"
    );
}

#[tokio::test]
async fn small_edit_in_twenty_megabyte_file_has_bounded_diff_and_patch_output() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.txt");
    let filler = format!("{}\n", "x".repeat(99));
    let half = filler.repeat(100_000);
    let original = format!("{half}TARGET = 1\n{half}");
    std::fs::write(&path, original).unwrap();

    let result = execute_edit(
        root.path(),
        json!({"path":"big.txt", "edits":[{"oldText":"TARGET = 1", "newText":"TARGET = 2"}]}),
    )
    .await
    .unwrap();
    let details = result.details.unwrap();
    let diff = details["diff"].as_str().unwrap();
    let patch = details["patch"].as_str().unwrap();
    let changes = diff
        .lines()
        .filter(|line| line.starts_with(['+', '-']))
        .collect::<Vec<_>>();
    assert_eq!(changes, ["-100001 TARGET = 1", "+100001 TARGET = 2"]);
    assert!(
        diff.len() < 4 * 1024,
        "display diff grew with the 20 MB file"
    );
    assert!(patch.len() < 4 * 1024, "patch grew with the 20 MB file");
}

#[tokio::test(flavor = "current_thread")]
async fn parallel_real_edits_on_one_file_preserve_both_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("parallel.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let holder = tokio::spawn(with_file_mutation_queue(path.clone(), async move {
        let _ = entered_tx.send(());
        let _ = release_rx.await;
        Ok(())
    }));
    entered_rx.await.unwrap();

    let cwd = root.path().to_path_buf();
    let (_, first_signal) = AbortHandle::new();
    let first = tokio::spawn(execute_edit_with_signal(
        cwd.clone(),
        first_signal,
        json!({"path":"parallel.txt", "edits":[{"oldText":"one", "newText":"ONE"}]}),
    ));
    tokio::task::yield_now().await;
    let (_, second_signal) = AbortHandle::new();
    let second = tokio::spawn(execute_edit_with_signal(
        cwd,
        second_signal,
        json!({"path":"parallel.txt", "edits":[{"oldText":"two", "newText":"TWO"}]}),
    ));

    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "ONE\nTWO\n");
}

#[tokio::test(flavor = "current_thread")]
async fn real_write_then_edit_share_one_ordered_mutation_queue() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ordered.txt");
    std::fs::write(&path, "before\n").unwrap();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let holder_path = path.clone();
    let holder = tokio::spawn(with_file_mutation_queue(holder_path, async move {
        let _ = entered_tx.send(());
        let _ = release_rx.await;
        Ok(())
    }));
    entered_rx.await.unwrap();

    let cwd = root.path().to_path_buf();
    let (_, write_signal) = AbortHandle::new();
    let write = tokio::spawn(execute_write_with_signal(
        cwd.clone(),
        write_signal,
        json!({"path":"ordered.txt", "content":"alpha\n"}),
    ));
    tokio::task::yield_now().await;
    let (_, edit_signal) = AbortHandle::new();
    let edit = tokio::spawn(execute_edit_with_signal(
        cwd,
        edit_signal,
        json!({"path":"ordered.txt", "edits":[{"oldText":"alpha", "newText":"ALPHA"}]}),
    ));
    tokio::task::yield_now().await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before\n");

    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    write.await.unwrap().unwrap();
    edit.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "ALPHA\n");
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_real_edit_keeps_its_queue_position_before_following_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancelled-edit.txt");
    std::fs::write(&path, "before\n").unwrap();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let holder = tokio::spawn(with_file_mutation_queue(path.clone(), async move {
        let _ = entered_tx.send(());
        let _ = release_rx.await;
        Ok(())
    }));
    entered_rx.await.unwrap();

    let cwd = root.path().to_path_buf();
    let (abort_edit, edit_signal) = AbortHandle::new();
    let edit = tokio::spawn(execute_edit_with_signal(
        cwd.clone(),
        edit_signal,
        json!({"path":"cancelled-edit.txt", "edits":[{"oldText":"before", "newText":"after"}]}),
    ));
    tokio::task::yield_now().await;
    abort_edit.abort();
    let (_, write_signal) = AbortHandle::new();
    let write = tokio::spawn(execute_write_with_signal(
        cwd,
        write_signal,
        json!({"path":"cancelled-edit.txt", "content":"written\n"}),
    ));
    tokio::task::yield_now().await;

    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    assert!(matches!(edit.await.unwrap(), Err(ToolError::Aborted)));
    write.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "written\n");
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_real_write_keeps_its_queue_position_before_following_edit() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancelled.txt");
    std::fs::write(&path, "before\n").unwrap();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let holder_path = path.clone();
    let holder = tokio::spawn(with_file_mutation_queue(holder_path, async move {
        let _ = entered_tx.send(());
        let _ = release_rx.await;
        Ok(())
    }));
    entered_rx.await.unwrap();

    let cwd = root.path().to_path_buf();
    let (abort_write, write_signal) = AbortHandle::new();
    let write = tokio::spawn(execute_write_with_signal(
        cwd.clone(),
        write_signal,
        json!({"path":"cancelled.txt", "content":"must not be written\n"}),
    ));
    tokio::task::yield_now().await;
    abort_write.abort();
    let (_, edit_signal) = AbortHandle::new();
    let edit = tokio::spawn(execute_edit_with_signal(
        cwd,
        edit_signal,
        json!({"path":"cancelled.txt", "edits":[{"oldText":"before", "newText":"after"}]}),
    ));
    tokio::task::yield_now().await;

    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    assert!(matches!(write.await.unwrap(), Err(ToolError::Aborted)));
    edit.await.unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "after\n");
}
