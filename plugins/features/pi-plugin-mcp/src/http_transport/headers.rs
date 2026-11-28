//! Preserve rmcp's newer protocol header contract; frozen legacy versions skip this module.
use base64::Engine;
use http::{HeaderName, HeaderValue};
use rmcp::model::{JsonObject, Tool};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Default)]
pub(super) struct ToolHeaders(HashMap<String, Vec<(String, HeaderName)>>);

impl ToolHeaders {
    pub(super) fn observe(&mut self, tools: &mut Vec<Tool>) {
        tools.retain(|tool| match parameter_rules(&tool.input_schema) {
            Some(rules) => {
                self.0.insert(tool.name.to_string(), rules);
                true
            }
            None => false,
        });
    }
}

fn nested_annotation(value: &Value) -> bool {
    value
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties
                .values()
                .any(|child| child.get("x-mcp-header").is_some() || nested_annotation(child))
        })
}

fn parameter_rules(schema: &JsonObject) -> Option<Vec<(String, HeaderName)>> {
    let mut rules = Vec::new();
    let mut seen = HashSet::new();
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (property, schema) in properties {
            if nested_annotation(schema) {
                return None;
            }
            let Some(annotation) = schema.get("x-mcp-header") else {
                continue;
            };
            let name = annotation.as_str()?;
            // Parsing the annotation itself rejects empty strings and non-token bytes.
            let normalized = HeaderName::from_bytes(name.as_bytes()).ok()?;
            if !seen.insert(normalized)
                || !matches!(
                    schema.get("type").and_then(Value::as_str),
                    Some("string" | "integer" | "boolean")
                )
            {
                return None;
            }
            let header = HeaderName::from_bytes(format!("mcp-param-{name}").as_bytes()).ok()?;
            rules.push((property.clone(), header));
        }
    }
    Some(rules)
}

fn encoded(value: &str) -> HeaderValue {
    let must_encode = value.starts_with([' ', '\t'])
        || value.ends_with([' ', '\t'])
        || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        || value.starts_with("=?base64?") && value.ends_with("?=");
    let value = if must_encode {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(value)
        )
    } else {
        value.to_owned()
    };
    let mut value = HeaderValue::from_str(&value).expect("visible ASCII or base64");
    value.set_sensitive(true);
    value
}

pub(super) fn standard_headers(
    request: &Value,
    tools: &ToolHeaders,
) -> Vec<(HeaderName, HeaderValue)> {
    let Some(method) = request["method"].as_str() else {
        return Vec::new();
    };
    let mut headers = Vec::new();
    if let Ok(value) = HeaderValue::from_str(method) {
        headers.push((HeaderName::from_static("mcp-method"), value));
    }
    let field = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => "uri",
        "tasks/get" | "tasks/update" | "tasks/cancel" => "taskId",
        _ => return headers,
    };
    if let Some(name) = request["params"][field].as_str() {
        headers.push((HeaderName::from_static("mcp-name"), encoded(name)));
        if method == "tools/call"
            && let Some(rules) = tools.0.get(name)
        {
            for (property, header) in rules {
                let value = &request["params"]["arguments"][property];
                let text = match value {
                    Value::String(value) => value.clone(),
                    Value::Number(_) | Value::Bool(_) => value.to_string(),
                    _ => continue,
                };
                headers.push((header.clone(), encoded(&text)));
            }
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(schema: Value) -> Tool {
        serde_json::from_value(json!({"name":"echo", "inputSchema":schema})).unwrap()
    }

    #[test]
    fn validated_parameters_and_special_names_are_encoded_without_header_injection() {
        let mut rules = ToolHeaders::default();
        let mut tools = vec![tool(json!({"type":"object","properties":{
            "tenant":{"type":"string","x-mcp-header":"Tenant"},
            "count":{"type":"integer","x-mcp-header":"Count"},
            "enabled":{"type":"boolean","x-mcp-header":"Enabled"}
        }}))];
        rules.observe(&mut tools);
        assert_eq!(tools.len(), 1);
        let request = json!({"method":"tools/call","params":{"name":"echo","arguments":{
            "tenant":"a\r\nb", "count":7,"enabled":false}}});
        let headers: http::HeaderMap = standard_headers(&request, &rules).into_iter().collect();
        assert_eq!(headers["mcp-method"], "tools/call");
        assert_eq!(headers["mcp-name"], "echo");
        assert_eq!(headers["mcp-param-tenant"], "=?base64?YQ0KYg==?=");
        assert_eq!(headers["mcp-param-count"], "7");
        assert_eq!(headers["mcp-param-enabled"], "false");
        for name in ["日本語", " a ", "=?base64?a?=", "a\tb"] {
            let request = json!({"method":"resources/read","params":{"uri":name}});
            let headers: http::HeaderMap = standard_headers(&request, &rules).into_iter().collect();
            assert_eq!(headers["mcp-name"], encoded(name));
            assert!(!headers.contains_key("mcp-param-tenant"));
        }
    }

    #[test]
    fn invalid_annotations_are_not_admitted_to_the_wire_header_cache() {
        let cases = [
            json!({"a":{"type":"string","x-mcp-header":""}}),
            json!({"a":{"type":"string","x-mcp-header":false}}),
            json!({"a":{"type":"number","x-mcp-header":"a"}}),
            json!({"a":{"type":"string","x-mcp-header":"bad name"}}),
            json!({"a":{"type":"string","x-mcp-header":"Key"},
                "b":{"type":"integer","x-mcp-header":"key"}}),
            json!({"a":{"type":"object","properties":{
                "b":{"type":"string","x-mcp-header":"Nested"}}}}),
        ];
        for properties in cases {
            let mut rules = ToolHeaders::default();
            let mut tools = vec![tool(json!({"type":"object","properties":properties}))];
            rules.observe(&mut tools);
            assert!(tools.is_empty());
            assert!(rules.0.is_empty());
        }
    }
}
