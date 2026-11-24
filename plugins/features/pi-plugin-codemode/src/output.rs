use base64::Engine;
use pi_core::{ContentBlock, ImageContent, TextContent};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct Output {
    items: Vec<(ContentBlock, bool)>,
}

fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(value))
}

impl Output {
    pub fn append(&mut self, kind: &str, value: &str, mime: &str) {
        let block = if kind == "image" {
            ContentBlock::Image(ImageContent {
                data: value.into(),
                mime_type: mime.into(),
            })
        } else {
            text(value)
        };
        self.items.push((block, kind == "console"));
    }

    /// Formatting and token truncation are host policy, applied after the script
    /// has settled. Images count toward the VM's production limit but never the
    /// model-facing text budget. Spill files contain the complete formatted text.
    pub fn content(
        &self,
        error: Option<&str>,
        generated_images: usize,
        budget: usize,
        elapsed_seconds: f64,
    ) -> (Vec<ContentBlock>, Option<PathBuf>) {
        let total = self
            .items
            .iter()
            .filter(|(block, console)| !console && matches!(block, ContentBlock::Text(_)))
            .count();
        let mut items = Vec::new();
        let mut index = 0;
        let mut console = Vec::new();
        for (block, is_console) in &self.items {
            match block {
                ContentBlock::Text(block) if *is_console => console.push(block.text.as_str()),
                ContentBlock::Text(block) if total > 1 => {
                    index += 1;
                    items.push(text(format!(
                        "==> text {index}/{total} <==\n{}",
                        block.text
                    )));
                }
                other => items.push(other.clone()),
            }
        }
        if !console.is_empty() {
            items.push(text(format!(
                "<console_output>\n{}\n</console_output>",
                console.join("\n")
            )));
        }
        if let Some(error) = error {
            items.push(text(format!("Script error:\n{error}")));
        }
        if generated_images > 0
            && !items
                .iter()
                .any(|item| matches!(item, ContentBlock::Image(_)))
        {
            let suffix = if generated_images == 1 { "" } else { "s" };
            items.push(text(format!("Note: models.generateImages() returned {generated_images} image{suffix} that the script did not show. Show each image block of result.output with image(block).")));
        }
        let items = join_adjacent_text(items);
        let (items, path) = truncate(items, budget);
        // Labels must remain beside the image, and never be cut by truncation.
        let mut labels = HashMap::new();
        let mut saved = Vec::new();
        for item in items {
            if let ContentBlock::Image(image) = &item {
                let label = labels
                    .entry(image.data.clone())
                    .or_insert_with(|| save_image(image));
                saved.push(text(label.clone()));
            }
            saved.push(item);
        }
        let status = if error.is_some() {
            "Script failed"
        } else {
            "Script completed"
        };
        let mut content = vec![text(format!(
            "{status}\nWall time {elapsed_seconds:.1} seconds\nOutput:\n"
        ))];
        content.extend(join_adjacent_text(saved));
        (content, path)
    }
}

fn join_adjacent_text(items: Vec<ContentBlock>) -> Vec<ContentBlock> {
    let mut joined = Vec::new();
    let mut buffer = String::new();
    let mut has_text = false;
    for item in items {
        match item {
            ContentBlock::Text(block) => {
                if !buffer.is_empty() && !buffer.ends_with('\n') {
                    buffer.push('\n');
                }
                buffer.push_str(&block.text);
                has_text = true;
            }
            image => {
                if has_text {
                    joined.push(text(std::mem::take(&mut buffer)));
                    has_text = false;
                }
                joined.push(image);
            }
        }
    }
    if has_text {
        joined.push(text(buffer));
    }
    joined
}

