//! Differential fixture from the actual legacy coding-agent codemode executor.
//! Regenerate with scripts/sync-codemode-conformance.mjs; ordinary Cargo tests
//! require neither Node nor the legacy checkout. Only elapsed time, owned output
//! paths and explicitly opted-in engine stack frames are normalized.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use base64::Engine;
use pi_core::{
    AbortHandle, AbortSignal, AnyModelSpec, ClassifierContext, ClassifierResult, ImagesContext,
    ImagesResult, ModelId, ModelType, ProviderId, ToolCallId, ToolExecutionMode, ToolResult,
    ToolSpec,
};
use pi_plugin::{
    ContextParts, ModelsContextAccess, NestedToolExecutor, NestedToolResult, PluginContextEpoch,
    PluginContextResult, PluginContextScope, PluginDriver, RegistriesBuilder, SessionContextAccess,
    ToolContext, ToolDiscovery, ToolError, ToolInfo, ToolNamespace, ToolUpdateSink,
    UiContextAccess,
};
use pi_plugin_codemode::{CodemodeOptions, ConfiguredCodemodePlugin};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    oracle_revision: String,
    tools: Vec<Definition>,
    models: Vec<AnyModelSpec>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Definition {
    name: String,
    description: String,
    parameters: Value,
    output_schema: Option<Value>,
    namespace: Option<Namespace>,
    #[serde(default)]
    guidelines: Vec<String>,
}

#[derive(Deserialize)]
struct Namespace {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    description: Option<String>,
    instructions: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    code: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    prefix_spaces: usize,
    #[serde(default)]
    normalize_stack: bool,
    #[serde(default)]
    extra_input: serde_json::Map<String, Value>,
    #[serde(default)]
    models: bool,
    expected: Value,
}

struct Executor(AtomicUsize);

#[async_trait]
impl NestedToolExecutor for Executor {
    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        _signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        let id = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        let (result, structured_content) = match name {
            "structured" | "deep_roundtrip" => (
                ToolResult::text("structured fallback"),
                Some(json!({"value":arguments.get("value").unwrap_or(&Value::Null)})),
            ),
            "structured_fail" => (
                ToolResult::error("structured failure"),
                Some(json!({"isError":true})),
            ),
            "fail" => (ToolResult::error("tool rejected"), None),
            _ => (
                ToolResult::text(arguments["value"].as_str().unwrap_or(name)),
                None,
            ),
        };
        Ok(NestedToolResult {
            call_id: ToolCallId::new(format!("oracle/{id}")),
            result,
            structured_content,
        })
    }
}

struct FixtureModels(Vec<AnyModelSpec>);

#[async_trait]
impl SessionContextAccess for FixtureModels {}
#[async_trait]
impl UiContextAccess for FixtureModels {}

#[async_trait]
impl ModelsContextAccess for FixtureModels {
    fn models_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&ProviderId>,
    ) -> PluginContextResult<Vec<AnyModelSpec>> {
        Ok(self
            .0
            .iter()
            .filter(|model| {
                model.model_type() == model_type && provider.is_none_or(|p| p == model.provider())
            })
            .cloned()
            .collect())
    }

    fn available_models_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&ProviderId>,
    ) -> PluginContextResult<Vec<AnyModelSpec>> {
        self.models_of_type(model_type, provider)
    }

    async fn classify_model(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: ClassifierContext,
        _signal: AbortSignal,
    ) -> PluginContextResult<ClassifierResult> {
        let context = serde_json::to_value(request).unwrap();
        let answers: serde_json::Map<_, _> = context["questions"].as_object().unwrap().iter().map(|(id, question)| {
            let answer = match question["type"].as_str().unwrap() {
                "choice" => {
                    let labels: Vec<_> = question["criteria"].as_object().unwrap().keys().collect();
                    let probabilities: serde_json::Map<_, _> = labels.iter().enumerate().map(|(index, label)| ((*label).clone(), json!(if index == 0 { 1 } else { 0 }))).collect();
                    json!({"type":"choice","choice":labels[0],"probabilities":probabilities,"confidence":1})
                }
                "score" => json!({"type":"score","score":1,"confidence":1}),
                "bool" => json!({"type":"bool","probability":1}),
                _ => unreachable!(),
            };
            (id.clone(), answer)
        }).collect();
        let fail = context["state"]["fail"] == true;
        let mut result = json!({"api":"fixture","provider":provider,"model":model_id,"answers":answers,"stopReason":if fail {"error"} else {"stop"},"timestamp":1});
        if fail {
            result["errorMessage"] = json!("fixture model failure");
        }
        Ok(serde_json::from_value(result).unwrap())
    }

    async fn generate_model_images(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        request: ImagesContext,
        _signal: AbortSignal,
    ) -> PluginContextResult<ImagesResult> {
        Ok(serde_json::from_value(json!({"api":"fixture","provider":provider,"model":model_id,"output":request.input,"stopReason":"stop","timestamp":1})).unwrap())
    }
}

