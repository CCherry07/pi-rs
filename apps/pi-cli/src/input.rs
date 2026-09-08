//! CLI file expansion; media processing stays in pi-media, session policy in pi-session.
use std::path::Path;

use pi_media::image::{ImagePolicy, detect_mime_type, process_image};
use pi_session::SessionInput;

pub(crate) fn prepare_input(
    arguments: &[String],
    stdin: Option<&str>,
    cwd: &Path,
    policy: &ImagePolicy,
) -> Result<Option<SessionInput>, String> {
    let mut text = stdin.unwrap_or_default().to_string();
    let mut messages = Vec::new();
    let mut images = Vec::new();
    for argument in arguments {
        let Some(file) = argument.strip_prefix('@') else {
            messages.push(argument.as_str());
            continue;
        };
        if file.is_empty() {
            return Err("@file requires a path".to_string());
        }
        let path =
            pi_tool_support::resolve_read_path(cwd, file).map_err(|error| error.to_string())?;
        let metadata =
            std::fs::metadata(&path).map_err(|_| format!("File not found: {}", path.display()))?;
        if metadata.len() == 0 && metadata.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("Could not read file {}: {error}", path.display()))?;
        if detect_mime_type(&bytes).is_some() {
            let note = match process_image(&bytes, policy) {
                Ok(processed) => {
                    images.push(processed.content);
                    processed.hints.join("\n")
                }
                Err(error) => error.to_string(),
            };
            text.push_str(&format!(
                "<file name=\"{}\">{note}</file>\n",
                path.display()
            ));
        } else {
            let content = String::from_utf8_lossy(&bytes);
            let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
            text.push_str(&format!(
                "<file name=\"{}\">\n{content}\n</file>\n",
                path.display()
            ));
        }
    }
    text.push_str(&messages.join(" "));
    if text.is_empty() && images.is_empty() {
        return Ok(None);
    }
    let input = SessionInput::new(text);
    Ok(Some(if images.is_empty() {
        input
    } else {
        input.with_images(images)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_media::image::encode_rgba_png;

    #[test]
    fn combines_stdin_files_images_and_prompt_in_pi_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "\u{feff}notes").unwrap();
        let png = encode_rgba_png(1, 1, vec![1, 2, 3, 255]).unwrap();
        std::fs::write(dir.path().join("image with spaces.bin"), &png).unwrap();
        let arguments =
            ["compare", "@notes.md", "@image with spaces.bin", "these"].map(String::from);
        let result = prepare_input(
            &arguments,
            Some("stdin\n"),
            dir.path(),
            &ImagePolicy::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            result.text(),
            format!(
                "stdin\n<file name=\"{}\">\nnotes\n</file>\n<file name=\"{}\"></file>\ncompare these",
                dir.path().join("notes.md").display(),
                dir.path().join("image with spaces.bin").display()
            )
        );
        assert_eq!(result.images().len(), 1);
        assert_eq!(result.images()[0].mime_type, "image/png");
    }

    #[test]
    fn image_only_missing_empty_and_corrupt_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let png = encode_rgba_png(1, 1, vec![1, 2, 3, 255]).unwrap();
        std::fs::write(dir.path().join("a.png"), png).unwrap();
        let prepare = |file: &str| {
            prepare_input(
                &[file.to_string()],
                None,
                dir.path(),
                &ImagePolicy::default(),
            )
        };
        assert_eq!(prepare("@a.png").unwrap().unwrap().images().len(), 1);
        assert!(
            prepare("@missing.png")
                .unwrap_err()
                .contains("File not found")
        );
        assert!(prepare("@").is_err());
        assert!(prepare("@.").is_err());
        std::fs::write(dir.path().join("empty"), []).unwrap();
        assert!(prepare("@empty").unwrap().is_none());
        std::fs::write(dir.path().join("broken.gif"), b"GIF broken").unwrap();
        let omitted = prepare("@broken.gif").unwrap().unwrap();
        assert!(omitted.images().is_empty());
        assert!(omitted.text().contains("[Image omitted:"));
    }

    #[test]
    fn honors_resize_policy_and_parent_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(
            dir.path().join("large.png"),
            encode_rgba_png(3000, 1, vec![255; 3000 * 4]).unwrap(),
        )
        .unwrap();
        for auto_resize in [false, true] {
            let input = prepare_input(
                &["@../large.png".to_string()],
                None,
                &child,
                &ImagePolicy {
                    auto_resize,
                    ..ImagePolicy::default()
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(input.text().contains("displayed at 2000x1"), auto_resize);
            assert_eq!(input.images().len(), 1);
        }
    }
}
