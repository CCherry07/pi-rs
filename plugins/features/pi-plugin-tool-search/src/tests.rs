use pi_core::{AbortHandle, ContentBlock};
use pi_plugin::{ToolDiscovery, ToolExposure, ToolInfo, ToolNamespace};

use super::*;

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        output_schema: None,
        constrained_sampling: None,
        name: name.into(),
        label: name.into(),
        description: description.into(),
        parameters,
        execution_mode: ToolExecutionMode::Parallel,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
    }
}

fn document(name: &str, description: &str) -> ToolSearchDocument {
    create_tool_search_document(&spec(name, description, json!({})), None)
}

fn names(query: &str, documents: &[ToolSearchDocument], limit: usize) -> Vec<String> {
    Bm25Ranker::default()
        .rank(query, documents, limit)
        .into_iter()
        .map(|found| found.name)
        .collect()
}

fn deferred(name: &str, description: &str) -> ToolInfo {
    ToolInfo {
        spec: spec(name, description, json!({"type":"object"})),
        discovery: ToolDiscovery {
            origin: Default::default(),
            exposure: ToolExposure::Deferred,
            namespace: None,
        },
    }
}

fn context(candidates: Vec<ToolInfo>) -> ToolContext {
    let (_, signal) = AbortHandle::new();
    ToolContext::standalone(std::env::temp_dir(), signal).with_discoverable_tools(candidates)
}

async fn execute(context: ToolContext, input: Value) -> Result<ToolResult, ToolError> {
    let (updates, _) = ToolUpdateSink::channel();
    tool_search_tool()
        .execute(context, ToolCallId::new("search"), input, updates)
        .await
}

fn text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn identifier_tokens_match_pi_boundaries_and_english_forms() {
    assert_eq!(
        tokenize("listIssues for the GitHub_repo"),
        ["list", "issue", "git", "hub", "repo"]
    );
    assert_eq!(
        tokenize("searches queries HTTPServer"),
        ["search", "query", "http", "server"]
    );
    assert_eq!(
        tokenize("repo.issues/search:queries"),
        ["repo", "issue", "search", "query"]
    );
    assert!(tokenize(" \t，。!? 😀 ").is_empty());
}

#[test]
fn unicode_words_match_the_current_pi_regression_corpus() {
    for (input, expected) in [
        ("搜索仓库问题", vec!["搜索", "仓库", "问题"]),
        ("文書を検索", vec!["文書", "を", "検索"]),
        ("문서 검색", vec!["문서", "검색"]),
        ("ค้นหาเอกสาร", vec!["ค้นหา", "เอกสาร"]),
        ("Поиск документов", vec!["поиск", "документов"]),
        ("البحث عن الملفات", vec!["البحث", "عن", "الملفات"]),
        ("फ़ाइल खोजें", vec!["फ़ाइल", "खोजें"]),
        ("Éléments cafés", vec!["éléments", "cafés"]),
    ] {
        assert_eq!(tokenize(input), expected, "{input}");
    }
    assert_eq!(
        tokenize("搜索GitHub_repoIssues résumé cafés"),
        ["搜索", "git", "hub", "repo", "issue", "résumé", "cafés"]
    );
    assert_eq!(tokenize("CAFÉ cafe\u{301}"), ["café", "café"]);
}

#[test]
fn localized_queries_find_words_inside_unspaced_descriptions() {
    for (description, query) in [
        ("搜索仓库问题", "问题"),
        ("文書を検索", "検索"),
        ("문서 검색", "검색"),
        ("ค้นหาเอกสาร", "เอกสาร"),
        ("Поиск документов", "документов"),
        ("البحث عن الملفات", "الملفات"),
        ("फ़ाइल खोजें", "खोजें"),
        ("Rechercher un café", "cafe\u{301}"),
    ] {
        let documents = [
            document("irrelevant", "Search the documentation"),
            document("localized", description),
        ];
        assert_eq!(names(query, &documents, 8), ["localized"], "{description}");
    }
}