#[derive(Default)]
struct OutputFiles {
    paths: Vec<std::path::PathBuf>,
    images: Vec<String>,
    labels: HashMap<String, String>,
}

impl Drop for OutputFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn normalize_text(text: &str, case: &Case, files: &mut OutputFiles) -> String {
    let mut normalized = String::new();
    for line in text.split_inclusive('\n') {
        if case.normalize_stack && line.trim_start().starts_with("at ") {
            continue;
        }
        if let Some(time) = line.strip_prefix("Wall time ") {
            let time = time
                .trim_end_matches('\n')
                .strip_suffix(" seconds")
                .unwrap();
            assert!(time.parse::<f64>().is_ok(), "invalid wall time: {time}");
            assert_eq!(time.split('.').nth(1).unwrap().len(), 1);
            normalized.push_str("Wall time <elapsed> seconds");
            if line.ends_with('\n') {
                normalized.push('\n');
            }
        } else {
            normalized.push_str(line);
        }
    }
    let mut start = 0;
    const PREFIX: &str = "[Image saved to ";
    while let Some(index) = normalized[start..].find(PREFIX) {
        let path_start = start + index + PREFIX.len();
        let path_end = path_start + normalized[path_start..].find(" (image/").unwrap();
        let path = normalized[path_start..path_end].to_owned();
        let label = if let Some(label) = files.labels.get(&path) {
            label.clone()
        } else {
            let bytes = std::fs::read(&path).expect("emitted image path must be readable");
            let label = format!("<image-{}>", files.images.len());
            files
                .images
                .push(base64::engine::general_purpose::STANDARD.encode(bytes));
            files.paths.push(path.clone().into());
            files.labels.insert(path, label.clone());
            label
        };
        normalized.replace_range(path_start..path_end, &label);
        start = path_start + label.len();
    }
    normalized
}

fn normalize_result(result: ToolResult, case: &Case) -> Value {
    let mut files = OutputFiles::default();
    let mut content = serde_json::to_value(&result.content).unwrap();
    for block in content.as_array_mut().unwrap() {
        if block["type"] == "text" {
            block["text"] = json!(normalize_text(
                block["text"].as_str().unwrap(),
                case,
                &mut files
            ));
        }
    }
    let details = result.details.expect("codemode must return details");
    let calls = details["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            assert!(call["id"].as_str().unwrap().starts_with("oracle/"));
            let mut summary =
                json!({"name":call["name"],"args":call["args"],"status":call["status"]});
            if let Some(error) = call.get("error") {
                summary["error"] = error.clone();
            }
            summary
        })
        .collect::<Vec<_>>();
    let mut actual = json!({"isError":result.is_error,"content":content,"calls":calls});
    if let Some(path) = details.get("fullOutputPath").and_then(Value::as_str) {
        let full_output = std::fs::read_to_string(path).expect("full output must be readable");
        files.paths.push(path.into());
        actual["fullOutput"] = json!(normalize_text(&full_output, case, &mut files));
        for block in actual["content"].as_array_mut().unwrap() {
            if block["type"] == "text" {
                block["text"] = json!(
                    block["text"]
                        .as_str()
                        .unwrap()
                        .replace(path, "<full-output>")
                );
            }
        }
    }
    actual["savedImages"] = json!(files.images);
    actual
}

