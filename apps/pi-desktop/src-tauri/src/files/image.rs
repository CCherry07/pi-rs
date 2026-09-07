use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_INLINE_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

fn image_extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

fn image_mime_type(path: &str) -> Option<&'static str> {
    match image_extension(path)?.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tiff" | "tif" => Some("image/tiff"),
        _ => None,
    }
}

fn needs_heif_conversion(path: &str) -> bool {
    matches!(
        image_extension(path).as_deref(),
        Some("heic") | Some("heif")
    )
}

#[cfg(target_os = "macos")]
fn temp_converted_image_path(path: &str) -> PathBuf {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let safe_stem = stem
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("pi-desktop-image-{safe_stem}-{timestamp}.jpg"))
}

#[cfg(target_os = "macos")]
fn convert_heif_to_jpeg(path: &str) -> Result<Vec<u8>, String> {
    let output_path = temp_converted_image_path(path);
    let status = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg"])
        .arg(path)
        .arg("--out")
        .arg(&output_path)
        .status()
        .map_err(|error| format!("Failed to launch HEIC/HEIF conversion for {path}: {error}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&output_path);
        return Err(format!("Failed to convert HEIC/HEIF image to JPEG: {path}"));
    }
    let bytes = std::fs::read(&output_path).map_err(|error| {
        format!(
            "Failed to read converted JPEG for {path} at {}: {error}",
            output_path.display()
        )
    })?;
    let _ = std::fs::remove_file(&output_path);
    if bytes.is_empty() {
        return Err(format!("Converted JPEG is empty: {path}"));
    }
    Ok(bytes)
}

pub(crate) fn normalize_path(raw: &str) -> String {
    let path = raw.trim();
    let file_uri_path = path
        .strip_prefix("file://localhost")
        .or_else(|| path.strip_prefix("file://"));
    let Some(path) = file_uri_path else {
        return path.to_string();
    };

    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = |value| match value {
                b'0'..=b'9' => Some(value - b'0'),
                b'a'..=b'f' => Some(value - b'a' + 10),
                b'A'..=b'F' => Some(value - b'A' + 10),
                _ => None,
            };
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

pub(crate) fn read_as_data_url(path: &str) -> Result<String, String> {
    let path = normalize_path(path);
    if path.is_empty() {
        return Err("Image path is required".to_string());
    }
    if needs_heif_conversion(&path) {
        #[cfg(target_os = "macos")]
        {
            let encoded = STANDARD.encode(convert_heif_to_jpeg(&path)?);
            return Ok(format!("data:image/jpeg;base64,{encoded}"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Err(format!(
                "HEIC/HEIF images are not supported on this platform; convert to JPEG or PNG first: {path}"
            ));
        }
    }

    let mime_type = image_mime_type(&path)
        .ok_or_else(|| format!("Unsupported or missing image extension for path: {path}"))?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("Failed to stat image file at {path}: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("Image path must not be a symlink: {path}"));
    }
    if !metadata.is_file() {
        return Err(format!("Image path is not a file: {path}"));
    }
    if metadata.len() > MAX_INLINE_IMAGE_BYTES {
        return Err(format!(
            "Image file exceeds maximum size of {MAX_INLINE_IMAGE_BYTES} bytes: {path}"
        ));
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("Failed to read image file at {path}: {error}"))?;
    if bytes.is_empty() {
        return Err(format!("Image file is empty: {path}"));
    }
    Ok(format!(
        "data:{mime_type};base64,{}",
        STANDARD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_file_urls_without_decoding_plain_paths() {
        assert_eq!(
            normalize_path("file:///tmp/path%20with%20spaces/image.png"),
            "/tmp/path with spaces/image.png"
        );
        assert_eq!(
            normalize_path("/tmp/report%20final.png"),
            "/tmp/report%20final.png"
        );
    }

    #[test]
    fn reads_supported_image_as_data_url() {
        let root = std::env::temp_dir().join(format!("pi-desktop-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("image.png");
        std::fs::write(&path, [0x89, 0x50, 0x4e, 0x47]).unwrap();

        let result = read_as_data_url(&format!("file://{}", path.display())).unwrap();
        assert!(result.starts_with("data:image/png;base64,"));

        let _ = std::fs::remove_dir_all(root);
    }
}
