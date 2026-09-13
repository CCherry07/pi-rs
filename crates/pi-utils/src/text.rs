/// Escapes text for XML element content and attribute values.
pub fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::escape_xml;

    #[test]
    fn escapes_xml_metacharacters() {
        assert_eq!(escape_xml("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    }
}
