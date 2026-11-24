use pi_plugin::{ToolInfo, ToolNamespace};
use pi_plugin_tool_search::{Bm25Ranker, ToolSearchDocument, create_tool_search_document};
use serde_json::{Value, json};

use crate::declarations::{identifier, sample};

pub(crate) struct Catalog {
    tools: Vec<ToolInfo>,
    documents: Vec<ToolSearchDocument>,
    pub declarations: String,
}

impl Catalog {
    pub fn new(tools: &[ToolInfo]) -> Result<Self, String> {
        // The sandbox installs the first tool with each normalized identifier.
        // Discovery deliberately retains every original entry, even collisions.
        let entries = tools
            .iter()
            .map(|tool| {
                json!({"name": tool.spec.name, "jsName": identifier(&tool.spec.name), "description": sample(&tool.spec)})
            })
            .collect::<Vec<_>>();
        Ok(Self {
            tools: tools.to_vec(),
            documents: tools
                .iter()
                .map(|tool| {
                    create_tool_search_document(&tool.spec, tool.discovery.namespace.as_ref())
                })
                .collect(),
            declarations: serde_json::to_string(&entries).map_err(|e| e.to_string())?,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.spec.name == name)
    }

    pub fn query(&self, kind: &str, args: &str) -> Result<String, String> {
        let args: Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let name = args[0].as_str().ok_or(match kind {
            "searchTools" => "searchTools() expects a query string",
            "describeTool" => "describeTool() expects a tool name",
            "describeNamespace" => "describeNamespace() expects a namespace name",
            _ => "Unknown discovery helper",
        })?;
        let value = match kind {
            "describeTool" => self
                .tools
                .iter()
                .find(|tool| tool.spec.name == name || identifier(&tool.spec.name) == name)
                .map(|tool| Value::String(sample(&tool.spec))),
            "describeNamespace" => {
                let matching = self
                    .tools
                    .iter()
                    .filter(|tool| {
                        tool.discovery
                            .namespace
                            .as_ref()
                            .is_some_and(|ns| namespace_matches(ns, name))
                    })
                    .collect::<Vec<_>>();
                matching
                    .first()
                    .and_then(|tool| tool.discovery.namespace.as_ref())
                    .map(|namespace| {
                        let mut value = json!({"name": namespace.name});
                        if let Some(description) = namespace
                            .description
                            .as_ref()
                            .filter(|value| !value.is_empty())
                        {
                            value["description"] = json!(description);
                        }
                        if let Some(instructions) = namespace
                            .instructions
                            .as_ref()
                            .filter(|value| !value.is_empty())
                        {
                            value["instructions"] = json!(instructions);
                        }
                        value["tools"] = json!(
                            matching
                                .iter()
                                .map(|tool| identifier(&tool.spec.name))
                                .collect::<Vec<_>>()
                        );
                        value
                    })
            }
            "searchTools" => {
                let limit = match args[1].get("limit") {
                    None | Some(Value::Null) => 8,
                    Some(value) => {
                        let limit = value
                            .as_f64()
                            .filter(|v| v.is_finite() && *v > 0.0 && v.fract() == 0.0)
                            .ok_or("searchTools() limit must be a positive integer")?;
                        // JavaScript Number.isInteger also accepts values larger than u64.
                        limit.min(self.tools.len() as f64) as usize
                    }
                };
                let namespace = match args[1].get("namespace") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_str()
                            .ok_or("searchTools() namespace must be a string")?,
                    ),
                }
                .filter(|name| !name.is_empty());
                let documents = self
                    .documents
                    .iter()
                    .zip(&self.tools)
                    .filter(|(_, tool)| {
                        namespace.is_none_or(|name| {
                            tool.discovery
                                .namespace
                                .as_ref()
                                .is_some_and(|ns| namespace_matches(ns, name))
                        })
                    })
                    .map(|(document, _)| document.clone())
                    .collect::<Vec<_>>();
                let found = Bm25Ranker::default().rank(name, &documents, limit);
                Some(Value::Array(found.iter().filter_map(|found| {
                    self.tools.iter().find(|tool| tool.spec.name == found.name)
                }).map(|tool| json!({"name": identifier(&tool.spec.name), "description": sample(&tool.spec)})).collect()))
            }
            _ => return Err("Unknown discovery helper".into()),
        };
        serde_json::to_string(&json!({"value": value, "found": value.is_some()}))
            .map_err(|e| e.to_string())
    }
}

