//! Pi's model-facing MCP CallToolResult envelope and nested schema references.
use serde_json::{Value, json};

pub(crate) fn result_schema(structured: Option<Value>) -> Value {
    json!({
        "type":"object", "properties": {
            "content":{"type":"array", "items":{
                "type":"object", "properties":{
                    "type":{"enum":["text","image","audio","resource_link","resource"]},
                    "text":{"type":"string"}, "data":{"type":"string"}, "mimeType":{"type":"string"}
                }, "required":["type"], "additionalProperties":true
            }},
            "structuredContent": structured.map(relocate).unwrap_or_else(|| json!({})),
            "isError":{"type":"boolean"}
        }, "required":["content"], "additionalProperties":true
    })
}

fn relocate(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(relocate).collect()),
        Value::Object(mut values) => {
            for (key, value) in &mut values {
                if key == "$ref"
                    && let Some(reference) = value.as_str()
                    && (reference == "#" || reference.starts_with("#/"))
                {
                    *value =
                        Value::String(format!("#/properties/structuredContent{}", &reference[1..]));
                } else if matches!(key.as_str(), "const" | "enum" | "default" | "examples") {
                    // Data can contain keys named $ref; those are not schema references.
                } else if matches!(
                    key.as_str(),
                    "properties"
                        | "patternProperties"
                        | "dependentSchemas"
                        | "dependencies"
                        | "$defs"
                        | "definitions"
                ) && value.is_object()
                {
                    for schema in value.as_object_mut().unwrap().values_mut() {
                        *schema = relocate(std::mem::take(schema));
                    }
                } else {
                    *value = relocate(std::mem::take(value));
                }
            }
            Value::Object(values)
        }
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_matches_the_typescript_oracle() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/output-schemas.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let input = (!case["schema"].is_null()).then(|| case["schema"].clone());
            assert_eq!(result_schema(input), case["expected"]);
        }
    }

    #[test]
    fn wraps_remote_output_and_relocates_only_schema_references() {
        let schema = result_schema(Some(json!({
            "$defs":{"Item":{"type":"string"}},
            "properties":{"$ref":{"$ref":"#/$defs/Item"},"self":{"$ref":"#"}},
            "const":{"$ref":"#/literal"}, "examples":[{"$ref":"#/literal"}],
            "anyOf":[{"$ref":"https://example.test/schema"}]
        })));
        assert_eq!(schema["required"], json!(["content"]));
        let inner = &schema["properties"]["structuredContent"];
        assert_eq!(
            inner["properties"]["$ref"]["$ref"],
            "#/properties/structuredContent/$defs/Item"
        );
        assert_eq!(
            inner["properties"]["self"]["$ref"],
            "#/properties/structuredContent"
        );
        assert_eq!(inner["const"]["$ref"], "#/literal");
        assert_eq!(inner["examples"][0]["$ref"], "#/literal");
        assert_eq!(inner["anyOf"][0]["$ref"], "https://example.test/schema");
        assert_eq!(
            result_schema(None)["properties"]["structuredContent"],
            json!({})
        );
    }
}
