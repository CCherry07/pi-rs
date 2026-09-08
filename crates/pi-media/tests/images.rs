use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use pi_media::{
    MediaError,
    image::{ImagePolicy, detect_mime_type, encode_rgba_png, process_image},
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
    assert!(matches!(
        process_image(b"GIF broken", &ImagePolicy::default()),
        Err(MediaError::InvalidImage)
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
