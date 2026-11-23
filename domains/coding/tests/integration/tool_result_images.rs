//! Tool-produced images cross the final Coding hook before history and provider projection.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use pi_coding::{Config, Features, Pi};
use pi_core::{AgentEvent, ContentBlock, ImageContent, Message, PluginId, TextContent, ToolResult};
use pi_plugin::{AgentPluginContext, Plugin, PluginError, ToolResultEvent, ToolResultPatch};
use pi_session::{AgentSessionEvent, PiSession, PromptOutcome, SessionGenerationOverlay};
use serde_json::{Value, json};

const NORMALIZER_ID: &str = "coding-tool-result-image-normalizer";
const INJECTOR_ID: &str = "tool-result-image-injector-fixture";

#[derive(Clone)]
struct InjectImageResult {
    content: Arc<Vec<ContentBlock>>,
    structured_content: Option<Value>,
    invocations: Arc<AtomicUsize>,
}

#[pi_plugin::plugin]
impl Plugin for InjectImageResult {
    fn id(&self) -> PluginId {
        PluginId::new(INJECTOR_ID)
    }

    async fn tool_result(
        &self,
        _context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        if event.tool_call.name != "read" {
            return Ok(ToolResultPatch::default());
        }
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResultPatch {
            content: Some(self.content.as_ref().clone()),
            structured_content: self.structured_content.clone(),
            ..ToolResultPatch::default()
        })
    }
}

