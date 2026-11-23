//! Offline guard for the reviewed legacy tool-test inventory. This checks that
//! every definition has a disposition and its Rust tests still exist; the tests
//! themselves, not this inventory, prove behavior. Refresh with the Node script.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inventory {
    schema_version: u32,
    oracle_revision: String,
    files: Vec<OracleFile>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct OracleFile {
    file: String,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: String,
    file: String,
    line: usize,
    title: String,
    definition_hash: String,
    disposition: String,
    #[serde(default)]
    reason: Option<String>,
    rust_tests: Vec<RustTest>,
}

#[derive(Deserialize)]
struct RustTest {
    path: String,
    name: String,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn test_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut pending = false;
    let mut ignored = false;
    for line in source.lines().map(str::trim) {
        if line.starts_with("#[test]") || line.starts_with("#[tokio::test") {
            pending = true;
        } else if line.starts_with("#[ignore") {
            ignored = true;
        } else if let Some(declaration) = line
            .strip_prefix("fn ")
            .or_else(|| line.strip_prefix("async fn "))
        {
            if pending
                && !ignored
                && let Some((name, _)) = declaration.split_once('(')
            {
                names.insert(name.trim().to_string());
            }
            pending = false;
            ignored = false;
        } else if !line.is_empty() && !line.starts_with("#[") && !line.starts_with("//") {
            pending = false;
            ignored = false;
        }
    }
    names
}

#[test]
fn frozen_legacy_tool_cases_have_reviewed_executable_rust_mappings() {
    let inventory: Inventory =
        serde_json::from_str(include_str!("fixtures/tool-test-parity.json")).unwrap();
    assert_eq!(inventory.schema_version, 1);
    assert!(is_hex(&inventory.oracle_revision, 40));
    assert!(!inventory.files.is_empty());
    assert!(!inventory.cases.is_empty());
    let files: BTreeSet<_> = inventory.files.iter().map(|file| &file.file).collect();
    assert_eq!(files.len(), inventory.files.len());
    for file in &inventory.files {
        assert!(is_hex(&file.sha256, 64), "{}", file.file);
    }
    let mut ids = BTreeSet::new();
    let mut covered_files = BTreeSet::new();
    let mut cache = BTreeMap::new();
    let mut missing = Vec::new();
    for case in &inventory.cases {
        assert!(ids.insert(&case.id), "duplicate oracle case: {}", case.id);
        assert!(
            files.contains(&case.file),
            "undeclared source: {}",
            case.file
        );
        covered_files.insert(&case.file);
        assert!(case.line > 0 && !case.title.trim().is_empty());
        assert!(is_hex(&case.definition_hash, 64), "{}", case.id);
        match case.disposition.as_str() {
            "not-applicable" => {
                assert!(
                    case.reason
                        .as_deref()
                        .is_some_and(|reason| !reason.trim().is_empty()),
                    "boundary difference needs a reason: {}",
                    case.id
                );
                assert!(case.rust_tests.is_empty(), "{}", case.id);
            }
            "covered" if !case.rust_tests.is_empty() => {
                for test in &case.rust_tests {
                    let relative = Path::new(&test.path);
                    assert!(
                        relative.extension().is_some_and(|ext| ext == "rs")
                            && relative
                                .components()
                                .all(|component| matches!(component, Component::Normal(_))),
                        "invalid test path: {}",
                        test.path
                    );
                    let names = cache.entry(test.path.clone()).or_insert_with(|| {
                        let path = repository().join(relative);
                        test_names(
                            &fs::read_to_string(&path)
                                .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
                        )
                    });
                    if !names.contains(&test.name) {
                        missing.push(format!(
                            "{} -> missing {}::{}",
                            case.id, test.path, test.name
                        ));
                    }
                }
            }
            _ => missing.push(format!("{} -> not mapped", case.id)),
        }
    }
    assert_eq!(
        covered_files, files,
        "a frozen source has no inventoried tests"
    );
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}

#[test]
fn inventory_references_must_name_tests_not_helpers_or_ignored_cases() {
    let source = r#"
        fn helper() {}
        #[cfg(unix)]
        #[tokio::test]
        async fn real_async() {}
        #[test]
        #[cfg(windows)]
        fn real_windows() {}
        #[test]
        #[ignore = "not validated"]
        fn ignored() {}
        #[ignore = "also not validated"]
        #[test]
        fn ignored_before_test() {}
        // #[test]
        fn commented() {}
    "#;
    assert_eq!(
        test_names(source),
        BTreeSet::from(["real_async".into(), "real_windows".into()])
    );
}