#[test]
fn ranking_matches_pi_exact_names_coverage_and_name_weight() {
    let documents = [
        document(
            "mcp__app__queue_read",
            "Read a queued message of this thread.",
        ),
        document(
            "mcp__dev-app__thread_read",
            "Read durable state and a paginated timeline from any thread. The default view returns user messages, assistant messages and plans. Continue with afterPosition until hasMore is false.",
        ),
    ];
    for query in [
        "thread_read",
        "dev_app__thread_read",
        "mcp__dev-app__thread_read",
        "Thread_Read",
        "thread read",
    ] {
        assert_eq!(
            names(query, &documents, 1),
            ["mcp__dev-app__thread_read"],
            "{query}"
        );
    }
    assert_eq!(names("read", &documents, 1), ["mcp__app__queue_read"]);

    let documents = [
        document(
            "mcp__peekaboo__app",
            "Launch, quit, focus, or switch applications by name or bundle identifier.",
        ),
        document(
            "mcp__peekaboo__see",
            "Capture the app window and describe the UI elements of the app.",
        ),
        document(
            "mcp__peekaboo__window",
            "Move, resize, or close a window of an app.",
        ),
        document(
            "mcp__peekaboo__image",
            "Take a screenshot of the screen, an app, or a window.",
        ),
    ];
    assert_eq!(
        names("screenshot window", &documents, 1),
        ["mcp__peekaboo__image"]
    );
    assert_eq!(names("describe app", &documents, 1), ["mcp__peekaboo__see"]);
    assert_eq!(names("window", &documents, 1), ["mcp__peekaboo__window"]);

    let documents = [
        document(
            "mcp__ci__status",
            "Deploy status: last deploy, deploy log, deploy errors.",
        ),
        document(
            "mcp__ci__deploy_release",
            "Start a release of the current branch to an environment, run the migrations, switch traffic over, and wait until the new version is live and healthy.",
        ),
    ];
    assert_eq!(names("deploy", &documents, 1), ["mcp__ci__deploy_release"]);
}

#[test]
fn namespace_text_and_whole_local_names_influence_ranking() {
    let namespace = ToolNamespace {
        name: "mcp__peekaboo".into(),
        aliases: Vec::new(),
        description: Some("macOS desktop tools".into()),
        instructions: Some("Peekaboo controls macOS apps by app name.".into()),
    };
    let documents = [
        spec("mcp__peekaboo__see", "Capture the app window of the peekaboo app and describe the app.", json!({})),
        spec("mcp__peekaboo__app", "Launch, quit, relaunch, focus, hide, or switch applications. Targets an application by name, bundle identifier, or process ID, and can wait until it is ready.", json!({})),
    ].iter().map(|tool| create_tool_search_document(tool, Some(&namespace))).collect::<Vec<_>>();
    assert_eq!(names("peekaboo app", &documents, 1), ["mcp__peekaboo__app"]);
    assert_eq!(
        names("open the peekaboo app tool", &documents, 1),
        ["mcp__peekaboo__app"]
    );
    assert_eq!(names("desktop", &documents, 8).len(), 2);
}

#[test]
fn ranker_limits_and_ties_keep_catalog_order() {
    let documents = [
        document("beta", "Search issues"),
        document("alpha", "Search issues"),
    ];
    assert_eq!(names("issue", &documents, 8), ["beta", "alpha"]);
    assert_eq!(names("issue", &documents, 1), ["beta"]);
    assert!(names("issue", &documents, 0).is_empty());
    for query in ["", "the", "kubernetes", "tickets"] {
        assert!(names(query, &documents, 8).is_empty(), "{query}");
    }
}

#[test]
fn schema_search_follows_local_refs_and_escaped_uri_pointers() {
    let tool = spec(
        "inspect",
        "Inspect records",
        json!({
            "type":"object",
            "properties": {
                "options": {"$ref":"#/$defs/options"},
                "range": {"$ref":"#/$defs/date~1range"},
                "locale": {"$ref":"#/$defs/%E4%B8%AD%E6%96%87"}
            },
            "$defs": {
                "options":{"type":"object","properties":{"mode":{"type":"string","description":"reconciliation"}}},
                "date/range":{"description":"fiscal period"},
                "中文":{"description":"localized geography"}
            }
        }),
    );
    let doc = create_tool_search_document(&tool, None);
    for query in ["options mode", "reconciliation", "fiscal", "geography"] {
        assert_eq!(names(query, std::slice::from_ref(&doc), 1), ["inspect"]);
    }
}