async fn provider_response(
    axum::extract::State(requests): axum::extract::State<Arc<Mutex<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl axum::response::IntoResponse {
    let model = request["model"].clone();
    let answer_tool = {
        let mut requests = requests.lock().unwrap();
        let answer_tool = requests.len() % 2 == 1;
        requests.push(request);
        answer_tool
    };
    let (delta, finish_reason) = if answer_tool {
        (
            json!({"role":"assistant","content":"Image received."}),
            "stop",
        )
    } else {
        (
            json!({"role":"assistant","tool_calls":[{
                "index":0,"id":"read-image","type":"function",
                "function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}
            }]}),
            "tool_calls",
        )
    };
    let chunk = json!({
        "id":"tool-result-images","object":"chat.completion.chunk","model":model,
        "choices":[{"index":0,"delta":delta,"finish_reason":null}]
    });
    let end = json!({
        "id":"tool-result-images","object":"chat.completion.chunk","model":model,
        "choices":[{"index":0,"delta":{},"finish_reason":finish_reason}]
    });
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}

struct Fixture {
    _directory: tempfile::TempDir,
    pi: Pi,
    session: PiSession,
    requests: Arc<Mutex<Vec<Value>>>,
    invocations: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(
        content: Vec<ContentBlock>,
        structured_content: Option<Value>,
        auto_resize: bool,
        resize: Value,
    ) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = axum::Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post(provider_response),
            )
            .with_state(Arc::clone(&requests));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let cwd = directory.path().join("project");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("note.txt"), "fixture").unwrap();
        std::fs::write(
            agent_dir.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            json!({"images":{"autoResize":auto_resize}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            agent_dir.join("models.json"),
            json!({"providers":{"fixture":{
                "baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
                "models":[{
                    "id":"test","name":"Test","input":["text","image"],
                    "contextWindow":100000,"maxTokens":1024,
                    "inputLimits":{"images":{"resize":resize}}
                }, {
                    "id":"narrow","name":"Narrow","input":["text","image"],
                    "contextWindow":100000,"maxTokens":1024,
                    "inputLimits":{"images":{"resize":{
                        "maxWidth":60,"maxHeight":60,"maxBytes":1000000,"jpegQuality":80
                    }}}
                }]
            }}})
            .to_string(),
        )
        .unwrap();

        let invocations = Arc::new(AtomicUsize::new(0));
        let injector = InjectImageResult {
            content: Arc::new(content),
            structured_content,
            invocations: Arc::clone(&invocations),
        };
        let overlay =
            SessionGenerationOverlay::new().with_plugin(move || Arc::new(injector.clone()));
        let mut config = Config::new(cwd.clone(), agent_dir.clone());
        config.features = Features::none();
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.load_mcp_config = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let mut options =
            pi_session::CreateSessionOptions::new(&cwd, agent_dir.join("tool-result-images.jsonl"));
        options.generation_overlay = overlay;
        let session = pi.sessions().create_session(options).await.unwrap();
        Self {
            _directory: directory,
            pi,
            session,
            requests,
            invocations,
            server,
        }
    }

    async fn prompt(&self) -> (Vec<Message>, ToolResult) {
        let mut subscription = self.session.current().subscribe();
        let PromptOutcome::Agent(outcome) = self
            .session
            .current()
            .prompt("Inspect the fixture file.")
            .await
            .unwrap()
        else {
            panic!("expected an agent turn");
        };
        let mut execution_result = None;
        while let Ok(event) = subscription.events.try_recv() {
            if let AgentSessionEvent::Agent(event) = event.event
                && let AgentEvent::ToolExecutionEnd { result, .. } = *event
            {
                execution_result = Some(result);
            }
        }
        (
            outcome.new_messages,
            execution_result.expect("tool result event"),
        )
    }

    fn assert_registration_order(&self) {
        let order = self.session.current().catalog().plugin_order();
        assert_eq!(order.last().unwrap().as_str(), NORMALIZER_ID);
        let injector = order
            .iter()
            .position(|id| id.as_str() == INJECTOR_ID)
            .expect("overlay injector registered");
        let normalizer = order
            .iter()
            .position(|id| id.as_str() == NORMALIZER_ID)
            .expect("normalizer registered");
        assert!(injector < normalizer);
    }

    async fn shutdown(self) {
        self.pi.sessions().shutdown().await.unwrap();
        self.server.abort();
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    pi_media::image::encode_rgba_png(
        width,
        height,
        vec![128; width as usize * height as usize * 4],
    )
    .unwrap()
}

fn bmp_1x1_red() -> Vec<u8> {
    let mut bytes = vec![0; 58];
    bytes[0..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&58_u32.to_le_bytes());
    bytes[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&1_i32.to_le_bytes());
    bytes[22..26].copy_from_slice(&1_i32.to_le_bytes());
    bytes[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24_u16.to_le_bytes());
    bytes[34..38].copy_from_slice(&4_u32.to_le_bytes());
    bytes[56] = 0xff;
    bytes
}

fn image(bytes: &[u8], mime_type: &str) -> ContentBlock {
    ContentBlock::Image(ImageContent {
        data: STANDARD.encode(bytes),
        mime_type: mime_type.to_string(),
    })
}

fn normalized_image(result: &ToolResult) -> &ImageContent {
    let ContentBlock::Image(image) = &result.content[1] else {
        panic!("expected the injected image after leading text");
    };
    image
}

fn dimensions(image: &ImageContent) -> (u32, u32) {
    assert_eq!(image.mime_type, "image/png");
    let bytes = STANDARD.decode(&image.data).unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

fn tool_message(messages: &[Message]) -> &pi_core::ToolResultMessage {
    messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("tool result message")
}

#[tokio::test]
async fn product_normalizes_after_overlay_with_current_model_profile_before_history_and_reload() {
    let original = png(240, 60);
    let fixture = Fixture::new(
        vec![
            ContentBlock::Text(TextContent::new("before")),
            image(&original, "image/png"),
            ContentBlock::Text(TextContent::new("after")),
        ],
        Some(json!({"survives":true})),
        true,
        json!({"maxWidth":120,"maxHeight":120,"maxBytes":1000000,"jpegQuality":80}),
    )
    .await;
    fixture.assert_registration_order();

    let original_base64 = STANDARD.encode(&original);
    let (messages, result) = fixture.prompt().await;
    assert_eq!(fixture.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(result.structured_content, Some(json!({"survives":true})));
    assert_eq!(
        result
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(_) => "text",
                ContentBlock::Image(_) => "image",
                _ => "other",
            })
            .collect::<Vec<_>>(),
        ["text", "image", "text", "text"]
    );
    let resized = normalized_image(&result);
    assert_eq!(dimensions(resized), (120, 30));
    let ContentBlock::Text(note) = &result.content[2] else {
        panic!("resize note follows the normalized image");
    };
    assert_eq!(
        note.text,
        "[Image: original 240x60, displayed at 120x30. Multiply coordinates by 2.00 to map to original image.]"
    );
    assert_eq!(tool_message(&messages).content, result.content);
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second = requests[1].to_string();
        assert!(second.contains(&resized.data));
        assert!(second.contains("original 240x60"));
        assert!(!second.contains(&original_base64));
    }
    let history = std::fs::read_to_string(fixture.session.current().history().path()).unwrap();
    assert!(history.contains(&resized.data));
    assert!(history.contains("original 240x60"));
    assert!(!history.contains(&original_base64));

    fixture
        .session
        .current()
        .set_model("fixture".into(), "narrow".into())
        .unwrap();
    let (_, narrow_result) = fixture.prompt().await;
    assert_eq!(fixture.invocations.load(Ordering::SeqCst), 2);
    assert_eq!(dimensions(normalized_image(&narrow_result)), (60, 15));
    assert_eq!(
        narrow_result.structured_content,
        Some(json!({"survives":true}))
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[3]["model"], "narrow");
        assert!(
            requests[3]
                .to_string()
                .contains(&normalized_image(&narrow_result).data)
        );
    }

    // A reload must rebuild the final policy factory with new settings, not reuse an old hook.
    std::fs::write(
        fixture._directory.path().join("agent/settings.json"),
        r#"{"images":{"autoResize":false}}"#,
    )
    .unwrap();
    let previous = fixture.session.current();
    fixture.session.reload().await.unwrap();
    assert!(!Arc::ptr_eq(&previous, &fixture.session.current()));
    fixture.assert_registration_order();
    let (_, reloaded_result) = fixture.prompt().await;
    assert_eq!(fixture.invocations.load(Ordering::SeqCst), 3);
    assert_eq!(dimensions(normalized_image(&reloaded_result)), (240, 60));
    assert_eq!(normalized_image(&reloaded_result).data, original_base64);
    assert_eq!(reloaded_result.content.len(), 3);
    assert_eq!(
        reloaded_result.structured_content,
        Some(json!({"survives":true}))
    );
    {
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        assert_eq!(requests[5]["model"], "narrow");
        assert!(requests[5].to_string().contains(&original_base64));
    }

    fixture.shutdown().await;
}

