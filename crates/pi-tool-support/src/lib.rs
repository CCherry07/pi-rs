use std::path::{Component, Path, PathBuf};

use pi_core::{ToolError, ToolExecutionMode, ToolSpec};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization as _;

pub const MAX_OUTPUT_BYTES: usize = 1_000_000;
pub const MAX_OUTPUT_LINES: usize = 2000;
pub const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

pub fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        parameters,
        execution_mode: ToolExecutionMode::Parallel,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
    }
}

pub fn with_prompt(
    mut tool: ToolSpec,
    snippet: impl Into<String>,
    guidelines: impl IntoIterator<Item = impl Into<String>>,
) -> ToolSpec {
    tool.prompt_snippet = Some(snippet.into());
    tool.prompt_guidelines = guidelines.into_iter().map(Into::into).collect();
    tool
}

/// Resolve a tool path with the same input semantics as Pi's `resolveToCwd`.
///
/// This deliberately does not confine the result to `cwd`: Pi's project trust
/// controls project resource loading, not filesystem tool access.
pub fn resolve_to_cwd(cwd: &Path, input: &str) -> Result<PathBuf, ToolError> {
    let candidate = normalize_tool_path_input(input)?;
    let cwd = absolute_lexical_path(cwd)?;
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };
    Ok(lexical_normalize(&joined))
}

/// Resolve a path for reading, including Pi's macOS filename fallbacks.
pub fn resolve_read_path(cwd: &Path, input: &str) -> Result<PathBuf, ToolError> {
    let resolved = resolve_to_cwd(cwd, input)?;
    if resolved.exists() {
        return Ok(resolved);
    }
    let Some(text) = resolved.to_str() else {
        return Ok(resolved);
    };

    let am_pm = macos_screenshot_path_variant(text);
    if am_pm != text && Path::new(&am_pm).exists() {
        return Ok(PathBuf::from(am_pm));
    }

    let nfd = text.nfd().collect::<String>();
    if nfd != text && Path::new(&nfd).exists() {
        return Ok(PathBuf::from(nfd));
    }

    let curly = text.replace('\'', "\u{2019}");
    if curly != text && Path::new(&curly).exists() {
        return Ok(PathBuf::from(curly));
    }

    let nfd_curly = nfd.replace('\'', "\u{2019}");
    if nfd_curly != text && Path::new(&nfd_curly).exists() {
        return Ok(PathBuf::from(nfd_curly));
    }

    Ok(resolved)
}

fn normalize_tool_path_input(input: &str) -> Result<PathBuf, ToolError> {
    let mut normalized = input
        .chars()
        .map(|character| {
            if matches!(
                character,
                '\u{00a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            ) {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if normalized.starts_with('@') {
        normalized.remove(0);
    }
    normalized = normalize_windows_shell_path(normalized);

    let is_tilde_child =
        normalized.starts_with("~/") || cfg!(windows) && normalized.starts_with("~\\");
    if normalized == "~" || is_tilde_child {
        let home = user_home_dir().ok_or_else(|| execution("cannot determine home directory"))?;
        if normalized == "~" {
            return Ok(home);
        }
        return Ok(home.join(&normalized[2..]));
    }

    if normalized.starts_with("file://") {
        let url = url::Url::parse(&normalized)
            .map_err(|error| invalid(format!("invalid file URL: {error}")))?;
        return url
            .to_file_path()
            .map_err(|()| invalid("file URL cannot be converted to a local path"));
    }

    Ok(PathBuf::from(normalized))
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf, ToolError> {
    if path.is_absolute() {
        return Ok(lexical_normalize(path));
    }
    let process_cwd = std::env::current_dir()
        .map_err(|error| execution(format!("cannot access process working directory: {error}")))?;
    Ok(lexical_normalize(&process_cwd.join(path)))
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(windows_user_home_dir)
}

#[cfg(windows)]
fn windows_user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            Some(PathBuf::from(drive).join(path))
        })
}

#[cfg(not(windows))]
fn windows_user_home_dir() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn normalize_windows_shell_path(input: String) -> String {
    if !input.starts_with('/') || input.starts_with("//") || input.contains('\\') {
        return input;
    }
    let path = input
        .strip_prefix("/mnt/")
        .or_else(|| input.strip_prefix("/cygdrive/"))
        .or_else(|| input.strip_prefix('/'));
    let Some(path) = path else { return input };
    let mut characters = path.chars();
    let Some(drive) = characters.next().filter(char::is_ascii_alphabetic) else {
        return input;
    };
    let remainder = characters.as_str();
    if !remainder.is_empty() && !remainder.starts_with('/') {
        return input;
    }
    format!(
        "{}:\\{}",
        drive.to_ascii_uppercase(),
        remainder.trim_start_matches('/').replace('/', "\\")
    )
}

#[cfg(not(windows))]
fn normalize_windows_shell_path(input: String) -> String {
    input
}

fn macos_screenshot_path_variant(path: &str) -> String {
    ["AM", "PM", "am", "pm"]
        .into_iter()
        .fold(path.to_string(), |path, marker| {
            path.replace(&format!(" {marker}."), &format!("\u{202f}{marker}."))
        })
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            component => output.push(component.as_os_str()),
        }
    }
    output
}

pub fn require_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("`{key}` must be a string")))
}

pub fn optional_positive_usize(
    value: &Value,
    key: &str,
    default: usize,
) -> Result<usize, ToolError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let number = value
                .as_u64()
                .and_then(|number| usize::try_from(number).ok())
                .ok_or_else(|| invalid(format!("`{key}` must be a positive integer")))?;
            if number == 0 {
                Err(invalid(format!("`{key}` must be greater than 0")))
            } else {
                Ok(number)
            }
        }
    }
}