#[test]
fn recursive_and_shared_schema_refs_are_read_once_and_unreferenced_defs_are_ignored() {
    let tool = spec(
        "inspect",
        "Inspect records",
        json!({
            "properties": {
                "root":{"$ref":"#/$defs/node"},
                "other":{"$ref":"#/$defs/node"},
                "self":{"$ref":"#"},
                "remote":{"$ref":"https://example.com/schema.json#/$defs/x"},
                "missing":{"$ref":"#/$defs/missing"},
                "malformed":{"$ref":"#/$defs/%E0%A4%A"}
            },
            "$defs": {
                "node":{"description":"tree node","properties":{"children":{"type":"array","items":{"$ref":"#/$defs/node"}}}},
                "unused":{"description":"reconciliation"}
            }
        }),
    );
    let doc = create_tool_search_document(&tool, None);
    assert_eq!(doc.text.matches("tree node").count(), 1);
    assert_eq!(doc.text.matches("children").count(), 1);
    assert_eq!(
        names("tree children", std::slice::from_ref(&doc), 1),
        ["inspect"]
    );
    assert!(names("reconciliation", &[doc], 1).is_empty());
}

#[test]
fn schema_variants_and_array_items_are_searchable() {
    let tool = spec(
        "inspect",
        "Inspect records",
        json!({
            "items":{"description":"ledger"},
            "anyOf":[{"description":"account"}],
            "oneOf":[{"description":"reconciliation"}],
            "allOf":[{"properties":{"fiscalPeriod":{"description":"quarter"}}}]
        }),
    );
    let doc = create_tool_search_document(&tool, None);
    for query in ["ledger", "account", "reconciliation", "fiscal", "quarter"] {
        assert_eq!(names(query, std::slice::from_ref(&doc), 1), ["inspect"]);
    }
}

#[tokio::test]
async fn search_requests_ranked_additions_without_session_capabilities() {
    let candidates = vec![
        deferred("mcp__docs__index", "List documentation titles."),
        deferred(
            "mcp__docs__search",
            " Search the docs.\r\nFull details on another line. ",
        ),
    ];
    let result = execute(
        context(candidates),
        json!({"query":"search the docs","limit":1}),
    )
    .await
    .unwrap();
    assert_eq!(
        text(&result),
        "Loaded 1 tool. They are available from your next call:\n- mcp__docs__search: Search the docs."
    );
    assert_eq!(
        result.details,
        Some(json!({"loaded":["mcp__docs__search"]}))
    );
    assert_eq!(
        result.added_tool_names,
        Some(vec!["mcp__docs__search".into()])
    );
    assert!(!result.is_error);
}

#[tokio::test]
async fn search_uses_default_limit_and_does_not_expand_an_empty_catalog() {
    let candidates = (0..10)
        .map(|index| deferred(&format!("lookup_{index}"), "Search records"))
        .collect();
    let result = execute(context(candidates), json!({"query":"records"}))
        .await
        .unwrap();
    assert_eq!(
        result.added_tool_names.as_ref().unwrap().len(),
        DEFAULT_TOOL_SEARCH_LIMIT
    );
    assert!(text(&result).starts_with("Loaded 8 tools. They are available from your next call:\n"));

    let result = execute(context(Vec::new()), json!({"query":"records"}))
        .await
        .unwrap();
    assert_eq!(text(&result), "No matching tools found.");
    assert_eq!(result.details, Some(json!({"loaded":[]})));
    assert_eq!(result.added_tool_names, Some(Vec::new()));
}

#[tokio::test]
async fn search_rejects_bad_input_and_accepts_large_integral_limits() {
    for input in [
        json!({"query":""}),
        json!({"query":" \n\t"}),
        json!({}),
        json!({"query":42}),
        json!([]),
        json!({"query":"x","limit":0}),
        json!({"query":"x","limit":-1}),
        json!({"query":"x","limit":1.5}),
        json!({"query":"x","limit":"8"}),
        json!({"query":"x","limit":null}),
    ] {
        assert!(
            matches!(
                tool_search_tool().validate_arguments(&input),
                Err(ToolError::InvalidArguments(_))
            ),
            "{input}"
        );
        assert!(matches!(
            execute(context(Vec::new()), input).await,
            Err(ToolError::InvalidArguments(_))
        ));
    }
    for limit in [json!(1.0), json!(1e30)] {
        let result = execute(
            context(vec![deferred("lookup", "Search records")]),
            json!({"query":"records","limit":limit}),
        )
        .await
        .unwrap();
        assert_eq!(result.added_tool_names, Some(vec!["lookup".into()]));
    }
}

#[tokio::test]
async fn aborted_search_does_not_request_activation() {
    let (abort, signal) = AbortHandle::new();
    let context = ToolContext::standalone(std::env::temp_dir(), signal)
        .with_discoverable_tools(vec![deferred("lookup", "Search records")]);
    abort.abort();
    assert!(matches!(
        execute(context, json!({"query":"records"})).await,
        Err(ToolError::Aborted)
    ));
}
