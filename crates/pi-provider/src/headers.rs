use std::collections::BTreeMap;

/// Replaces a header by name, comparing existing names case-insensitively.
///
/// The inserted spelling is retained so provider-specific wire formats can
/// preserve their canonical header casing.
pub fn insert_header(
    headers: &mut BTreeMap<String, String>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) {
    let name = name.as_ref();
    remove_header(headers, name);
    headers.insert(name.to_string(), value.into());
}

/// Removes the existing header with `name`, comparing names case-insensitively.
pub fn remove_header(headers: &mut BTreeMap<String, String>, name: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_replaces_case_insensitively_and_preserves_new_spelling() {
        let mut headers = BTreeMap::from([
            ("authorization".to_string(), "old".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ]);

        insert_header(&mut headers, "Authorization", "Bearer token");

        assert_eq!(
            headers.get("Authorization"),
            Some(&"Bearer token".to_string())
        );
        assert!(!headers.contains_key("authorization"));
        assert_eq!(headers.get("Accept"), Some(&"application/json".to_string()));
    }

    #[test]
    fn remove_matches_header_names_case_insensitively() {
        let mut headers = BTreeMap::from([("X-Request-Id".to_string(), "123".to_string())]);

        remove_header(&mut headers, "x-request-id");

        assert!(headers.is_empty());
    }
}