fn namespace_matches(namespace: &ToolNamespace, query: &str) -> bool {
    let query_id = identifier(query);
    std::iter::once(&namespace.name)
        .chain(&namespace.aliases)
        .any(|name| name == query || identifier(name) == query_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ToolExecutionMode, ToolSpec};
    use pi_plugin::ToolDiscovery;

    fn tool(name: &str, description: &str) -> ToolInfo {
        ToolInfo {
            spec: ToolSpec {
                name: name.into(),
                label: name.into(),
                description: description.into(),
                parameters: json!({}),
                output_schema: None,
                constrained_sampling: None,
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: vec![],
            },
            discovery: ToolDiscovery::default(),
        }
    }

    fn query(catalog: &Catalog, kind: &str, args: Value) -> Value {
        serde_json::from_str::<Value>(&catalog.query(kind, &args.to_string()).unwrap()).unwrap()["value"].clone()
    }

    #[test]
    fn normalized_collisions_use_first_match_but_search_retains_entries() {
        let tools = [
            tool("foo-bar", "first unique"),
            tool("foo_bar", "second unique"),
        ];
        let catalog = Catalog::new(&tools).unwrap();
        assert!(
            query(&catalog, "describeTool", json!(["foo_bar"]))
                .as_str()
                .unwrap()
                .starts_with("first unique")
        );
        let declarations: Value = serde_json::from_str(&catalog.declarations).unwrap();
        assert_eq!(declarations[0]["jsName"], "foo_bar");
        assert_eq!(declarations[1]["jsName"], "foo_bar");
        let found = query(&catalog, "searchTools", json!(["unique"]));
        assert_eq!(found.as_array().unwrap().len(), 2);
        assert!(
            found[1]["description"]
                .as_str()
                .unwrap()
                .starts_with("second unique")
        );
    }

    #[test]
    fn namespace_aliases_normalize_both_sides_and_omit_empty_metadata() {
        let mut tool = tool("alpha", "unique");
        tool.discovery.namespace = Some(ToolNamespace {
            name: "mcp__dev-radius".into(),
            aliases: vec!["dev-radius".into()],
            description: Some("".into()),
            instructions: None,
        });
        let catalog = Catalog::new(&[tool]).unwrap();
        for alias in ["dev_radius", "dev radius", "mcp__dev radius"] {
            assert_eq!(
                query(&catalog, "describeNamespace", json!([alias])),
                json!({"name":"mcp__dev-radius","tools":["alpha"]})
            );
            assert_eq!(
                query(
                    &catalog,
                    "searchTools",
                    json!(["unique",{"namespace":alias}])
                )
                .as_array()
                .unwrap()
                .len(),
                1
            );
        }
        assert_eq!(
            query(
                &catalog,
                "searchTools",
                json!(["unique",{"namespace":"", "limit":1e20}])
            )
            .as_array()
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn discovery_errors_match_each_helper() {
        let catalog = Catalog::new(&[]).unwrap();
        for (helper, expected) in [
            ("describeTool", "describeTool() expects a tool name"),
            (
                "describeNamespace",
                "describeNamespace() expects a namespace name",
            ),
            ("searchTools", "searchTools() expects a query string"),
        ] {
            assert_eq!(catalog.query(helper, "[]").unwrap_err(), expected);
        }
        assert_eq!(
            catalog
                .query("searchTools", r#"["q",{"limit":1.5}]"#)
                .unwrap_err(),
            "searchTools() limit must be a positive integer"
        );
    }
}
