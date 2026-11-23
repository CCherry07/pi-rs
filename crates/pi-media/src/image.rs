//! Image normalization shared by CLI attachments and the read tool.
use std::io::Cursor;

use base64::Engine as _;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use pi_core::{ContentBlock, ImageContent, ModelImageResizeOptions, TextContent};

use crate::MediaError;

/// Pi inline-image defaults. The encoded limit is exclusive and includes base64
/// expansion. Disabling resize preserves supported bytes; BMP still becomes PNG.
#[derive(Debug, Clone, Copy)]
pub struct ImagePolicy {
    pub auto_resize: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub max_encoded_bytes: usize,
    pub jpeg_quality: u8,
}

impl Default for ImagePolicy {
    fn default() -> Self {
        Self {
            auto_resize: true,
            max_width: 2_000,
            max_height: 2_000,
            max_encoded_bytes: 4_718_592,
            jpeg_quality: 80,
        }
    }
}

impl ImagePolicy {
    /// Overlay one model or fallback profile on the processor defaults.
    pub fn with_resize_options(mut self, options: ModelImageResizeOptions) -> Self {
        if let Some(value) = options.max_width {
            self.max_width = value;
        }
        if let Some(value) = options.max_height {
            self.max_height = value;
        }
        if let Some(value) = options.max_bytes {
            self.max_encoded_bytes = value;
        }
        if let Some(value) = options.jpeg_quality {
            self.jpeg_quality = value;
        }
        self
    }
}

#[derive(Debug)]
pub struct ProcessedImage {
    pub content: ImageContent,
    pub hints: Vec<String>,
}

/// Normalize every image in a tool result while preserving all other blocks and their order.
///
/// Returns `None` when the caller can keep the original content allocation. Invalid base64 and
/// image-processing failures deliberately leave that image untouched: a tool has already produced
/// the result, so normalization must not silently discard it.
pub fn normalize_tool_result_images(
    content: &[ContentBlock],
    policy: &ImagePolicy,
) -> Option<Vec<ContentBlock>> {
    if !content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)))
    {
        return None;
    }

    let mut changed = false;
    let mut normalized = Vec::with_capacity(content.len());
    for block in content {
        let ContentBlock::Image(original) = block else {
            normalized.push(block.clone());
            continue;
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&original.data) else {
            normalized.push(block.clone());
            continue;
        };
        let Ok(processed) = process_image(&bytes, policy) else {
            normalized.push(block.clone());
            continue;
        };
        if processed.content == *original && processed.hints.is_empty() {
            normalized.push(block.clone());
            continue;
        }

        normalized.push(ContentBlock::Image(processed.content));
        if !processed.hints.is_empty() {
            normalized.push(ContentBlock::Text(TextContent::new(
                processed.hints.join("\n"),
            )));
        }
        changed = true;
    }

    changed.then_some(normalized)
}

/// Identify, normalize and encode one image. This performs CPU work synchronously;
/// async callers should run it on a blocking worker.
pub fn process_image(bytes: &[u8], policy: &ImagePolicy) -> Result<ProcessedImage, MediaError> {
    let source_mime = detect_mime_type(bytes).ok_or(MediaError::InvalidImage)?;
    // Normalize BMP before applying inline limits, even with resizing disabled.
    let converted = if source_mime == "image/bmp" {
        let image = decode_image(bytes).map_err(|_| MediaError::InvalidImage)?;
        Some(encode_image_png(&image).map_err(|_| MediaError::InvalidImage)?)
    } else {
        None
    };
    let bytes = converted.as_deref().unwrap_or(bytes);
    let mime = if converted.is_some() {
        "image/png"
    } else {
        source_mime
    };
    let hints = |output_mime: &str| {
        if converted.is_some() {
            vec![format!(
                "[Image converted from {source_mime} to {output_mime}.]"
            )]
        } else {
            Vec::new()
        }
    };
    if !policy.auto_resize {
        return Ok(processed(bytes, mime, hints(mime)));
    }
    if policy.max_width == 0
        || policy.max_height == 0
        || policy.max_encoded_bytes == 0
        || !(1..=100).contains(&policy.jpeg_quality)
    {
        return Err(MediaError::InvalidImagePolicy);
    }
    // Pi treats decode failures during resize as an inline-size omission, not a
    // normalization failure. EXIF orientation is measured before dimension limits.
    let decoded = decode_image(bytes).map_err(|_| MediaError::ImageSizeLimit)?;
    let original_width = decoded.width();
    let original_height = decoded.height();
    if original_width <= policy.max_width
        && original_height <= policy.max_height
        && base64_size(bytes.len()) < policy.max_encoded_bytes
    {
        return Ok(processed(bytes, mime, hints(mime)));
    }
    let (mut width, mut height) = fit_dimensions(
        original_width,
        original_height,
        policy.max_width,
        policy.max_height,
    );
    if width == 0 || height == 0 {
        return Err(MediaError::ImageSizeLimit);
    }
    loop {
        let resized = decoded.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
        if let Some((encoded, output_mime)) = encode_within_limit(&resized, policy)? {
            let mut hints = hints(output_mime);
            let scale = f64::from(original_width) / f64::from(width);
            // Any re-encoding sets wasResized in Pi, even when dimensions did not change.
            hints.push(format!(
                "[Image: original {original_width}x{original_height}, displayed at {width}x{height}. Multiply coordinates by {scale:.2} to map to original image.]"
            ));
            return Ok(processed(&encoded, output_mime, hints));
        }
        if width == 1 && height == 1 {
            return Err(MediaError::ImageSizeLimit);
        }
        width = (width.saturating_mul(3) / 4).max(1);
        height = (height.saturating_mul(3) / 4).max(1);
    }
}

