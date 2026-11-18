//! Model-facing TypeScript declarations, matching Pi's codemode schema renderer.

use std::collections::HashSet;

use pi_core::ToolSpec;
use serde_json::Value;

const MAX_INPUT_CHARS: usize = 16_000;
const MAX_REF_EXPANSIONS: usize = 32;

pub(crate) fn identifier(name: &str) -> String {
    let value: String = name
        .chars()
        .enumerate()
        .map(|(index, c)| {
            if c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit()) || c == '_' || c == '$'
            {
                c
            } else {
                '_'
            }
        })
        .collect();
    if value.is_empty() { "_".into() } else { value }
}

fn property_key(name: &str) -> String {
    if identifier(name) == name {
        name.into()
    } else {
        Value::String(name.into()).to_string()
    }
}

/// Pi uses JavaScript string length for declaration and inline-catalog budgets.
pub(crate) fn character_count(value: &str) -> usize {
    value.encode_utf16().count()
}

pub(crate) fn sample(tool: &ToolSpec) -> String {
    sample_with_name(tool, &identifier(&tool.name))
}

pub(crate) fn sample_with_name(tool: &ToolSpec, callable_name: &str) -> String {
    let mut description = tool.description.trim().to_string();
    let guidelines = tool
        .prompt_guidelines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>();
    if !guidelines.is_empty() {
        description.push_str("\n\n");
        description.push_str(&guidelines.join("\n"));
    }
    let input = schema_to_type(&tool.parameters, Some(MAX_INPUT_CHARS));
    let output = output_type(tool.output_schema.as_ref());
    format!(
        "{description}\n\ncodemode tool declaration:\n```ts\ndeclare const tools: {{ {}(args: {input}): Promise<{output}>; }};\n```",
        property_key(callable_name)
    )
}

pub(crate) fn describe_script_call(tool: &ToolSpec) -> String {
    format!(
        "{}\n\nCodemode: `tools.{}(args)` resolves to {}.",
        tool.description.trim(),
        identifier(&tool.name),
        describe_output(&tool.name, tool.output_schema.as_ref())
    )
}

fn output_type(schema: Option<&Value>) -> String {
    schema.map_or_else(|| "string".into(), |schema| schema_to_type(schema, None))
}

fn describe_output(name: &str, schema: Option<&Value>) -> String {
    let rendered = output_type(schema);
    if rendered == "string" {
        return "a string".into();
    }
    if let Some(schema) = schema
        && let Some(fields) =
            describe_fields(schema, schema, 1).or_else(|| describe_union_fields(schema, schema))
    {
        return format!("`{fields}`");
    }
    if !rendered.contains('\n') {
        return format!("`{rendered}`");
    }
    format!(
        "a value; `await describeTool({})` shows its type",
        Value::String(name.into())
    )
}

fn describe_fields(schema: &Value, root: &Value, depth: usize) -> Option<String> {
    let schema = follow_ref(schema, root);
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    let required = required_names(schema);
    let fields = properties
        .iter()
        .map(|(name, field)| {
            let field = follow_ref(field, root);
            let nested = field.get("const").map(Value::to_string).or_else(|| {
                (depth > 0)
                    .then(|| describe_fields(field, root, depth - 1))
                    .flatten()
            });
            format!(
                "{name}{}{}",
                if required.contains(name.as_str()) {
                    ""
                } else {
                    "?"
                },
                nested.map_or_else(String::new, |value| format!(": {value}"))
            )
        })
        .collect::<Vec<_>>();
    Some(format!("{{ {} }}", fields.join(", ")))
}

fn describe_union_fields(schema: &Value, root: &Value) -> Option<String> {
    let schema = follow_ref(schema, root);
    let variants = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))?
        .as_array()?;
    if variants.is_empty() {
        return None;
    }
    variants
        .iter()
        .map(|variant| describe_fields(variant, root, 0))
        .collect::<Option<Vec<_>>>()
        .map(|fields| fields.join(" | "))
}

