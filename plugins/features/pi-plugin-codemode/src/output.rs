use base64::Engine;
use pi_core::{ContentBlock, ImageContent, TextContent};

use crate::source::MAX_OUTPUT_BYTES;

#[derive(Default)]
pub(crate) struct Output {
    items: Vec<(ContentBlock, bool)>,
    produced_bytes: usize,
    produced_items: usize,
    retained_bytes: usize,
    budget: usize,
    pub truncated: bool,
}

impl Output {
    pub fn append(
        &mut self,
        kind: &str,
        value: &str,
        mime: &str,
        budget: usize,
    ) -> Result<(), String> {
        self.budget = budget;
        self.produced_items += 1;
        self.produced_bytes = self.produced_bytes.saturating_add(value.len());
        if self.produced_bytes > MAX_OUTPUT_BYTES || self.produced_items > 10_000 {
            return Err("Script output exceeds 16 MiB or 10000 items".into());
        }
        let remaining = budget.saturating_sub(self.retained_bytes);
        if kind == "image" {
            if !mime.starts_with("image/")
                || mime.len() <= 6
                || mime.len() > 100
                || value.is_empty()
                || !mime[6..]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
                || base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .is_err()
            {
                return Err("image() requires base64 image data and an image MIME type".into());
            }
            // Images cannot be truncated while remaining valid.
            if value.len() > remaining {
                self.truncated = true;
                return Ok(());
            }
            self.items.push((
                ContentBlock::Image(ImageContent {
                    data: value.into(),
                    mime_type: mime.into(),
                }),
                false,
            ));
            self.retained_bytes += value.len();
        } else {
            let value = prefix(value, remaining);
            self.truncated |= self.produced_bytes > budget;
            if !value.is_empty() || remaining > 0 {
                self.items.push((
                    ContentBlock::Text(TextContent::new(value)),
                    kind == "console",
                ));
                self.retained_bytes += value.len();
            }
        }
        Ok(())
    }

    pub fn content(&mut self, error: Option<&str>) -> Vec<ContentBlock> {
        let mut content = vec![ContentBlock::Text(TextContent::new(if error.is_some() {
            "Script failed"
        } else {
            "Script completed"
        }))];
        let total = self
            .items
            .iter()
            .filter(|(block, console)| !console && matches!(block, ContentBlock::Text(_)))
            .count();
        let mut index = 0;
        let mut console = String::new();
        for (block, is_console) in &self.items {
            match block {
                ContentBlock::Text(text) if *is_console => {
                    if !console.is_empty() {
                        console.push('\n');
                    }
                    console.push_str(&text.text);
                }
                ContentBlock::Text(text) if total > 1 => {
                    index += 1;
                    content.push(ContentBlock::Text(TextContent::new(format!(
                        "==> text {index}/{total} <==\n{}",
                        text.text
                    ))));
                }
                other => content.push(other.clone()),
            }
        }
        if !console.is_empty() {
            content.push(ContentBlock::Text(TextContent::new(format!(
                "<console_output>\n{console}\n</console_output>"
            ))));
        }
        // Count framing too: empty text items must not bypass a token budget
        // by producing arbitrarily many item labels or console newlines.
        let generated = content.split_off(1);
        let mut remaining = self.budget;
        for block in generated {
            match block {
                ContentBlock::Text(mut text) => {
                    let kept = prefix(&text.text, remaining).len();
                    self.truncated |= kept < text.text.len();
                    text.text.truncate(kept);
                    remaining -= kept;
                    if !text.text.is_empty() {
                        content.push(ContentBlock::Text(text));
                    }
                }
                ContentBlock::Image(image) if image.data.len() <= remaining => {
                    remaining -= image.data.len();
                    content.push(ContentBlock::Image(image));
                }
                _ => self.truncated = true,
            }
        }
        if self.truncated {
            content.push(ContentBlock::Text(TextContent::new(
                "[Output truncated to the requested budget.]",
            )));
        }
        if let Some(error) = error {
            content.push(ContentBlock::Text(TextContent::new(format!(
                "Script error: {}",
                prefix(error, 8000)
            ))));
        }
        content
    }
}

pub(crate) fn prefix(value: &str, bytes: usize) -> &str {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
