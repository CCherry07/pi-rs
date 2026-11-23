use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use pi_core::{ContentBlock, ImageContent, TextContent};
use pi_media::{
    MediaError,
    image::{
        ImagePolicy, detect_mime_type, encode_rgba_png, normalize_tool_result_images, process_image,
    },
};

fn fixture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
        width,
        height,
        Rgba([12, 34, 56, 255]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    image.to_rgb8().write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

fn image_block(bytes: &[u8], mime_type: &str) -> ContentBlock {
    ContentBlock::Image(ImageContent {
        data: STANDARD.encode(bytes),
        mime_type: mime_type.to_string(),
    })
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text))
}

#[test]
fn tool_result_normalizer_returns_none_without_images_or_for_images_within_limits() {
    let text_only = vec![text_block("no images here")];
    assert_eq!(
        normalize_tool_result_images(&text_only, &ImagePolicy::default()),
        None
    );

    let tiny_png = fixture(ImageFormat::Png, 1, 1);
    let within_limits = vec![
        text_block("screenshot"),
        image_block(&tiny_png, "image/png"),
    ];
    assert_eq!(
        normalize_tool_result_images(&within_limits, &ImagePolicy::default()),
        None
    );
}

#[test]
fn tool_result_normalizer_resizes_and_preserves_surrounding_block_order() {
    let oversized = fixture(ImageFormat::Png, 2_400, 4_800);
    let content = vec![
        text_block("before"),
        image_block(&oversized, "image/png"),
        text_block("after"),
    ];

    let normalized =
        normalize_tool_result_images(&content, &ImagePolicy::default()).expect("image changed");
    assert_eq!(normalized.len(), 4);
    assert_eq!(normalized[0], text_block("before"));
    assert_eq!(normalized[3], text_block("after"));
    let ContentBlock::Image(image) = &normalized[1] else {
        panic!("normalized image must remain in place");
    };
    let encoded = STANDARD.decode(&image.data).unwrap();
    assert!(image.data.len() < ImagePolicy::default().max_encoded_bytes);
    let decoded = image::load_from_memory(&encoded).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1_000, 2_000));
    let ContentBlock::Text(note) = &normalized[2] else {
        panic!("dimension note must immediately follow the image");
    };
    assert_eq!(
        note.text,
        "[Image: original 2400x4800, displayed at 1000x2000. Multiply coordinates by 2.40 to map to original image.]"
    );
}

#[test]
fn tool_result_normalizer_honors_disabled_resize() {
    let oversized = fixture(ImageFormat::Png, 2_400, 100);
    let content = vec![image_block(&oversized, "image/png")];
    assert_eq!(
        normalize_tool_result_images(
            &content,
            &ImagePolicy {
                auto_resize: false,
                ..ImagePolicy::default()
            }
        ),
        None
    );
}