pub fn truncate_head(lines: impl IntoIterator<Item = String>, max_lines: usize) -> (String, bool) {
    let mut output = String::new();
    let mut truncated = false;
    for (index, line) in lines.into_iter().enumerate() {
        if index >= max_lines || output.len().saturating_add(line.len() + 1) > MAX_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
    }
    (output, truncated)
}

pub fn truncate_tail(text: &str) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES && text.lines().count() <= MAX_OUTPUT_LINES {
        return (text.to_string(), false);
    }
    let mut kept = Vec::new();
    let mut bytes = 0usize;
    for line in text.lines().rev() {
        if kept.len() >= MAX_OUTPUT_LINES || bytes.saturating_add(line.len() + 1) > MAX_OUTPUT_BYTES
        {
            break;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    kept.reverse();
    (kept.join("\n"), true)
}

const NIBBLE_STR: &[u8; 16] = b"ZPMQVRWSNKTXJBYH";

pub fn hashline_tag(line_index: usize, line: &str, had_bom: bool) -> String {
    let mut significant = String::new();
    if had_bom && line_index == 0 {
        significant.push('\u{feff}');
    }
    significant.extend(
        line.trim_end_matches('\r')
            .chars()
            .filter(|c| !c.is_whitespace()),
    );
    let seed = if significant.chars().any(char::is_alphanumeric) {
        0
    } else {
        line_index as u32
    };
    let byte = (xxhash_rust::xxh32::xxh32(significant.as_bytes(), seed) & 0xff) as usize;
    format!(
        "{}#{}{}",
        line_index + 1,
        NIBBLE_STR[byte & 0x0f] as char,
        NIBBLE_STR[(byte >> 4) & 0x0f] as char
    )
}

pub fn snapshot_and_atomic_replace(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
) -> std::io::Result<()> {
    if replacement.len() > MAX_WRITE_BYTES {
        return Err(std::io::Error::other("replacement exceeds size limit"));
    }
    let before = std::fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(std::io::Error::other(
            "target must be a regular non-symlink file",
        ));
    }
    if std::fs::read(path)? != expected {
        return Err(std::io::Error::other(
            "file changed while edit was in progress",
        ));
    }
    let after = std::fs::symlink_metadata(path)?;
    if !same_file_identity(&before, &after) {
        return Err(std::io::Error::other(
            "file identity changed while edit was in progress",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write as _;
    temp.write_all(replacement)?;
    temp.as_file().set_permissions(before.permissions())?;
    temp.as_file().sync_all()?;
    if std::fs::read(path)? != expected
        || !same_file_identity(&before, &std::fs::symlink_metadata(path)?)
    {
        return Err(std::io::Error::other("file changed before persist"));
    }
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev() && left.ino() == right.ino() && left.len() == right.len()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

pub fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments(message.into())
}

pub fn execution(message: impl Into<String>) -> ToolError {
    ToolError::Execution(message.into())
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn absolute_paths_are_not_confined_to_the_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let target = root.path().join("shared").join("SKILL.md");
        std::fs::create_dir(&cwd).unwrap();

        assert_eq!(
            resolve_to_cwd(&cwd, target.to_str().unwrap()).unwrap(),
            target
        );
    }

    #[test]
    fn parent_segments_may_resolve_outside_the_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        std::fs::create_dir(&cwd).unwrap();

        assert_eq!(
            resolve_to_cwd(&cwd, "../shared/SKILL.md").unwrap(),
            root.path().join("shared").join("SKILL.md")
        );
    }

    #[test]
    fn empty_input_resolves_to_the_working_directory() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(resolve_to_cwd(root.path(), "").unwrap(), root.path());
    }

    #[test]
    fn leading_tilde_expands_to_the_user_home() {
        let root = tempfile::tempdir().unwrap();
        let home = std::env::var_os("HOME").expect("HOME is required by the test environment");

        assert_eq!(
            resolve_to_cwd(root.path(), "~/.agents/skills/example/SKILL.md").unwrap(),
            PathBuf::from(home).join(".agents/skills/example/SKILL.md")
        );
    }

    #[test]
    fn tool_path_input_strips_at_and_normalizes_unicode_spaces() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            resolve_to_cwd(root.path(), "@folder\u{202f}name/file.txt").unwrap(),
            root.path().join("folder name/file.txt")
        );
    }

    #[test]
    fn file_urls_are_converted_to_local_paths() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("folder name/file.txt");
        let url = url::Url::from_file_path(&target).unwrap();

        assert_eq!(resolve_to_cwd(root.path(), url.as_str()).unwrap(), target);
    }

    #[test]
    fn read_path_uses_the_macos_screenshot_space_variant() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("Screenshot 1.00.00\u{202f}PM.png");
        std::fs::write(&actual, "image").unwrap();

        assert_eq!(
            resolve_read_path(root.path(), "Screenshot 1.00.00 PM.png").unwrap(),
            actual
        );
    }

    #[test]
    fn read_path_resolves_nfd_and_curly_quote_filename() {
        let root = tempfile::tempdir().unwrap();
        let actual_name = "Capture d\u{2019}écran.txt".nfd().collect::<String>();
        let actual = root.path().join(actual_name);
        std::fs::write(&actual, "capture").unwrap();

        let resolved = resolve_read_path(root.path(), "Capture d'écran.txt").unwrap();
        assert_eq!(std::fs::read_to_string(resolved).unwrap(), "capture");
    }
}