fn follow_ref<'a>(mut schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut seen = HashSet::new();
    while let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if !seen.insert(reference) {
            break;
        }
        let Some(target) = resolve_schema_ref(reference, root) else {
            break;
        };
        schema = target;
    }
    schema
}

pub(crate) fn resolve_schema_ref<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    if reference != "#" && !reference.starts_with("#/") {
        return None;
    }
    let mut current = root;
    for segment in reference
        .get(2..)
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
    {
        let decoded = decode_segment(segment)?
            .replace("~1", "/")
            .replace("~0", "~");
        current = current.as_object()?.get(&decoded)?;
    }
    (current.is_object() || current.is_boolean()).then_some(current)
}

fn decode_segment(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

pub(crate) fn schema_to_type(schema: &Value, max_chars: Option<usize>) -> String {
    let render = |tags| {
        to_type(
            schema,
            &mut SchemaContext {
                root: schema,
                resolving: HashSet::new(),
                expansions: 0,
                tags,
            },
        )
    };
    let rendered = render(true);
    if max_chars.is_none_or(|limit| character_count(&rendered) <= limit) {
        return rendered;
    }
    let untagged = render(false);
    if max_chars.is_none_or(|limit| character_count(&untagged) <= limit) {
        untagged
    } else {
        "unknown".into()
    }
}

struct SchemaContext<'a> {
    root: &'a Value,
    resolving: HashSet<String>,
    expansions: usize,
    tags: bool,
}

fn union(types: impl IntoIterator<Item = String>) -> String {
    let mut unique = Vec::new();
    for value in types {
        if value == "unknown" {
            return value;
        }
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    if unique.is_empty() {
        "never".into()
    } else {
        unique.join(" | ")
    }
}

fn to_type(schema: &Value, context: &mut SchemaContext<'_>) -> String {
    if schema == &Value::Bool(false) {
        return "never".into();
    }
    let Some(object) = schema.as_object() else {
        return "unknown".into();
    };
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        if context.resolving.contains(reference) || context.expansions >= MAX_REF_EXPANSIONS {
            return "unknown".into();
        }
        let Some(target) = resolve_schema_ref(reference, context.root) else {
            return "unknown".into();
        };
        context.expansions += 1;
        context.resolving.insert(reference.into());
        let rendered = to_type(target, context);
        context.resolving.remove(reference);
        return rendered;
    }
    if let Some(value) = object.get("const") {
        return value.to_string();
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return union(values.iter().map(Value::to_string));
    }
    if let Some(variants) = object
        .get("anyOf")
        .and_then(Value::as_array)
        .or_else(|| object.get("oneOf").and_then(Value::as_array))
    {
        return union(variants.iter().map(|variant| to_type(variant, context)));
    }
    if let Some(variants) = object.get("allOf").and_then(Value::as_array) {
        let parts = variants
            .iter()
            .map(|variant| to_type(variant, context))
            .filter(|part| part != "unknown")
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>();
        return if parts.is_empty() {
            "unknown".into()
        } else {
            parts.join(" & ")
        };
    }
    if let Some(types) = object.get("type").and_then(Value::as_array) {
        return union(types.iter().map(|kind| {
            let mut variant = schema.clone();
            variant["type"] = kind.clone();
            to_type(&variant, context)
        }));
    }
    match object.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("integer" | "number") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("null") => "null".into(),
        Some("array") => array_type(schema, context),
        Some("object") => object_type(schema, context),
        None if ["properties", "additionalProperties", "required"]
            .iter()
            .any(|key| object.contains_key(*key)) =>
        {
            object_type(schema, context)
        }
        None if object.contains_key("items") || object.contains_key("prefixItems") => {
            array_type(schema, context)
        }
        _ => "unknown".into(),
    }
}