fn decode_image(bytes: &[u8]) -> image::ImageResult<DynamicImage> {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = if matches!(detect_mime_type(bytes), Some("image/jpeg" | "image/webp")) {
        decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms)
    } else {
        image::metadata::Orientation::NoTransforms
    };
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(DynamicImage::ImageRgba8(image.into_rgba8()))
}

fn encode_image_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, MediaError> {
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality)
        .encode_image(&image.to_rgb8())
        .map_err(|_| MediaError::ImageSizeLimit)?;
    Ok(output)
}

fn encode_within_limit(
    image: &DynamicImage,
    policy: &ImagePolicy,
) -> Result<Option<(Vec<u8>, &'static str)>, MediaError> {
    let png = encode_image_png(image)?;
    let jpeg = encode_image_jpeg(image, policy.jpeg_quality)?;
    // Compare the encoded payload, not raw bytes: ties prefer PNG.
    let (preferred, mime) = if base64_size(jpeg.len()) < base64_size(png.len()) {
        (jpeg, "image/jpeg")
    } else {
        (png, "image/png")
    };
    if base64_size(preferred.len()) < policy.max_encoded_bytes {
        return Ok(Some((preferred, mime)));
    }
    // This is Pi's ordered, deduplicated ladder, not a descending sort.
    for quality in [85, 70, 55, 40] {
        if quality == policy.jpeg_quality {
            continue;
        }
        let jpeg = encode_image_jpeg(image, quality)?;
        if base64_size(jpeg.len()) < policy.max_encoded_bytes {
            return Ok(Some((jpeg, "image/jpeg")));
        }
    }
    Ok(None)
}

/// Encode native clipboard RGBA pixels as PNG without resizing them.
pub fn encode_rgba_png(width: u32, height: u32, pixels: Vec<u8>) -> Result<Vec<u8>, MediaError> {
    let expected = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || expected.checked_mul(4) != Some(pixels.len() as u64) {
        return Err(MediaError::InvalidImage);
    }
    let rgba = RgbaImage::from_raw(width, height, pixels).ok_or(MediaError::InvalidImage)?;
    encode_image_png(&DynamicImage::ImageRgba8(rgba))
}

fn encode_image_png(image: &DynamicImage) -> Result<Vec<u8>, MediaError> {
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| MediaError::ImageSizeLimit)?;
    Ok(output.into_inner())
}

fn processed(bytes: &[u8], mime_type: &str, hints: Vec<String>) -> ProcessedImage {
    ProcessedImage {
        content: ImageContent {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime_type.to_string(),
        },
        hints,
    }
}

fn base64_size(bytes: usize) -> usize {
    bytes.div_ceil(3).saturating_mul(4)
}

fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let (mut width, mut height) = (width, height);
    if width > max_width {
        height = (f64::from(height) * f64::from(max_width) / f64::from(width)).round() as u32;
        width = max_width;
    }
    if height > max_height {
        width = (f64::from(width) * f64::from(max_height) / f64::from(height)).round() as u32;
        height = max_height;
    }
    (width, height)
}

/// Detect supported image bytes using current Pi's magic-byte rules, independent
/// of a filename or an untrusted declared MIME type. APNG and JPEG-LS are excluded.
pub fn detect_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes.get(8..12) == Some(&13_u32.to_be_bytes())
        && bytes.get(12..16) == Some(b"IHDR")
        && !is_animated_png(bytes)
    {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.get(3) != Some(&0xf7) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if is_bmp(bytes) {
        Some("image/bmp")
    } else {
        None
    }
}

fn is_animated_png(bytes: &[u8]) -> bool {
    let mut offset = 8usize;
    while offset.saturating_add(8) <= bytes.len().min(4_100) {
        let Some(length_bytes) = bytes.get(offset..offset + 4) else {
            return false;
        };
        let length = u32::from_be_bytes(length_bytes.try_into().expect("four bytes")) as usize;
        let chunk_type = bytes.get(offset + 4..offset + 8);
        if chunk_type == Some(b"acTL") {
            return true;
        }
        if chunk_type == Some(b"IDAT") {
            return false;
        }
        let next = offset.saturating_add(12).saturating_add(length);
        if next <= offset || next > bytes.len().min(4_100) {
            return false;
        }
        offset = next;
    }
    false
}

fn is_bmp(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"BM") || bytes.len() < 26 {
        return false;
    }
    let read_u16 = |offset: usize| {
        bytes
            .get(offset..offset + 2)
            .and_then(|value| value.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let read_u32 = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let Some(file_size) = read_u32(2) else {
        return false;
    };
    let Some(pixel_offset) = read_u32(10) else {
        return false;
    };
    let Some(header_size) = read_u32(14) else {
        return false;
    };
    if (file_size != 0 && file_size < 26)
        || u64::from(pixel_offset) < 14 + u64::from(header_size)
        || (file_size != 0 && pixel_offset >= file_size)
    {
        return false;
    }
    let (planes, bits) = if header_size == 12 {
        (read_u16(22), read_u16(24))
    } else if (40..=124).contains(&header_size) {
        (read_u16(26), read_u16(28))
    } else {
        return false;
    };
    planes == Some(1) && matches!(bits, Some(1 | 4 | 8 | 16 | 24 | 32))
}