#[test]
fn tool_result_normalizer_converts_bmp_and_inserts_hint_when_resize_is_disabled() {
    let bmp = fixture(ImageFormat::Bmp, 1, 1);
    let content = vec![image_block(&bmp, "image/bmp")];
    let normalized = normalize_tool_result_images(
        &content,
        &ImagePolicy {
            auto_resize: false,
            ..ImagePolicy::default()
        },
    )
    .expect("BMP normalization changes the block");

    assert_eq!(normalized.len(), 2);
    let ContentBlock::Image(image) = &normalized[0] else {
        panic!("expected image");
    };
    assert_eq!(image.mime_type, "image/png");
    assert!(
        STANDARD
            .decode(&image.data)
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    assert_eq!(
        normalized[1],
        text_block("[Image converted from image/bmp to image/png.]")
    );
}

#[test]
fn tool_result_normalizer_keeps_invalid_base64_and_undecodable_images() {
    for image in [
        ImageContent {
            data: "%%% invalid base64 %%%".to_string(),
            mime_type: "image/png".to_string(),
        },
        ImageContent {
            data: STANDARD.encode(b"not-an-image"),
            mime_type: "image/png".to_string(),
        },
    ] {
        let content = vec![ContentBlock::Image(image)];
        assert_eq!(
            normalize_tool_result_images(&content, &ImagePolicy::default()),
            None
        );
    }
}

#[test]
fn detects_complete_gif_signatures_and_bmp_magic() {
    assert_eq!(detect_mime_type(b"GIF87a"), Some("image/gif"));
    assert_eq!(detect_mime_type(b"GIF89a"), Some("image/gif"));
    assert_eq!(detect_mime_type(b"GIF"), None);
    assert_eq!(detect_mime_type(b"GIF87"), None);
    assert_eq!(detect_mime_type(b"GIF89"), None);
    assert_eq!(
        detect_mime_type(&fixture(ImageFormat::Bmp, 1, 1)),
        Some("image/bmp")
    );
}

#[test]
fn process_image_keeps_caller_input_bytes_intact() {
    let bytes = fixture(ImageFormat::Png, 300, 100);
    let original = bytes.clone();
    let result = process_image(
        &bytes,
        &ImagePolicy {
            max_width: 100,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(bytes, original);
    assert_eq!(
        image::load_from_memory(&STANDARD.decode(result.content.data).unwrap())
            .unwrap()
            .width(),
        100
    );
}

#[test]
fn preserves_supported_small_images_without_reencoding() {
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/gif"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = fixture(format, 2, 3);
        let result = process_image(&bytes, &ImagePolicy::default()).unwrap();
        assert_eq!(result.content.mime_type, mime);
        assert_eq!(STANDARD.decode(result.content.data).unwrap(), bytes);
        assert!(result.hints.is_empty());
    }
}

#[test]
fn scales_dimensions_and_reports_coordinate_mapping() {
    let bytes = fixture(ImageFormat::Png, 3000, 30);
    let result = process_image(&bytes, &ImagePolicy::default()).unwrap();
    let decoded = image::load_from_memory(&STANDARD.decode(result.content.data).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2000, 20));
    assert!(result.hints[0].contains("Multiply coordinates by 1.50"));
    let unchanged = process_image(
        &bytes,
        &ImagePolicy {
            auto_resize: false,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(STANDARD.decode(unchanged.content.data).unwrap(), bytes);
}

#[test]
fn bmp_conversion_honors_disabled_resize() {
    let bytes = fixture(ImageFormat::Bmp, 30, 3);
    for auto_resize in [false, true] {
        let result = process_image(
            &bytes,
            &ImagePolicy {
                auto_resize,
                max_width: 10,
                ..ImagePolicy::default()
            },
        )
        .unwrap();
        assert_eq!(result.content.mime_type, "image/png");
        assert!(result.hints[0].contains("converted from image/bmp"));
        let decoded =
            image::load_from_memory(&STANDARD.decode(result.content.data).unwrap()).unwrap();
        assert_eq!(decoded.width(), if auto_resize { 10 } else { 30 });
    }
}

#[test]
fn enforces_encoded_limit_and_rejects_impossible_limits() {
    let bytes = fixture(ImageFormat::Png, 256, 256);
    let result = process_image(
        &bytes,
        &ImagePolicy {
            max_encoded_bytes: 256,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert!(result.content.data.len() < 256);
    assert!(matches!(
        process_image(
            &bytes,
            &ImagePolicy {
                max_encoded_bytes: 5,
                ..ImagePolicy::default()
            }
        ),
        Err(MediaError::ImageSizeLimit)
    ));
    assert!(matches!(
        process_image(
            &bytes,
            &ImagePolicy {
                max_width: 0,
                ..ImagePolicy::default()
            }
        ),
        Err(MediaError::InvalidImagePolicy)
    ));
}

#[test]
fn rejects_corrupt_and_excluded_formats_without_panicking() {
    assert_eq!(detect_mime_type(b"GIF"), None);
    assert_eq!(detect_mime_type(b"GIF87"), None);
    assert_eq!(detect_mime_type(b"GIF89"), None);
    assert_eq!(detect_mime_type(b"GIF87a"), Some("image/gif"));
    assert_eq!(detect_mime_type(b"GIF89a"), Some("image/gif"));
    assert!(matches!(
        process_image(b"GIF89a broken", &ImagePolicy::default()),
        Err(MediaError::ImageSizeLimit)
    ));
    assert_eq!(detect_mime_type(b"not an image.png"), None);
    assert_eq!(detect_mime_type(&[0xff, 0xd8, 0xff, 0xf7]), None);
    let mut png = fixture(ImageFormat::Png, 1, 1);
    png.splice(33..33, [0, 0, 0, 8, b'a', b'c', b'T', b'L']);
    assert_eq!(detect_mime_type(&png), None);
    png[8..12].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(detect_mime_type(&png), None);
    let mut bmp = fixture(ImageFormat::Bmp, 1, 1);
    bmp[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(detect_mime_type(&bmp), None);
}

fn noisy_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 7_u32;
    let pixels = (0..width * height * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let image = image::RgbImage::from_raw(width, height, pixels).unwrap();
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn jpeg(bytes: &[u8], quality: u8) -> Vec<u8> {
    let image = image::load_from_memory(bytes).unwrap().to_rgb8();
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
        .encode_image(&image)
        .unwrap();
    encoded
}

#[test]
fn prefers_smaller_jpeg_and_notes_reencoding_without_dimension_changes() {
    let bytes = noisy_png(96, 96);
    let result = process_image(
        &bytes,
        &ImagePolicy {
            max_encoded_bytes: STANDARD.encode(&bytes).len(),
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(result.content.mime_type, "image/jpeg");
    assert_eq!(
        STANDARD.decode(&result.content.data).unwrap(),
        jpeg(&bytes, 80)
    );
    assert_eq!(
        result.hints,
        [
            "[Image: original 96x96, displayed at 96x96. Multiply coordinates by 1.00 to map to original image.]"
        ]
    );
}

#[test]
fn resized_photo_jpeg_remains_jpeg_instead_of_a_larger_png() {
    let (width, height) = (1_200, 800);
    let mut seed = 1_u32;
    let pixels = image::RgbImage::from_fn(width, height, |x, y| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
        let noise = (seed % 6) as u8;
        image::Rgb([
            (x * 200 / width) as u8 + noise,
            (y * 200 / height) as u8 + noise,
            (128.0 + (f64::from(x) / 30.0).sin() * 60.0) as u8 + noise,
        ])
    });
    let mut source = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut source, 85)
        .encode_image(&pixels)
        .unwrap();
    let result = process_image(
        &source,
        &ImagePolicy {
            max_width: 1_000,
            max_height: 1_000,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(result.content.mime_type, "image/jpeg");
    let decoded = image::load_from_memory(&STANDARD.decode(&result.content.data).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1_000, 667));
    assert!(result.content.data.len() < source.len().div_ceil(3) * 4 * 2);
    assert!(result.hints[0].contains("original 1200x800, displayed at 1000x667"));
}

#[test]
fn quality_ladder_uses_exclusive_base64_limit_before_shrinking() {
    let bytes = noisy_png(96, 96);
    let limit = STANDARD.encode(jpeg(&bytes, 80)).len();
    assert!(STANDARD.encode(jpeg(&bytes, 85)).len() >= limit);
    assert!(STANDARD.encode(jpeg(&bytes, 70)).len() < limit);
    let result = process_image(
        &bytes,
        &ImagePolicy {
            max_encoded_bytes: limit,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(
        STANDARD.decode(&result.content.data).unwrap(),
        jpeg(&bytes, 70)
    );
    let custom = process_image(
        &bytes,
        &ImagePolicy {
            jpeg_quality: 40,
            max_encoded_bytes: limit,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(
        STANDARD.decode(&custom.content.data).unwrap(),
        jpeg(&bytes, 40)
    );
}

#[test]
fn prefers_png_when_smaller_and_only_normalization_gets_conversion_hint() {
    let bytes = fixture(ImageFormat::Png, 64, 64);
    let mut padded = bytes.clone();
    padded.extend([0; 10_000]);
    let result = process_image(
        &padded,
        &ImagePolicy {
            max_encoded_bytes: 1_000,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(result.content.mime_type, "image/png");
    assert_eq!(result.hints.len(), 1);
    assert!(result.hints[0].contains("displayed at 64x64"));
    let result = process_image(
        &fixture(ImageFormat::Jpeg, 64, 64),
        &ImagePolicy {
            max_width: 32,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert!(result.hints.iter().all(|hint| !hint.contains("converted")));
}

#[test]
fn shrinking_preserves_pi_sequential_dimension_rounding() {
    // Width-first rounding followed by height rounding gives 1x2, whereas
    // a single min-ratio fit would give 2x2.
    let bytes = fixture(ImageFormat::Png, 3, 4);
    let result = process_image(
        &bytes,
        &ImagePolicy {
            max_width: 2,
            max_height: 2,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    let decoded = image::load_from_memory(&STANDARD.decode(result.content.data).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1, 2));
}

#[test]
fn exif_orientation_is_applied_before_limits_and_coordinate_notes() {
    let mut bytes = fixture(ImageFormat::Jpeg, 4, 2);
    // JPEG APP1 containing little-endian TIFF orientation 6 (90 degrees).
    let exif = [
        0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0,
        0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    // XMP is also APP1, but must not hide a later EXIF orientation segment.
    let payload = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
    let mut xmp = vec![0xff, 0xe1];
    xmp.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    xmp.extend_from_slice(payload);
    xmp.extend_from_slice(&exif);
    bytes.splice(2..2, xmp);
    let preserved = process_image(
        &bytes,
        &ImagePolicy {
            max_width: 2,
            max_height: 4,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(STANDARD.decode(preserved.content.data).unwrap(), bytes);
    let resized = process_image(
        &bytes,
        &ImagePolicy {
            max_width: 1,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    let decoded =
        image::load_from_memory(&STANDARD.decode(&resized.content.data).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (1, 2));
    assert_eq!(
        resized.hints,
        [
            "[Image: original 2x4, displayed at 1x2. Multiply coordinates by 2.00 to map to original image.]"
        ]
    );
}

#[test]
fn resize_disabled_ignores_invalid_resize_limits() {
    let bytes = fixture(ImageFormat::Png, 2, 2);
    let result = process_image(
        &bytes,
        &ImagePolicy {
            auto_resize: false,
            max_width: 0,
            max_encoded_bytes: 0,
            jpeg_quality: 0,
            ..ImagePolicy::default()
        },
    )
    .unwrap();
    assert_eq!(STANDARD.decode(result.content.data).unwrap(), bytes);
}

#[test]
fn clipboard_pixels_round_trip_and_malformed_buffers_fail() {
    let pixels = vec![1, 2, 3, 255, 4, 5, 6, 128];
    let png = encode_rgba_png(2, 1, pixels.clone()).unwrap();
    assert_eq!(
        image::load_from_memory(&png).unwrap().to_rgba8().into_raw(),
        pixels
    );
    assert!(encode_rgba_png(1, 1, vec![0; 3]).is_err());
    assert!(encode_rgba_png(u32::MAX, u32::MAX, Vec::new()).is_err());
}