fn truncate(items: Vec<ContentBlock>, budget: usize) -> (Vec<ContentBlock>, Option<PathBuf>) {
    let texts: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            ContentBlock::Text(block) => Some(block.text.as_str()),
            _ => None,
        })
        .collect();
    let combined = texts.join("\n");
    let chars = combined.encode_utf16().count();
    if texts.is_empty() || chars <= budget {
        return (items, None);
    }
    let head = budget / 2;
    let tail = budget - head;
    let mut rendered = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{}…{} tokens truncated…{}",
        chars.div_ceil(4),
        combined.split('\n').count(),
        prefix(&combined, head),
        (chars - budget).div_ceil(4),
        suffix(&combined, tail),
    );
    let path = match save_file(".txt", combined.as_bytes()) {
        Ok(path) => {
            rendered.push_str(&format!(
                "\n\n[Full output: {} (read with offset/limit)]",
                path.display()
            ));
            Some(path)
        }
        Err(error) => {
            rendered.push_str(&format!("\n\n[Could not save the full output: {error}]"));
            None
        }
    };
    let mut truncated = vec![text(rendered)];
    truncated.extend(
        items
            .into_iter()
            .filter(|item| matches!(item, ContentBlock::Image(_))),
    );
    (truncated, path)
}

fn image_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        _ => None,
    }
}

fn save_file(extension: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("pi-codemode-")
        .suffix(extension)
        .tempfile()?;
    file.write_all(bytes)?;
    let (_, path) = file.keep().map_err(|error| error.error)?;
    Ok(path)
}

fn save_image(image: &ImageContent) -> String {
    // Node's Buffer.from(base64) accepts unused trailing bits. The prelude has
    // already checked alphabet, padding, length and the actual image signature.
    let decoder = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );
    let bytes = match decoder.decode(&image.data) {
        Ok(bytes) => bytes,
        Err(error) => return format!("[Image could not be saved: {error}]"),
    };
    let size = if bytes.len() < 1024 {
        format!("{}B", bytes.len())
    } else if bytes.len() < 1024 * 1024 {
        format!("{:.1}KB", bytes.len() as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes.len() as f64 / (1024.0 * 1024.0))
    };
    let kind = format!("{}, {size}", image.mime_type);
    match save_file(image_extension(&image.mime_type).unwrap_or(".img"), &bytes) {
        Ok(path) => format!("[Image saved to {} ({kind})]", path.display()),
        Err(error) => format!("[Image ({kind}) could not be saved: {error}]"),
    }
}

/// A JavaScript-character prefix without splitting a UTF-16 surrogate pair.
pub(crate) fn prefix(value: &str, chars: usize) -> &str {
    let mut remaining = chars;
    for (offset, ch) in value.char_indices() {
        if ch.len_utf16() > remaining {
            return &value[..offset];
        }
        remaining -= ch.len_utf16();
    }
    value
}

fn suffix(value: &str, chars: usize) -> &str {
    let mut remaining = chars;
    for (offset, ch) in value.char_indices().rev() {
        if ch.len_utf16() > remaining {
            return &value[offset + ch.len_utf8()..];
        }
        remaining -= ch.len_utf16();
    }
    value
}

pub(crate) fn preview(value: &str, chars: usize) -> String {
    if value.encode_utf16().count() > chars {
        format!("{}...", prefix(value, chars.saturating_sub(3)))
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_budget_keeps_head_tail_and_complete_spill() {
        let mut output = Output::default();
        output.append("text", "头😀中间😀尾", "");
        let (content, path) = output.content(None, 0, 4, 0.0);
        let path = path.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "头😀中间😀尾");
        let ContentBlock::Text(block) = &content[1] else {
            panic!()
        };
        assert!(block.text.contains("头…1 tokens truncated…尾"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_console_items_and_images_survive_zero_text_budget() {
        let mut output = Output::default();
        output.append("console", "", "");
        output.append("image", "iVBORw0KGgo=", "image/png");
        let (content, path) = output.content(None, 0, 0, 0.0);
        assert!(
            content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_)))
        );
        let path = path.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<console_output>\n\n</console_output>"
        );
        std::fs::remove_file(path).unwrap();
    }
}
