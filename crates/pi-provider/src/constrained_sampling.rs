//! Provider-neutral tool sampling contracts; callers supply compatibility capabilities.

use std::collections::{BTreeMap, HashSet};

use pi_core::{ConstrainedSampling, GrammarFormat, ToolSchemaStrict, ToolSpec};
use serde_json::{Value, json};

pub fn resolve_json_schema_sampling(
    tool: &ToolSpec,
    supports: bool,
) -> Result<Option<Value>, String> {
    resolve_json_schema_sampling_with(tool, supports, |_, _| false)
}

pub fn resolve_json_schema_sampling_with(
    tool: &ToolSpec,
    supports: bool,
    unsupported: impl Fn(&str, &Value) -> bool,
) -> Result<Option<Value>, String> {
    let Some(ConstrainedSampling::JsonSchema { strict }) = &tool.constrained_sampling else {
        return Ok(None);
    };
    let converted = if supports {
        make_strict_json_schema(&tool.parameters, &unsupported)
    } else {
        Err("strict tools are unsupported".into())
    };
    match converted {
        Ok(schema) => Ok(Some(schema)),
        Err(reason) if *strict == ToolSchemaStrict::Require => Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but {reason}.",
            tool.name
        )),
        Err(_) => Ok(None),
    }
}

fn make_strict_json_schema(
    schema: &Value,
    unsupported: &impl Fn(&str, &Value) -> bool,
) -> Result<Value, String> {
    let mut schema = schema.clone();
    make_node_strict(&mut schema, unsupported)?;
    if schema["type"] != "object" {
        return Err("root schema must have type object".into());
    }
    Ok(schema)
}

fn make_node_strict(
    schema: &mut Value,
    unsupported: &impl Fn(&str, &Value) -> bool,
) -> Result<(), String> {
    let object = schema
        .as_object_mut()
        .ok_or("boolean schemas are unsupported")?;
    for key in [
        "$ref",
        "$defs",
        "definitions",
        "allOf",
        "oneOf",
        "patternProperties",
        "dependentSchemas",
        "dependencies",
        "unevaluatedProperties",
        "propertyNames",
        "contains",
        "prefixItems",
        "not",
        "if",
        "then",
        "else",
    ] {
        if object.contains_key(key) {
            return Err(format!("{key} schemas are unsupported"));
        }
    }
    for (key, value) in object.iter() {
        if unsupported(key, value) {
            return Err(format!("{key}: {value} is unsupported"));
        }
    }
    if !["type", "anyOf", "enum"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return Err("schemas without a type are unsupported".into());
    }
    if let Some(variants) = object.get_mut("anyOf") {
        let variants = variants
            .as_array_mut()
            .filter(|variants| !variants.is_empty())
            .ok_or("anyOf must contain at least one schema")?;
        for variant in variants {
            let structured = variant.get("type").is_some_and(|kind| {
                kind == "object"
                    || kind == "array"
                    || kind.as_array().is_some_and(|kinds| {
                        kinds.iter().any(|kind| kind == "object" || kind == "array")
                    })
            }) || variant.get("properties").is_some()
                || variant.get("items").is_some();
            if structured {
                return Err("object and array unions are unsupported".into());
            }
            make_node_strict(variant, unsupported)?;
        }
    }
    if let Some(items) = object.get_mut("items") {
        if items.is_array() {
            return Err("tuple schemas are unsupported".into());
        }
        make_node_strict(items, unsupported)?;
    }
    let is_object = object.get("type") == Some(&json!("object"));
    if object.contains_key("properties") && !is_object {
        return Err("properties require type object".into());
    }
    if !is_object {
        return Ok(());
    }
    if object
        .get("additionalProperties")
        .is_some_and(|additional| additional != &Value::Bool(false))
    {
        return Err("schema-valued or true additionalProperties is unsupported".into());
    }
    let required = match object.get("required") {
        None => HashSet::new(),
        Some(Value::Array(required)) if required.iter().all(Value::is_string) => required
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => return Err("object required must be a string array".into()),
    };
    let property_names = if let Some(properties) = object.get_mut("properties") {
        let properties = properties
            .as_object_mut()
            .ok_or("object properties must be a schema map")?;
        if required.iter().any(|name| !properties.contains_key(name)) {
            return Err("required contains an unknown property".into());
        }
        for (name, property) in properties.iter_mut() {
            make_node_strict(property, unsupported)?;
            if !required.contains(name) && !allows_null(property) {
                *property = json!({"anyOf":[std::mem::take(property), {"type":"null"}]});
            }
        }
        properties.keys().cloned().collect::<Vec<_>>()
    } else {
        if !required.is_empty() {
            return Err("required contains an unknown property".into());
        }
        Vec::new()
    };
    object.insert("required".into(), json!(property_names));
    object.insert("additionalProperties".into(), Value::Bool(false));
    Ok(())
}

fn allows_null(schema: &Value) -> bool {
    schema.get("type").is_some_and(|kind| {
        kind == "null"
            || kind
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "null"))
    }) || schema.get("const") == Some(&Value::Null)
        || schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.contains(&Value::Null))
        || schema
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|variants| variants.iter().any(allows_null))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarSampling {
    pub format: &'static str,
    pub definition: String,
    pub input_property: String,
}

