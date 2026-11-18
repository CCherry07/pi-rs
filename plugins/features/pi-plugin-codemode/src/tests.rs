use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pi_core::AbortSignal;
use pi_plugin::{NestedToolExecutor, ToolDiscovery, ToolInfo};
use tokio::sync::Barrier;

use super::*;

fn text_of(result: &ToolResult) -> String {
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

async fn script(code: &str) -> ToolResult {
    let (_abort, signal) = AbortHandle::new();
    let context = ToolContext::standalone(std::env::temp_dir(), signal);
    codemode_tool()
        .execute(
            context,
            ToolCallId::new("script"),
            json!({"code":code}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn runs_async_source_console_return_images_and_store_with_fresh_globals() {
    let result = script(r#"
        text(typeof process + ':' + typeof require + ':' + typeof fetch + ':' + typeof setTimeout + ':' + typeof WebAssembly);
        store('__proto__', {ok: true});
        const value = load('__proto__'); value.ok = false;
        text(load('__proto__'));
        console.warn('warning', 2);
        image('data:image/png;base64,aGVsbG8=');
        globalThis.someState = 9;
        return await Promise.resolve({answer:42});
    "#).await;
    assert!(!result.is_error, "{}", text_of(&result));
    let text = text_of(&result);
    assert!(text.contains("undefined:undefined:undefined:undefined:undefined"));
    assert!(text.contains(r#"{"ok":true}"#));
    assert!(text.contains(r#"{"answer":42}"#));
    assert!(text.contains("<console_output>\nwarning 2\n</console_output>"));
    assert!(matches!(
        result
            .content
            .iter()
            .find(|block| matches!(block, ContentBlock::Image(_))),
        Some(ContentBlock::Image(_))
    ));
    let next = script("text(typeof someState); text(load('__proto__')); ").await;
    assert_eq!(text_of(&next).matches("undefined").count(), 2);
}

#[tokio::test]
async fn keeps_partial_output_on_errors_and_successful_exit_even_if_caught() {
    let failed = script("text('before'); throw new Error('expected failure');").await;
    assert!(failed.is_error);
    assert!(text_of(&failed).contains("before"));
    assert!(text_of(&failed).contains("expected failure"));
    let exit = script("text('finished'); try { exit(); } catch (_) { while (true) {} }").await;
    assert!(!exit.is_error, "{}", text_of(&exit));
    assert!(text_of(&exit).contains("finished"));
}

#[tokio::test]
async fn enforces_deadlines_on_cpu_and_promise_loops_and_detects_stalled_promises() {
    for body in ["while (true) {}", "while (true) await null;"] {
        let started = Instant::now();
        let result = script(&format!(
            "// @options: {{\"timeout_ms\":30}}\ntext('before'); {body}"
        ))
        .await;
        assert!(result.is_error);
        assert!(
            text_of(&result).contains("timed out"),
            "{}",
            text_of(&result)
        );
        assert!(text_of(&result).contains("before"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    let stalled = script("await new Promise(() => {});").await;
    assert!(stalled.is_error);
    assert!(text_of(&stalled).contains("stalled"));
}

#[tokio::test]
async fn output_and_source_limits_are_enforced_without_losing_utf8_boundaries() {
    let result = script("// @options: {\"max_output_tokens\":2}\ntext('你好世界');").await;
    assert!(!result.is_error);
    assert!(text_of(&result).contains("你好"));
    assert!(!text_of(&result).contains("世界"));
    assert_eq!(result.details.unwrap()["truncated"], true);
    let framed =
        script("// @options: {\"max_output_tokens\":1}\nfor (let n=0; n<9999; n++) text('');")
            .await;
    assert!(!framed.is_error);
    assert!(text_of(&framed).len() < 100);
    assert_eq!(framed.details.unwrap()["truncated"], true);
    let result = script("for (let n=0; n<11000; n++) text('');").await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("10000 items"));
    for source in [
        "// @options: {\"timeout_ms\":0}\ntext(1)",
        "text(1);\n// @options: {}",
        "// @options: {\"extra\":true}\ntext(1)",
    ] {
        assert!(Source::parse(source).is_err());
    }
}

#[test]
fn enforces_quickjs_heap_limit() {
    let (_abort, signal) = AbortHandle::new();
    let (requests, _received) = tokio::sync::mpsc::unbounded_channel();
    let (_replies, received) = mpsc::channel();
    let result = vm::execute(VmInput {
        source: Source::parse(
            "const data=[]; for (let i=0; i<100; i++) data.push(new ArrayBuffer(1024*1024));",
        )
        .unwrap(),
        catalog: Arc::new(Catalog::new(&[]).unwrap()),
        store: json!({}),
        signal: signal.clone(),
        parent_signal: signal,
        output: Arc::new(Mutex::new(Output::default())),
        requests,
        replies: received,
        deadline: Instant::now() + Duration::from_secs(2),
        memory_limit: 8 * 1024 * 1024,
    });
    assert!(result.error.is_some(), "heap allocation must fail");
}

struct FakeExecutor {
    barrier: Option<Arc<Barrier>>,
    started: AtomicUsize,
    cancelled: Arc<AtomicBool>,
}

#[async_trait]
impl NestedToolExecutor for FakeExecutor {
    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        let id = self.started.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        if name == "wait" {
            struct Cancelled(Arc<AtomicBool>);
            impl Drop for Cancelled {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _cancelled = Cancelled(self.cancelled.clone());
            signal.wait().await;
            return Err(ToolError::Aborted);
        }
        if name == "fail" {
            return Ok(NestedToolResult {
                call_id: ToolCallId::new(format!("script/{id}")),
                result: ToolResult::error("tool rejected"),
                structured_content: None,
            });
        }
        let mut result = ToolResult::text(arguments["value"].as_str().unwrap_or("text"));
        result.usage = Some(Usage {
            output: 2,
            total_tokens: 2,
            ..Usage::default()
        });
        let structured_content = (name == "structured").then(
            || json!({"content":[{"type":"text","text":"raw"}],"structuredContent":{"ok":true}}),
        );
        Ok(NestedToolResult {
            call_id: ToolCallId::new(format!("script/{id}")),
            result,
            structured_content,
        })
    }
}

fn fake_context(signal: AbortSignal, executor: Arc<dyn NestedToolExecutor>) -> ToolContext {
    let tools: Vec<_> = ["read", "structured", "fail", "wait"]
        .into_iter()
        .map(|name| ToolInfo {
            spec: ToolSpec {
                name: name.into(),
                label: name.into(),
                description: format!("{name} test tool"),
                parameters: json!({"type":"object","properties":{"value":{"type":"string"}}}),
                output_schema: (name == "structured").then(|| json!({"type":"object","properties":{"content":{"type":"array"},"structuredContent":{"type":"object"}}})),
                constrained_sampling: None,
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
            },
            discovery: ToolDiscovery::default(),
        })
        .collect();
    ToolContext::standalone(std::env::temp_dir(), signal).with_nested_executor(tools, executor)
}

struct CancellationFinalizer {
    finalized: AtomicUsize,
}

#[async_trait]
impl NestedToolExecutor for CancellationFinalizer {
    async fn execute(
        &self,
        name: &str,
        _arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        let mut result = if name == "wait" {
            signal.wait().await;
            // A real scheduler still awaits its tool_result hook after the
            // execution signal fires; dropping this future would skip it.
            tokio::task::yield_now().await;
            self.finalized.fetch_add(1, Ordering::SeqCst);
            ToolResult::error("Operation aborted")
        } else {
            ToolResult::text("ready")
        };
        result.usage = Some(Usage {
            output: 3,
            total_tokens: 3,
            ..Usage::default()
        });
        Ok(NestedToolResult {
            call_id: ToolCallId::new(format!("script/{name}")),
            result,
            structured_content: None,
        })
    }
}

#[tokio::test]
async fn script_exit_and_timeout_drain_cancelled_pipelines_and_their_usage() {
    for (code, expected_usage, expected_error) in [
        ("tools.wait({}); await tools.read({}); exit();", 6, false),
        (
            "// @options: {\"timeout_ms\":50}\nawait tools.wait({});",
            3,
            true,
        ),
    ] {
        let (_abort, signal) = AbortHandle::new();
        let executor = Arc::new(CancellationFinalizer {
            finalized: AtomicUsize::new(0),
        });
        let result = codemode_tool()
            .execute(
                fake_context(signal, executor.clone()),
                ToolCallId::new("script"),
                json!({"code":code}),
                ToolUpdateSink::channel().0,
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, expected_error, "{}", text_of(&result));
        assert_eq!(executor.finalized.load(Ordering::SeqCst), 1);
        assert_eq!(result.usage.unwrap().output, expected_usage);
        assert_eq!(result.details.unwrap()["calls"][0]["status"], "cancelled");
    }
}

#[tokio::test]
async fn bridges_parallel_json_calls_structured_results_errors_and_usage() {
    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: Some(Arc::new(Barrier::new(2))),
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let result = codemode_tool()
        .execute(
            fake_context(signal, executor),
            ToolCallId::new("script"),
            json!({"code":r#"
      const values = await Promise.allSettled([tools.read({value:'hello'}), tools.structured({})]);
      text(values);
      text(await describeTool('read'));
      text(await searchTools('read'));
      text(ALL_TOOLS.length);
    "#}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).contains("hello"));
    assert!(text_of(&result).contains("structuredContent"));
    assert_eq!(
        result.details.as_ref().unwrap()["calls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(result.usage.unwrap().output, 4);

    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let result = codemode_tool()
        .execute(
            fake_context(signal, executor),
            ToolCallId::new("script"),
            json!({"code":"try { await tools.fail({}); } catch (e) { text(e.message); }"}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(text_of(&result).contains("tool rejected"));
}

#[tokio::test]
async fn aborting_script_drops_pending_nested_tools() {
    let (abort, signal) = AbortHandle::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    let context = fake_context(signal, executor.clone());
    let task = tokio::spawn(async move {
        codemode_tool()
            .execute(
                context,
                ToolCallId::new("script"),
                json!({"code":"text('waiting'); await tools.wait({});"}),
                ToolUpdateSink::channel().0,
            )
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while executor.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    abort.abort();
    let result = task.await.unwrap();
    assert!(result.is_error);
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(text_of(&result).contains("waiting"));
    assert_eq!(result.details.unwrap()["calls"][0]["status"], "cancelled");
}

#[tokio::test]
async fn successful_return_cancels_unawaited_calls_and_call_storms_are_bounded() {
    let (_abort, signal) = AbortHandle::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    let result = codemode_tool()
        .execute(
            fake_context(signal, executor),
            ToolCallId::new("script"),
            json!({"code":"tools.wait({}); await tools.read({}); return 'complete';"}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(result.details.unwrap()["calls"][0]["status"], "cancelled");

    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let result = codemode_tool().execute(fake_context(signal, executor.clone()), ToolCallId::new("script"), json!({"code":"const results = await Promise.allSettled(Array.from({length:130}, () => tools.read({}))); text(results.filter(x => x.status === 'rejected').length);"}), ToolUpdateSink::channel().0).await.unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert_eq!(executor.started.load(Ordering::SeqCst), 128);
    assert!(text_of(&result).ends_with('2'));
}

#[tokio::test]
async fn invalid_images_fail_without_entering_provider_content() {
    let result = script("image({type:'image',mimeType:'image/png',data:'===='});").await;
    assert!(result.is_error);
    assert!(
        !result
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_)))
    );
}

#[test]
fn catalog_preserves_exact_names_and_exposes_namespace_instructions() {
    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let context = fake_context(signal, executor);
    let mut tools = context.nested_tools().to_vec();
    tools.truncate(3);
    tools[0].spec.name = "read-file".into();
    tools[1].spec.name = "read_file".into();
    tools[2].spec.name = "read.file".into();
    tools[0].discovery.namespace = Some(pi_plugin::ToolNamespace {
        name: "files".into(),
        description: Some("file search".into()),
        instructions: Some("Use relative paths".into()),
    });
    let catalog = Catalog::new(&tools).unwrap();
    let declarations: Value = serde_json::from_str(&catalog.declarations).unwrap();
    assert_eq!(declarations[0]["jsName"], "read-file");
    assert_eq!(declarations[1]["jsName"], "read_file");
    assert_eq!(declarations[2]["jsName"], "read.file");
    let namespace: Value =
        serde_json::from_str(&catalog.query("describeNamespace", r#"["files"]"#).unwrap()).unwrap();
    assert_eq!(namespace["value"]["instructions"], "Use relative paths");
    assert_eq!(namespace["value"]["tools"][0], "read-file");
    let found: Value = serde_json::from_str(
        &catalog
            .query("searchTools", r#"["file",{"namespace":"files"}]"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found["value"].as_array().unwrap().len(), 1);
    let description = declarations[0]["description"].as_str().unwrap();
    assert!(description.contains("codemode tool declaration:"));
    assert!(description.contains("\"read-file\"(args:"));
    assert!(description.contains("Promise<string>"));
}

#[tokio::test]
async fn emitted_images_are_saved_once_and_paths_reach_the_model() {
    let result =
        script("image('data:image/png;base64,aGVsbG8='); image('data:image/png;base64,aGVsbG8=');")
            .await;
    assert!(!result.is_error, "{}", text_of(&result));
    let labels = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) if text.text.starts_with("[Image saved to ") => {
                Some(text.text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0], labels[1]);
    let path = labels[0]
        .strip_prefix("[Image saved to ")
        .unwrap()
        .split(" (image/png,")
        .next()
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"hello");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn codemode_loadout_changes_actual_registered_declarations_without_mutating_the_catalog() {
    struct DeclaredTool(ToolSpec);
    #[async_trait]
    impl Tool for DeclaredTool {
        fn spec(&self) -> ToolSpec {
            self.0.clone()
        }

        async fn execute(
            &self,
            _context: ToolContext,
            _id: ToolCallId,
            _input: Value,
            _updates: ToolUpdateSink,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("rendering declarations cannot execute tools")
        }
    }
    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let context = fake_context(signal, executor);
    let registry = |mode| {
        let mut builder = pi_plugin::RegistriesBuilder::new();
        builder
            .register_tool(
                PluginId::new("codemode"),
                Arc::new(CodemodeTool {
                    options: CodemodeOptions {
                        mode,
                        ..CodemodeOptions::default()
                    },
                }),
            )
            .unwrap();
        for tool in context.nested_tools() {
            builder
                .register_tool(
                    PluginId::new("fixture"),
                    Arc::new(DeclaredTool(tool.spec.clone())),
                )
                .unwrap();
        }
        builder.freeze()
    };
    let active = ["codemode".into(), "read".into(), "structured".into()];
    let on = registry(CodemodeMode::On);
    let prepared = on.prepare_tool_loadout(&active).unwrap();
    let read = prepared
        .tools
        .iter()
        .find(|tool| tool.name == "read")
        .unwrap();
    assert!(
        read.description
            .ends_with("Codemode: `tools.read(args)` resolves to a string.")
    );
    assert!(
        prepared
            .tools
            .iter()
            .find(|tool| tool.name == "structured")
            .unwrap()
            .description
            .contains("structuredContent")
    );
    assert!(prepared.hidden_tools.is_empty());
    let without_codemode = on.prepare_tool_loadout(&["read".into()]).unwrap();
    assert_eq!(without_codemode.tools[0].description, "read test tool");
    assert_eq!(
        on.all_tool_specs()
            .iter()
            .find(|tool| tool.name == "read")
            .unwrap()
            .description,
        "read test tool"
    );
    let only = registry(CodemodeMode::Only)
        .prepare_tool_loadout(&active)
        .unwrap();
    assert_eq!(only.tools.len(), 1);
    assert_eq!(only.hidden_tools.len(), 2);
    assert!(only.tools[0].description.contains("### `read`"));
    assert!(only.tools[0].description.contains("Promise<string>"));
    assert!(!only.tools[0].description.contains("### `wait`"));
}

#[test]
fn queued_result_budget_is_released_on_drop() {
    let total = Arc::new(AtomicUsize::new(0));
    let reservation = ReplyReservation::new(&total, 32 * 1024 * 1024).unwrap();
    assert!(ReplyReservation::new(&total, 1).is_none());
    drop(reservation);
    assert!(ReplyReservation::new(&total, 1).is_some());
}
