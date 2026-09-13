//! Pi-compatible YAML frontmatter extraction and decoding.

use serde::de::DeserializeOwned;

/// How a Markdown document's opening frontmatter was recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmatterStatus {
    /// The normalized document does not begin with `---`.
    Absent,
    /// The document begins with `---` but has no closing delimiter.
    Unterminated,
    /// Both delimiters were found and `frontmatter` contains the header.
    Present,
}

/// A normalized Markdown document with an optional decoded YAML header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterDocument<T> {
    pub status: FrontmatterStatus,
    pub frontmatter: Option<T>,
    pub body: String,
}

/// Splits Pi-style YAML frontmatter without interpreting its fields.
///
/// A UTF-8 BOM is removed and newlines are normalized before delimiters are
/// inspected. Missing and unterminated headers retain the complete normalized
/// document as their body, matching current Pi behavior.
pub fn split_frontmatter(content: &str) -> FrontmatterDocument<String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return FrontmatterDocument {
            status: FrontmatterStatus::Absent,
            frontmatter: None,
            body: normalized,
        };
    }
    let Some(relative_end) = normalized[3..].find("\n---") else {
        return FrontmatterDocument {
            status: FrontmatterStatus::Unterminated,
            frontmatter: None,
            body: normalized,
        };
    };
    let end = 3 + relative_end;
    let yaml_start = 4.min(end);
    FrontmatterDocument {
        status: FrontmatterStatus::Present,
        frontmatter: Some(normalized[yaml_start..end].to_string()),
        body: normalized[end + 4..].trim().to_string(),
    }
}

/// Splits frontmatter and deserializes a present YAML header into `T`.
///
/// Empty and comment-only headers decode as an empty mapping. Missing or
/// unterminated headers return `None`, leaving callers to apply their own
/// required/optional policy.
pub fn parse_frontmatter<T>(content: &str) -> Result<FrontmatterDocument<T>, serde_yaml::Error>
where
    T: DeserializeOwned,
{
    let document = split_frontmatter(content);
    let frontmatter = document
        .frontmatter
        .map(|yaml| {
            // Pi's YAML decoder treats the newline immediately before the
            // closing delimiter as part of the YAML document. Preserve that
            // behavior for literal block scalars even though the raw splitter
            // excludes the delimiter's leading newline.
            let yaml = format!("{yaml}\n");
            let value = serde_yaml::from_str::<serde_yaml::Value>(&yaml)?;
            if value == serde_yaml::Value::Null {
                serde_yaml::from_value(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
            } else {
                serde_yaml::from_str(&yaml)
            }
        })
        .transpose()?;
    Ok(FrontmatterDocument {
        status: document.status,
        frontmatter,
        body: document.body,
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Header {
        name: Option<String>,
        description: Option<String>,
    }

    #[test]
    fn parses_bom_crlf_multiline_yaml_and_trims_the_body() {
        let document = parse_frontmatter::<Header>(
            "\u{feff}---\r\nname: demo\r\ndescription: |\r\n  line one\r\n  line two\r\n---\r\n\r\nBody\r\n",
        )
        .unwrap();
        assert_eq!(document.status, FrontmatterStatus::Present);
        assert_eq!(
            document.frontmatter,
            Some(Header {
                name: Some("demo".into()),
                description: Some("line one\nline two\n".into()),
            })
        );
        assert_eq!(document.body, "Body");
    }

    #[test]
    fn absent_and_unterminated_headers_remain_document_content() {
        let absent = split_frontmatter("\u{feff}Body\r\ntext");
        assert_eq!(absent.status, FrontmatterStatus::Absent);
        assert_eq!(absent.frontmatter, None);
        assert_eq!(absent.body, "Body\ntext");

        let unterminated = split_frontmatter("---\r\nname: demo\r\nBody");
        assert_eq!(unterminated.status, FrontmatterStatus::Unterminated);
        assert_eq!(unterminated.frontmatter, None);
        assert_eq!(unterminated.body, "---\nname: demo\nBody");
    }

    #[test]
    fn empty_headers_decode_as_empty_mappings_and_invalid_yaml_fails() {
        let empty = parse_frontmatter::<Header>("---\n# comment\n---\nBody").unwrap();
        assert_eq!(
            empty.frontmatter,
            Some(Header {
                name: None,
                description: None,
            })
        );
        assert!(parse_frontmatter::<Header>("---\nname: [\n---\nBody").is_err());
    }
}