#[tokio::test]
async fn product_disabled_resize_still_converts_bmp_and_preserves_failed_images() {
    let invalid = ImageContent {
        data: STANDARD.encode(b"not-an-image"),
        mime_type: "image/png".to_string(),
    };
    let fixture = Fixture::new(
        vec![
            ContentBlock::Text(TextContent::new("before")),
            image(&bmp_1x1_red(), "image/bmp"),
            ContentBlock::Image(invalid.clone()),
            ContentBlock::Text(TextContent::new("after")),
        ],
        None,
        false,
        json!({"maxWidth":1,"maxHeight":1,"maxBytes":1,"jpegQuality":1}),
    )
    .await;
    fixture.assert_registration_order();

    let (messages, result) = fixture.prompt().await;
    assert_eq!(result.structured_content, None);
    assert_eq!(result.content.len(), 5);
    let ContentBlock::Image(converted) = &result.content[1] else {
        panic!("BMP remains an image");
    };
    assert_eq!(converted.mime_type, "image/png");
    assert_eq!(dimensions(converted), (1, 1));
    assert_eq!(
        result.content[2],
        ContentBlock::Text(TextContent::new(
            "[Image converted from image/bmp to image/png.]"
        ))
    );
    assert_eq!(result.content[3], ContentBlock::Image(invalid.clone()));
    assert_eq!(tool_message(&messages).content, result.content);
    {
        let requests = fixture.requests.lock().unwrap();
        let second = requests[1].to_string();
        assert!(second.contains(&converted.data));
        assert!(second.contains(&invalid.data));
        assert!(second.contains("converted from image/bmp"));
    }

    fixture.shutdown().await;
}
