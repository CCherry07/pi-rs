use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageFormat, Rgba, RgbaImage};
use pi_core::{AbortHandle, ContentBlock, ModelInput, ModelSpec, ToolCallId, ToolResult};
use pi_plugin::{
    ModelsContextAccess, PluginContextEpoch, PluginContextResult, RegistriesBuilder,
    SessionContextAccess, Tool, ToolContext, ToolUpdateSink, UiContextAccess,
};
use pi_plugin_read::{ConfiguredReadPlugin, ReadTool};
use serde_json::{Value, json};

struct SelectedModel(Mutex<ModelSpec>);
impl ModelsContextAccess for SelectedModel {
    fn model(&self) -> PluginContextResult<Option<ModelSpec>> {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
}
impl SessionContextAccess for SelectedModel {}
impl UiContextAccess for SelectedModel {}

fn fixture() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    RgbaImage::from_pixel(120, 60, Rgba([12, 34, 56, 255]))
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn context(directory: &std::path::Path, model: Arc<SelectedModel>) -> ToolContext {
    let (_, signal) = AbortHandle::new();
    ToolContext::with_plugin_context(
        directory.into(),
        signal,
        PluginContextEpoch::new(model).context(),
    )
}

async fn execute(tool: &dyn Tool, context: ToolContext, path: &str) -> ToolResult {
    let (updates, _) = ToolUpdateSink::channel();
    tool.execute(
        context,
        ToolCallId::new("read"),
        json!({"path":path}),
        updates,
    )
    .await
    .unwrap()
}

fn dimensions(result: &ToolResult) -> (u32, u32) {
    assert!(!result.is_error);
    let ContentBlock::Image(image) = &result.content[1] else {
        panic!("expected image: {result:?}")
    };
    let decoded = image::load_from_memory(&STANDARD.decode(&image.data).unwrap()).unwrap();
    (decoded.width(), decoded.height())
}

#[tokio::test]
async fn registered_tool_reads_current_model_profile_and_falls_back_only_when_absent() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("image.png"), fixture()).unwrap();
    let fallback = serde_json::from_value(json!({"maxWidth":40,"maxHeight":40})).unwrap();
    let (_, registries) = RegistriesBuilder::new()
        .register_plugins(vec![Arc::new(
            ConfiguredReadPlugin::new(true).with_resize_options(fallback),
        )])
        .unwrap();
    let tool = registries.tool("read").unwrap();
    let mut model = ModelSpec::new("arbitrary-provider", "vision", "Vision", "test");
    model.input.push(ModelInput::Image);
    let selected = Arc::new(SelectedModel(Mutex::new(model)));
    let context = context(directory.path(), selected.clone());

    for (limits, expected) in [
        (Some(json!({"images":{"resize":{"maxWidth":30}}})), (30, 15)),
        // An explicitly empty model profile replaces, rather than merges, the fallback.
        (Some(json!({"images":{"resize":{}}})), (120, 60)),
        (Some(json!({"images":{"maxPerRequest":1}})), (40, 20)),
        (None, (40, 20)),
    ] {
        selected.0.lock().unwrap().input_limits =
            limits.map(|limits| serde_json::from_value(limits).unwrap());
        let result = execute(tool.as_ref(), context.clone(), "image.png").await;
        assert_eq!(dimensions(&result), expected);
        assert!(result.details.is_none());
        assert_eq!(result.structured_content.as_ref().unwrap()["type"], "image");
    }
    let (_, signal) = AbortHandle::new();
    let standalone = ToolContext::standalone(directory.path().into(), signal);
    assert_eq!(
        dimensions(&execute(tool.as_ref(), standalone, "image.png").await),
        (40, 20)
    );
}

#[tokio::test]
async fn resize_disabled_preserves_bytes_and_reports_text_only_model_without_dropping_image() {
    let directory = tempfile::tempdir().unwrap();
    let original = fixture();
    std::fs::write(directory.path().join("image.png"), &original).unwrap();
    let mut model = ModelSpec::new("test", "text-only", "Text only", "test");
    model.input_limits = Some(
        serde_json::from_value(json!({"images":{"resize":{
            "maxWidth":1,"maxHeight":1,"maxBytes":1
        }}}))
        .unwrap(),
    );
    let (_, registries) = RegistriesBuilder::new()
        .register_plugins(vec![Arc::new(ConfiguredReadPlugin::new(false))])
        .unwrap();
    let tool = registries.tool("read").unwrap();
    let result = execute(
        tool.as_ref(),
        context(directory.path(), Arc::new(SelectedModel(Mutex::new(model)))),
        "image.png",
    )
    .await;
    assert_eq!(dimensions(&result), (120, 60));
    assert_eq!(
        result.details,
        Some(json!({"imageUnsupportedByModel":"text-only"}))
    );
    assert_eq!(
        result.structured_content.as_ref().unwrap()["note"],
        "Read image file [image/png]"
    );
    let ContentBlock::Image(image) = &result.content[1] else {
        unreachable!()
    };
    assert_eq!(STANDARD.decode(&image.data).unwrap(), original);
}

#[tokio::test]
async fn conversion_and_resize_failures_are_successful_structured_text_omissions() {
    let directory = tempfile::tempdir().unwrap();
    let mut bmp = vec![0; 30];
    bmp[0..2].copy_from_slice(b"BM");
    bmp[2..6].copy_from_slice(&58_u32.to_le_bytes());
    bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bmp[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bmp[28..30].copy_from_slice(&24_u16.to_le_bytes());
    for (name, bytes, mime, reason) in [
        (
            "broken.bmp",
            bmp,
            "image/bmp",
            "converted to a supported inline image format",
        ),
        (
            "broken.gif",
            b"GIF89a broken".to_vec(),
            "image/gif",
            "resized below the inline image size limit",
        ),
    ] {
        std::fs::write(directory.path().join(name), bytes).unwrap();
        let (_, signal) = AbortHandle::new();
        let result = execute(
            &ReadTool,
            ToolContext::standalone(directory.path().into(), signal),
            name,
        )
        .await;
        let note = format!("Read image file [{mime}]\n[Image omitted: could not be {reason}.]");
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.structured_content, Some(Value::String(note.clone())));
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == note));
        assert!(result.details.is_none());
    }
}

#[tokio::test]
async fn model_encoded_size_limit_produces_omission_instead_of_a_tool_error() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("image.png"), fixture()).unwrap();
    let mut model = ModelSpec::new("test", "vision", "Vision", "test");
    model.input_limits =
        Some(serde_json::from_value(json!({"images":{"resize":{"maxBytes":1}}})).unwrap());
    let result = execute(
        &ReadTool,
        context(directory.path(), Arc::new(SelectedModel(Mutex::new(model)))),
        "image.png",
    )
    .await;
    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);
    assert_eq!(
        result.structured_content,
        Some(json!(
            "Read image file [image/png]\n[Image omitted: could not be resized below the inline image size limit.]"
        ))
    );
    assert!(result.details.is_none());
}