fn array_type(schema: &Value, context: &mut SchemaContext<'_>) -> String {
    if let Some(items) = schema.get("items").filter(|value| !value.is_array()) {
        return format!("Array<{}>", to_type(items, context));
    }
    if let Some(tuple) = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .or_else(|| schema.get("items").and_then(Value::as_array))
        .filter(|tuple| !tuple.is_empty())
    {
        return format!(
            "[{}]",
            tuple
                .iter()
                .map(|item| to_type(item, context))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    "unknown[]".into()
}

fn required_names(schema: &Value) -> HashSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn object_type(schema: &Value, context: &mut SchemaContext<'_>) -> String {
    let mut properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    properties.sort_by_key(|(name, _)| *name);
    let required = required_names(schema);
    let mut members = properties
        .iter()
        .map(|(name, property)| {
            format!(
                "{}{}: {};",
                property_key(name),
                if required.contains(name.as_str()) {
                    ""
                } else {
                    "?"
                },
                to_type(property, context)
            )
        })
        .collect::<Vec<_>>();
    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(additional) => {
            members.push(format!("[key: string]: {};", to_type(additional, context)))
        }
        None if properties.is_empty() => members.push("[key: string]: unknown;".into()),
        None => {}
    }
    if members.is_empty() {
        return "{}".into();
    }
    let comments = properties
        .iter()
        .map(|(_, property)| {
            let mut lines = property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split(['\r', '\n', '\u{2028}', '\u{2029}'])
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if context.tags {
                let tags = constraint_tags(property);
                if !tags.is_empty() {
                    lines.push(tags);
                }
            }
            lines
        })
        .collect::<Vec<_>>();
    if comments.iter().all(Vec::is_empty) {
        return format!("{{ {} }}", members.join(" "));
    }
    let mut lines = vec!["{".into()];
    for (index, comment) in comments.iter().enumerate() {
        lines.extend(comment.iter().map(|line| format!("  // {line}")));
        lines.push(format!("  {}", members[index].replace('\n', "\n  ")));
    }
    lines.extend(
        members[properties.len()..]
            .iter()
            .map(|member| format!("  {member}")),
    );
    lines.push("}".into());
    lines.join("\n")
}

fn constraint_tags(property: &Value) -> String {
    let Some(object) = property.as_object() else {
        return String::new();
    };
    let variants = object
        .get("anyOf")
        .and_then(Value::as_array)
        .or_else(|| object.get("oneOf").and_then(Value::as_array));
    let non_null = variants
        .into_iter()
        .flatten()
        .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
        .collect::<Vec<_>>();
    let mut schemas = vec![object.clone()];
    if non_null.len() == 1
        && let Some(branch) = non_null[0].as_object()
    {
        schemas.push(branch.clone());
    }
    for schema in &mut schemas {
        for (exclusive, bound) in [
            ("exclusiveMinimum", "minimum"),
            ("exclusiveMaximum", "maximum"),
        ] {
            if schema.get(exclusive) == Some(&Value::Bool(true)) {
                let value = schema.remove(bound).unwrap_or(Value::Null);
                schema.insert(exclusive.into(), value);
            }
        }
    }
    let mut tags = Vec::new();
    if schemas.iter().any(|schema| {
        schema.get("type").is_some_and(|kind| {
            kind == "integer"
                || kind
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "integer"))
        })
    }) {
        tags.push("@integer".into());
    }
    for keyword in [
        "minimum",
        "exclusiveMinimum",
        "maximum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "format",
        "minItems",
        "maxItems",
    ] {
        let lower = matches!(
            keyword,
            "minimum" | "exclusiveMinimum" | "minLength" | "minItems"
        );
        let upper = matches!(
            keyword,
            "maximum" | "exclusiveMaximum" | "maxLength" | "maxItems"
        );
        let values = schemas
            .iter()
            .filter_map(|schema| schema.get(keyword))
            .collect::<Vec<_>>();
        if lower || upper {
            if let Some(value) =
                values
                    .into_iter()
                    .filter(|value| value.is_number())
                    .reduce(|left, right| {
                        if (right.as_f64().unwrap_or(0.0) > left.as_f64().unwrap_or(0.0)) == lower {
                            right
                        } else {
                            left
                        }
                    })
            {
                tags.push(format!("@{keyword} {value}"));
            }
        } else {
            let mut seen = HashSet::new();
            for value in values {
                let text = match value {
                    Value::String(text) => text.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => continue,
                };
                if seen.insert(text.clone()) {
                    tags.push(format!("@{keyword} {text}"));
                }
            }
        }
    }
    if schemas
        .iter()
        .any(|schema| schema.get("uniqueItems") == Some(&Value::Bool(true)))
    {
        tags.push("@uniqueItems".into());
    }
    if let Some(value) = object.get("default").filter(|value| !value.is_null()) {
        tags.push(format!("@default {value}"));
    }
    if object.get("deprecated") == Some(&Value::Bool(true)) {
        tags.push("@deprecated".into());
    }
    tags.into_iter()
        .filter(|tag| character_count(tag) <= 80)
        .map(|tag| {
            tag.chars()
                .map(|c| match c {
                    '\r' | '\n' | '\u{2028}' | '\u{2029}' => format!("\\u{:04x}", c as u32),
                    _ => c.to_string(),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_types_and_schema_constraints_like_pi() {
        let schema = json!({"type":"object","properties":{
            "limit":{"anyOf":[{"type":"integer","minimum":10,"maximum":50,"format":"int32"},{"type":"null"}],"minimum":0,"maximum":20,"format":"port"},
            "id":{"type":"string","description":"Identifier.","maxLength":8},
            "tags":{"type":"array","items":{"type":"string"},"maxItems":5,"uniqueItems":true,"deprecated":true}
        },"required":["id"]});
        assert_eq!(
            schema_to_type(&schema, None),
            "{\n  // Identifier.\n  // @maxLength 8\n  id: string;\n  // @integer @minimum 10 @maximum 20 @format port @format int32\n  limit?: number | null;\n  // @maxItems 5 @uniqueItems @deprecated\n  tags?: Array<string>;\n}"
        );
        assert_eq!(
            schema_to_type(
                &json!({"allOf":[{"anyOf":[{"type":"string"},{"type":"number"}]},{"const":1}]}),
                None
            ),
            "(string | number) & 1"
        );
        assert_eq!(
            schema_to_type(&json!({"anyOf":[{"type":"string"},{}]}), None),
            "unknown"
        );
    }

    #[test]
    fn resolves_escaped_refs_and_bounds_recursive_and_large_schemas() {
        let schema = json!({"type":"object","properties":{"node":{"$ref":"#/$defs/a~1b%20c"},"remote":{"$ref":"https://example.com"}},"$defs":{"a/b c":{"type":"object","properties":{"child":{"$ref":"#/$defs/a~1b%20c"}}}}});
        assert_eq!(
            schema_to_type(&schema, None),
            "{ node?: { child?: unknown; }; remote?: unknown; }"
        );
        assert!(resolve_schema_ref("#/$defs/%E0%A4%A", &schema).is_none());
        let constrained =
            json!({"type":"object","properties":{"n":{"type":"integer","minimum":1}}});
        assert_eq!(schema_to_type(&constrained, Some(20)), "{ n?: number; }");
        assert_eq!(schema_to_type(&constrained, Some(10)), "unknown");
    }

    #[test]
    fn output_summary_preserves_union_fields_and_nested_constants() {
        let schema = json!({"anyOf":[{"type":"string"},{"type":"object","properties":{"type":{"const":"image"},"data":{"type":"string"},"mimeType":{"type":"string"}},"required":["type","data","mimeType"]}]});
        assert!(
            describe_output("read", Some(&schema))
                .contains("string | { data: string; mimeType: string; type: \"image\"; }")
        );
        let schema = json!({"type":"object","properties":{"structuredContent":{"type":"object","properties":{"action":{"const":"start"},"id":{"type":"string"}},"required":["action"]}},"required":[]});
        assert_eq!(
            describe_output("start", Some(&schema)),
            "`{ structuredContent?: { action: \"start\", id? } }`"
        );
    }
}