#[tokio::test]
async fn current_legacy_codemode_contract() {
    let fixture: Fixture = pi_core::json::from_str_with_max_depth(
        include_str!("fixtures/legacy-conformance.json"),
        1024,
    )
    .unwrap();
    assert_eq!(fixture.oracle_revision.len(), 40);
    assert!(
        fixture
            .oracle_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    let mut failures = Vec::new();
    for case in &fixture.cases {
        let tools: Vec<_> = case
            .tools
            .iter()
            .map(|name| {
                let tool = fixture
                    .tools
                    .iter()
                    .find(|tool| &tool.name == name)
                    .unwrap();
                ToolInfo {
                    spec: ToolSpec {
                        name: tool.name.clone(),
                        label: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                        output_schema: tool.output_schema.clone(),
                        constrained_sampling: None,
                        execution_mode: ToolExecutionMode::Parallel,
                        prompt_snippet: None,
                        prompt_guidelines: tool.guidelines.clone(),
                    },
                    discovery: ToolDiscovery {
                        namespace: tool.namespace.as_ref().map(|namespace| ToolNamespace {
                            name: namespace.name.clone(),
                            aliases: namespace.aliases.clone(),
                            description: namespace.description.clone(),
                            instructions: namespace.instructions.clone(),
                        }),
                        ..ToolDiscovery::default()
                    },
                }
            })
            .collect();
        let (_abort, signal) = AbortHandle::new();
        let epoch = PluginContextEpoch::new(Arc::new(FixtureModels(fixture.models.clone())));
        let context = ToolContext::with_plugin_context(
            std::env::temp_dir(),
            signal,
            ContextParts::new(epoch.handle(PluginContextScope::Base)),
        )
        .with_nested_executor(tools, Arc::new(Executor(AtomicUsize::new(0))));
        let source = " ".repeat(case.prefix_spaces) + &case.code;
        let mut input = json!({"code": source});
        input
            .as_object_mut()
            .unwrap()
            .extend(case.extra_input.clone());
        let mut registry = RegistriesBuilder::new();
        PluginDriver::new(vec![Arc::new(ConfiguredCodemodePlugin::new(
            CodemodeOptions {
                models: case.models,
                docs_path: "/pi/docs/codemode.md".into(),
                ..CodemodeOptions::default()
            },
        ))])
        .unwrap()
        .register_all(&mut registry)
        .unwrap();
        let tool = registry.freeze().tool("codemode").unwrap();
        // The product intentionally has no implicit execution deadline. A test
        // watchdog must still bound regressions in VM terminal-state handling.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tool.execute(
                context,
                ToolCallId::new("oracle"),
                input,
                ToolUpdateSink::channel().0,
            ),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{}: sandbox failed to settle within test watchdog",
                case.name
            )
        });
        match result {
            Ok(result) => {
                let actual = normalize_result(result, case);
                if actual != case.expected {
                    // Keep failures bounded even for the large-output cases.
                    let expected = serde_json::to_string(&case.expected).unwrap();
                    let actual = serde_json::to_string(&actual).unwrap();
                    failures.push(format!(
                        "{}\n expected: {}\n actual: {}",
                        case.name,
                        expected.chars().take(2000).collect::<String>(),
                        actual.chars().take(2000).collect::<String>()
                    ));
                }
            }
            Err(ToolError::InvalidArguments(message))
                if case.expected == json!({"inputError": message}) => {}
            Err(error) => failures.push(format!("{}: rejected with {error}", case.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