pub fn resolve_grammar_sampling(
    tool: &ToolSpec,
    supports: bool,
) -> Result<Option<GrammarSampling>, String> {
    let Some(ConstrainedSampling::Grammar { variants }) = &tool.constrained_sampling else {
        return Ok(None);
    };
    if !supports {
        return Ok(None);
    }
    let fail = |reason: &str| {
        format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {reason}.",
            tool.name
        )
    };
    let (format, definition) = [
        (GrammarFormat::OpenAiLark, "lark"),
        (GrammarFormat::OpenAiRegex, "regex"),
    ]
    .into_iter()
    .find_map(|(kind, format)| {
        variants
            .get(&kind)
            .filter(|definition| !definition.trim().is_empty())
            .map(|definition| (format, definition))
    })
    .ok_or_else(|| fail("no supported grammar variant was provided"))?;
    let schema = &tool.parameters;
    if schema["type"] != "object" {
        return Err(fail(
            "grammar constrained sampling requires an object parameter schema",
        ));
    }
    let required = schema["required"]
        .as_array()
        .filter(|required| required.len() == 1)
        .and_then(|required| required[0].as_str())
        .ok_or_else(|| {
            fail("grammar constrained sampling requires exactly one required string property")
        })?;
    let property = schema["properties"].get(required).ok_or_else(|| {
        fail(&format!(
            "grammar constrained sampling requires a properties entry for {required}"
        ))
    })?;
    if property["type"] != "string" {
        return Err(fail(&format!(
            "grammar constrained sampling property {required} must have type string"
        )));
    }
    Ok(Some(GrammarSampling {
        format,
        definition: definition.clone(),
        input_property: required.into(),
    }))
}

pub fn grammar_tool_input_properties(
    tools: &[ToolSpec],
    supports: bool,
) -> Result<BTreeMap<String, String>, String> {
    let mut properties = BTreeMap::new();
    for tool in tools {
        if let Some(grammar) = resolve_grammar_sampling(tool, supports)? {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}

#[derive(Debug, Default)]
pub struct GrammarInputBuffer {
    input: String,
    started: bool,
    closed: bool,
}

impl GrammarInputBuffer {
    pub fn append(&mut self, property: &str, delta: &str) -> Result<Option<String>, String> {
        if self.closed {
            return Err(format!(
                "grammar tool input for property \"{property}\" changed after it was closed"
            ));
        }
        if delta.is_empty() {
            return Ok(None);
        }
        self.input.push_str(delta);
        Ok(Some(format!("{}{}", self.start(property), escaped(delta))))
    }

    pub fn close(
        &mut self,
        property: &str,
        final_input: Option<&str>,
    ) -> Result<Option<String>, String> {
        if self.closed {
            return if final_input.is_none_or(|value| value == self.input) {
                Ok(None)
            } else {
                Err(format!(
                    "grammar tool input for property \"{property}\" changed after it was closed"
                ))
            };
        }
        let rest = match final_input {
            Some(value) => value.strip_prefix(&self.input).ok_or_else(|| {
                format!("grammar tool input for property \"{property}\" changed non-monotonically")
            })?,
            None => "",
        };
        let tail = escaped(rest);
        if let Some(value) = final_input {
            self.input = value.into();
        }
        self.closed = true;
        Ok(Some(format!("{}{tail}\"}}", self.start(property))))
    }

    fn start(&mut self, property: &str) -> String {
        if self.started {
            String::new()
        } else {
            self.started = true;
            format!("{{{}:\"", json!(property))
        }
    }
}

fn escaped(value: &str) -> String {
    let value = json!(value).to_string();
    value[1..value.len() - 1].into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_preserves_original_optional_meaning_and_rejects_unsafe_shapes() {
        let original = json!({"type":"object","properties":{"required":{"type":"string"},"optional":{"type":"integer"},"nested":{"type":"object","properties":{"value":{"type":"boolean"}}}},"required":["required"]});
        let strict = make_strict_json_schema(&original, &|_, _| false).unwrap();
        let mut required = strict["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect::<Vec<_>>();
        required.sort_unstable();
        assert_eq!(required, ["nested", "optional", "required"]);
        assert_eq!(strict["additionalProperties"], false);
        assert_eq!(
            strict["properties"]["optional"]["anyOf"][1],
            json!({"type":"null"})
        );
        assert_eq!(
            strict["properties"]["nested"]["anyOf"][0]["additionalProperties"],
            false
        );
        assert!(original["properties"]["optional"].get("anyOf").is_none());
        for invalid in [
            json!({"type":"object","additionalProperties":true}),
            json!({"type":"object","$defs":{}}),
            json!({"type":"object","properties":{"value":true}}),
        ] {
            assert!(make_strict_json_schema(&invalid, &|_, _| false).is_err());
        }
    }

    #[test]
    fn grammar_deltas_preserve_quotes_newlines_and_reject_nonmonotonic_final_input() {
        let mut buffer = GrammarInputBuffer::default();
        let mut text = buffer.append("source", "text(\"").unwrap().unwrap();
        text.push_str(&buffer.append("source", "你好\");\n").unwrap().unwrap());
        text.push_str(
            &buffer
                .close("source", Some("text(\"你好\");\nreturn 1"))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"source":"text(\"你好\");\nreturn 1"})
        );
        assert!(buffer.close("source", Some("changed")).is_err());
        assert_eq!(buffer.close("source", None).unwrap(), None);
    }
}
