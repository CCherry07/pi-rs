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
        image('data:image/png;base64,iVBORw0KGgo=');
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
            "// @options: {{\"timeout_ms\":500}}\ntext('before'); {body}"
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
    assert!(text_of(&stalled).contains("promise that can never settle"));
}

#[tokio::test]
async fn output_and_source_limits_are_enforced_without_losing_utf8_boundaries() {
    let result = script("// @options: {\"max_output_tokens\":2}\ntext('你好世界');").await;
    assert!(!result.is_error);
    assert!(text_of(&result).contains("你好世界"));
    assert!(result.details.unwrap().get("fullOutputPath").is_none());
    let framed =
        script("// @options: {\"max_output_tokens\":1}\nfor (let n=0; n<9999; n++) text('');")
            .await;
    assert!(!framed.is_error);
    assert!(text_of(&framed).len() < 500);
    let path = framed.details.unwrap()["fullOutputPath"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("==> text 9999/9999 <==")
    );
    std::fs::remove_file(path).unwrap();
    let result = script("for (let n=0; n<100001; n++) text('');").await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("100000 text(), image(), and console calls"));
    if let Some(path) = result.details.unwrap()["fullOutputPath"].as_str() {
        std::fs::remove_file(path).unwrap();
    }
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
        deadline: Some(Instant::now() + Duration::from_secs(2)),
        memory_limit: 8 * 1024 * 1024,
        models: false,
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
            || json!({"content":[{"type":"text","text":"raw"}],"structuredContent":arguments}),
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
            "// @options: {\"timeout_ms\":500}\nawait tools.wait({});",
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
async fn near_limit_json_round_trips_through_tool_arguments_results_and_store_writes() {
    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let result = codemode_tool()
        .execute(
            fake_context(signal, executor),
            ToolCallId::new("deep-roundtrip"),
            json!({"code":r#"
                let deep = "tool-leaf";
                for (let index = 0; index < 511; index++) deep = {next: deep};
                const echoed = await tools.structured({deep});
                let node = echoed.structuredContent.deep;
                let depth = 0;
                while (node && typeof node === "object") { depth++; node = node.next; }
                text({depth, leaf: node});

                let stored = "store-leaf";
                for (let index = 0; index < 512; index++) stored = {next: stored};
                store("deep", stored);
                node = load("deep");
                depth = 0;
                while (node && typeof node === "object") { depth++; node = node.next; }
                text({depth, leaf: node});
            "#}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    let output = text_of(&result);
    assert!(output.contains(r#"{"depth":511,"leaf":"tool-leaf"}"#));
    assert!(output.contains(r#"{"depth":512,"leaf":"store-leaf"}"#));
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
async fn successful_return_cancels_unawaited_calls_without_artificial_call_quotas() {
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
            json!({"code":"tools.wait({}); return 'complete';"}),
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
    assert_eq!(executor.started.load(Ordering::SeqCst), 130);
    assert!(text_of(&result).ends_with('0'));
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
fn catalog_normalizes_names_and_exposes_namespace_instructions() {
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
        aliases: Vec::new(),
        description: Some("file search".into()),
        instructions: Some("Use relative paths".into()),
    });
    let catalog = Catalog::new(&tools).unwrap();
    let declarations: Value = serde_json::from_str(&catalog.declarations).unwrap();
    assert_eq!(declarations[0]["jsName"], "read_file");
    assert_eq!(declarations[1]["jsName"], "read_file");
    assert_eq!(declarations[2]["jsName"], "read_file");
    let namespace: Value =
        serde_json::from_str(&catalog.query("describeNamespace", r#"["files"]"#).unwrap()).unwrap();
    assert_eq!(namespace["value"]["instructions"], "Use relative paths");
    assert_eq!(namespace["value"]["tools"][0], "read_file");
    let found: Value = serde_json::from_str(
        &catalog
            .query("searchTools", r#"["file",{"namespace":"files"}]"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found["value"].as_array().unwrap().len(), 1);
    let description = declarations[0]["description"].as_str().unwrap();
    assert!(description.contains("codemode tool declaration:"));
    assert!(description.contains("read_file(args:"));
    assert!(description.contains("Promise<string>"));
}

#[tokio::test]
async fn emitted_images_are_saved_once_and_paths_reach_the_model() {
    let result = script(
        "image('data:image/png;base64,iVBORw0KGgo='); image('data:image/png;base64,iVBORw0KGgo=');",
    )
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
    assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\n");
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

#[tokio::test]
async fn canonical_prelude_keeps_sloppy_arrow_scope_and_frozen_intrinsics() {
    let result = script("Array.prototype.push=()=>0; const a=[]; a.push(1); text(a); return [this===globalThis,typeof arguments];").await;
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).contains("[1]"));
    assert!(text_of(&result).contains("[true,\"undefined\"]"));
    let result = script("globalLeak=3; return globalThis.globalLeak;").await;
    assert!(!result.is_error);
    assert!(text_of(&result).ends_with('3'));
    assert!(script("const tools={}; return 1").await.is_error);
}

#[tokio::test]
async fn return_serialization_and_image_sniffing_match_script_contract() {
    let result = script("text(new Error('x')); return NaN;").await;
    assert!(!result.is_error);
    assert!(text_of(&result).ends_with("null"));
    assert!(text_of(&result).contains("{}"));
    assert!(script("return 1n;").await.is_error);
    assert!(script("const x={}; x.x=x; text(x);").await.is_error);
    let result = script("// @options: {\"max_output_tokens\":0}\nimage({type:'image',data:'iVBORw0KGgo=',mimeType:'image/jpeg'});").await;
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(
        result.content.iter().any(
            |block| matches!(block,ContentBlock::Image(image) if image.mime_type == "image/png")
        )
    );
}

#[tokio::test]
async fn unicode_store_limits_count_json_characters_not_bytes() {
    let result = script("const value='中'.repeat(200000); for(let i=0;i<4;i++) store(String(i),value); return load('3').length;").await;
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).ends_with("200000"));
}

#[test]
fn input_schema_allows_additional_fields() {
    assert!(parse_input(&json!({"code":"return 1", "ignored":true})).is_ok());
}

async fn with_context_and_workers(
    code: &str,
    workers: Arc<workers::Workers>,
    context: ToolContext,
) -> ToolResult {
    let catalog = Arc::new(Catalog::new(context.nested_tools()).unwrap());
    run(
        context,
        ToolCallId::new("queue-test"),
        Source::parse(code).unwrap(),
        catalog,
        json!({}),
        ToolUpdateSink::channel().0,
        RunOptions {
            models: false,
            docs_path: CodemodeOptions::default().docs_path,
            workers,
        },
    )
    .await
    .unwrap()
}

async fn with_workers(
    code: &str,
    workers: Arc<workers::Workers>,
    signal: AbortSignal,
) -> ToolResult {
    with_context_and_workers(
        code,
        workers,
        ToolContext::standalone(std::env::temp_dir(), signal),
    )
    .await
}

#[tokio::test]
async fn admission_failures_are_tool_results_and_never_start_the_script() {
    let workers = Arc::new(workers::Workers::new(|| 1));
    let occupied = workers.reserve().await;
    let (_abort, signal) = AbortHandle::new();
    let result = with_workers(
        "// @options: {\"timeout_ms\":20}\ntext('must not run')",
        workers.clone(),
        signal,
    )
    .await;
    assert!(result.is_error);
    let text = text_of(&result);
    assert!(
        text.contains("Execution timed out after 20 ms while waiting for a free codemode worker")
    );
    assert!(text.contains("The script did not start."));
    assert!(!text.contains("must not run"));
    assert_eq!(result.details.unwrap(), json!({"calls":[]}));
    let (abort, signal) = AbortHandle::new();
    abort.abort();
    let result = with_workers("return 1", workers.clone(), signal).await;
    assert!(text_of(&result).contains("Script aborted: Execution aborted"));
    drop(occupied);
    let (_abort, signal) = AbortHandle::new();
    assert!(!with_workers("return 1", workers, signal).await.is_error);
}

#[tokio::test]
async fn deadline_after_admission_uses_only_the_remaining_time() {
    let workers = Arc::new(workers::Workers::new(|| 1));
    let occupied = workers.reserve().await;
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(occupied);
    });
    let (_abort, signal) = AbortHandle::new();
    let result = with_workers(
        "// @options: {\"timeout_ms\":250}\ntext('started'); while(true) {}",
        workers,
        signal,
    )
    .await;
    release.await.unwrap();
    assert!(result.is_error);
    let text = text_of(&result);
    assert!(text.contains("started"), "{text}");
    let millis = text
        .split("Execution timed out after ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(
        millis <= 120,
        "admission must consume the original 250ms deadline: {text}"
    );
}

#[tokio::test]
async fn plain_codemode_has_no_models_but_configured_feature_exposes_them() {
    assert!(text_of(&script("return typeof models").await).ends_with("undefined"));
    let (_abort, signal) = AbortHandle::new();
    let result = CodemodeTool {
        options: CodemodeOptions {
            models: true,
            ..CodemodeOptions::default()
        },
    }
    .execute(
        ToolContext::standalone(std::env::temp_dir(), signal),
        ToolCallId::new("models-test"),
        json!({"code":"return typeof models.getModelsOfType"}),
        ToolUpdateSink::channel().0,
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).ends_with("function"));
}

#[tokio::test]
async fn model_named_tools_stay_in_the_tool_dispatch_scope() {
    let (_abort, signal) = AbortHandle::new();
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let context = fake_context(signal.clone(), executor.clone());
    let mut tool = context.nested_tools()[0].clone();
    tool.spec.name = "models.getModelsOfType".into();
    let context = ToolContext::standalone(std::env::temp_dir(), signal)
        .with_nested_executor(vec![tool], executor.clone());
    let result = codemode_tool()
        .execute(
            context,
            ToolCallId::new("tool-model-name"),
            json!({"code":"return await tools.models_getModelsOfType({value:'tool'});"}),
            ToolUpdateSink::channel().0,
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(text_of(&result).ends_with("tool"));
    assert_eq!(executor.started.load(Ordering::SeqCst), 1);
    let calls = &result.details.as_ref().unwrap()["calls"];
    assert_eq!(calls[0]["name"], "models.getModelsOfType");
    assert!(calls[0].get("cost").is_none());
}

#[tokio::test]
async fn caught_terminal_signals_cannot_start_more_tools() {
    for (code, failed) in [
        ("try { exit(); } catch (_) {} tools.read({});", false),
        (
            "try { text('x'.repeat(16*1024*1024+1)); } catch (_) {} tools.read({});",
            true,
        ),
    ] {
        let (_abort, signal) = AbortHandle::new();
        let executor = Arc::new(FakeExecutor {
            barrier: None,
            started: AtomicUsize::new(0),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let result = codemode_tool()
            .execute(
                fake_context(signal, executor.clone()),
                ToolCallId::new("terminal"),
                json!({"code":code}),
                ToolUpdateSink::channel().0,
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, failed, "{}", text_of(&result));
        assert_eq!(executor.started.load(Ordering::SeqCst), 0);
        assert_eq!(result.details.unwrap()["calls"], json!([]));
    }
}

#[tokio::test]
async fn deep_call_cancellation_and_overlimit_arguments_leave_the_next_worker_healthy() {
    let workers = Arc::new(workers::Workers::new(|| 1));
    let cancelled = Arc::new(AtomicBool::new(false));
    let executor = Arc::new(FakeExecutor {
        barrier: None,
        started: AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    let (_abort, signal) = AbortHandle::new();
    let result = with_context_and_workers(
        r#"
            let deep = 0;
            for (let index = 0; index < 511; index++) deep = {next: deep};
            tools.wait({deep});
            await tools.read({});
            return "finished";
        "#,
        workers.clone(),
        fake_context(signal, executor.clone()),
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(result.details.unwrap()["calls"][0]["status"], "cancelled");

    let (_abort, signal) = AbortHandle::new();
    let result = with_context_and_workers(
        r#"
            let deep = 0;
            for (let index = 0; index < 512; index++) deep = {next: deep};
            await tools.read({deep});
        "#,
        workers.clone(),
        fake_context(signal, executor.clone()),
    )
    .await;
    assert!(result.is_error);
    assert!(text_of(&result).contains("JSON nesting depth exceeds the limit of 512"));
    assert_eq!(executor.started.load(Ordering::SeqCst), 2);

    let (_abort, signal) = AbortHandle::new();
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        with_workers("return 1", workers, signal),
    )
    .await
    .expect("cancelled and rejected VMs must release the only worker");
    assert!(!next.is_error, "{}", text_of(&next));
}

#[tokio::test]
async fn bridge_decode_and_overlimit_failures_settle_and_release_the_worker() {
    let workers = Arc::new(workers::Workers::new(|| 1));
    for code in [
        "return String.fromCharCode(0xd800)",
        "store(String.fromCharCode(0xd800),1); return 1",
        "let x=0; for(let i=0;i<513;i++)x={x}; store('deep',x); return 1",
    ] {
        let (abort, signal) = AbortHandle::new();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            with_workers(code, workers.clone(), signal),
        )
        .await;
        abort.abort();
        let result = result.expect("bridge decoding must settle even without a source deadline");
        assert!(result.is_error, "{}", text_of(&result));
        assert!(text_of(&result).contains("Sandbox bridge broken"));
        let (abort, signal) = AbortHandle::new();
        let next = tokio::time::timeout(
            Duration::from_secs(2),
            with_workers("return 1", workers.clone(), signal),
        )
        .await;
        abort.abort();
        assert!(!next.expect("failed VM must release its worker").is_error);
    }
    let (_abort, signal) = AbortHandle::new();
    let result = with_workers(
        "let x=0;for(let i=0;i<600;i++)x={x};return x",
        workers,
        signal,
    )
    .await;
    assert!(!result.is_error, "{}", text_of(&result));
}
