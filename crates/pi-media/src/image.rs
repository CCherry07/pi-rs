//! Image normalization shared by CLI attachments and the read tool.
use std::io::Cursor;

use base64::Engine as _;
use image::{DynamicImage, ImageFormat, RgbaImage};
use pi_core::ImageContent;

use crate::MediaError;

/// Pi inline-image defaults. The encoded limit is exclusive and includes base64
/// expansion. Disabling resize preserves supported bytes; BMP still becomes PNG.
#[derive(Debug, Clone, Copy)]
pub struct ImagePolicy {
    pub auto_resize: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub max_encoded_bytes: usize,
}

impl Default for ImagePolicy {
    fn default() -> Self {
        Self {
            auto_resize: true,
            max_width: 2_000,
            max_height: 2_000,
            max_encoded_bytes: 4_718_592,
        }
    }
}

#[derive(Debug)]
pub struct ProcessedImage {
    pub content: ImageContent,
    pub hints: Vec<String>,
}

/// Identify, normalize and encode one image. This performs CPU work synchronously;
/// async callers should run it on a blocking worker.
pub fn process_image(bytes: &[u8], policy: &ImagePolicy) -> Result<ProcessedImage, MediaError> {
    if policy.max_width == 0 || policy.max_height == 0 || policy.max_encoded_bytes <= 4 {
        return Err(MediaError::InvalidImagePolicy);
    }
    let mime = detect_mime_type(bytes).ok_or(MediaError::InvalidImage)?;
    if !policy.auto_resize && mime != "image/bmp" {
        return Ok(processed(bytes, mime, Vec::new()));
    }
    let decoded = image::load_from_memory(bytes).map_err(|_| MediaError::InvalidImage)?;
    let original_width = decoded.width();
    let original_height = decoded.height();
    if mime != "image/bmp"
        && original_width <= policy.max_width
        && original_height <= policy.max_height
        && base64_size(bytes.len()) < policy.max_encoded_bytes
    {
        return Ok(processed(bytes, mime, Vec::new()));
    }
    let (mut width, mut height) = if policy.auto_resize {
        fit_dimensions(
            original_width,
            original_height,
            policy.max_width,
            policy.max_height,
        )
    } else {
        (original_width, original_height)
    };
    loop {
        let resized = if width == original_width && height == original_height {
            decoded.clone()
        } else {
            decoded.resize_exact(width, height, image::imageops::FilterType::Lanczos3)
        };
        let encoded = encode_image_png(&resized)?;
        if !policy.auto_resize || base64_size(encoded.len()) < policy.max_encoded_bytes {
            let mut hints = Vec::new();
            if mime != "image/png" {
                hints.push(format!("[Image converted from {mime} to image/png.]"));
            }
            if width != original_width || height != original_height {
                let scale = f64::from(original_width) / f64::from(width);
                hints.push(format!(
                    "[Image: original {original_width}x{original_height}, displayed at {width}x{height}. Multiply coordinates by {scale:.2} to map to original image.]"
                ));
            }
            return Ok(processed(&encoded, "image/png", hints));
        }
        if width == 1 && height == 1 {
            return Err(MediaError::ImageSizeLimit);
        }
        width = (width.saturating_mul(3) / 4).max(1);
        height = (height.saturating_mul(3) / 4).max(1);
    }
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
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
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
    } else if bytes.starts_with(b"GIF") {
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
