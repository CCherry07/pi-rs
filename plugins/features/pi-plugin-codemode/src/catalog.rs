use std::collections::HashMap;

use pi_plugin::ToolInfo;
use pi_plugin_tool_search::{Bm25Ranker, ToolSearchDocument, create_tool_search_document};
use serde_json::{Value, json};

use crate::declarations::{identifier, sample_with_name};

pub(crate) struct Catalog {
    tools: Vec<ToolInfo>,
    aliases: HashMap<String, usize>,
    documents: Vec<ToolSearchDocument>,
    pub declarations: String,
}

impl Catalog {
    pub fn new(tools: &[ToolInfo]) -> Result<Self, String> {
        if tools.len() > 4096 {
            return Err("codemode catalog exceeds 4096 tools".into());
        }
        let mut aliases = HashMap::new();
        // Preserve every exact name. A normalized alias is only installed when unambiguous.
        for (index, tool) in tools.iter().enumerate() {
            aliases.insert(tool.spec.name.clone(), index);
        }
        let mut normalized = HashMap::<String, Vec<usize>>::new();
        for (index, tool) in tools.iter().enumerate() {
            normalized
                .entry(identifier(&tool.spec.name))
                .or_default()
                .push(index);
        }
        for (name, matches) in normalized {
            if matches.len() == 1 {
                aliases.entry(name).or_insert(matches[0]);
            }
        }
        let entries = tools
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                let candidate = identifier(&tool.spec.name);
                let js_name = if aliases.get(&candidate) == Some(&index) {
                    candidate
                } else {
                    tool.spec.name.clone()
                };
                json!({"name": tool.spec.name, "jsName": js_name, "description": sample_with_name(&tool.spec, &js_name)})
            })
            .collect::<Vec<_>>();
        let declarations = serde_json::to_string(&entries).map_err(|e| e.to_string())?;
        if declarations.len() > 8 * 1024 * 1024 {
            return Err("codemode catalog exceeds the 8 MiB limit".into());
        }
        Ok(Self {
            tools: tools.to_vec(),
            aliases,
            documents: tools
                .iter()
                .map(|tool| {
                    create_tool_search_document(&tool.spec, tool.discovery.namespace.as_ref())
                })
                .collect(),
            declarations,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.spec.name == name)
    }

    pub fn query(&self, kind: &str, args: &str) -> Result<String, String> {
        if args.len() > 16 * 1024 {
            return Err("Discovery arguments exceed 16 KiB".into());
        }
        let args: Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let name = args[0]
            .as_str()
            .ok_or("Discovery expects a string as its first argument")?;
        let value = match kind {
            "describeTool" => self
                .aliases
                .get(name)
                .map(|index| Value::String(self.sample(*index))),
            "describeNamespace" => {
                let matching = self
                    .tools
                    .iter()
                    .enumerate()
                    .filter(|(_, tool)| {
                        tool.discovery
                            .namespace
                            .as_ref()
                            .is_some_and(|ns| ns.name == name || identifier(&ns.name) == name)
                    })
                    .collect::<Vec<_>>();
                matching.first().and_then(|(_, tool)| tool.discovery.namespace.as_ref()).map(|namespace| json!({
                    "name": namespace.name,
                    "description": namespace.description,
                    "instructions": namespace.instructions,
                    "tools": matching.iter().map(|(index, _)| self.js_name(*index)).collect::<Vec<_>>()
                }))
            }
            "searchTools" => {
                let limit = match args[1].get("limit") {
                    None | Some(Value::Null) => 8,
                    Some(value) => value
                        .as_u64()
                        .filter(|v| *v > 0)
                        .ok_or("searchTools limit must be a positive integer")?
                        .min(4096) as usize,
                };
                let namespace = match args[1].get("namespace") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_str()
                            .ok_or("searchTools namespace must be a string")?,
                    ),
                };
                let documents =
                    self.documents
                        .iter()
                        .zip(&self.tools)
                        .filter(|(_, tool)| {
                            namespace.is_none_or(|name| {
                                tool.discovery.namespace.as_ref().is_some_and(|ns| {
                                    ns.name == name || identifier(&ns.name) == name
                                })
                            })
                        })
                        .map(|(document, _)| document.clone())
                        .collect::<Vec<_>>();
                let found = Bm25Ranker::default().rank(name, &documents, limit);
                Some(Value::Array(
                    found
                        .iter()
                        .filter_map(|found| self.aliases.get(&found.name))
                        .map(|index| {
                            let name = self.js_name(*index);
                            json!({"name": name, "description": self.sample(*index)})
                        })
                        .collect(),
                ))
            }
            _ => return Err("Unknown discovery helper".into()),
        };
        // Undefined uses an empty envelope; JSON null is a valid stored value.
        serde_json::to_string(&json!({"value": value, "found": value.is_some()}))
            .map_err(|e| e.to_string())
    }

    fn js_name(&self, index: usize) -> String {
        let tool = &self.tools[index];
        let candidate = identifier(&tool.spec.name);
        if self.aliases.get(&candidate) == Some(&index) {
            candidate
        } else {
            tool.spec.name.clone()
        }
    }

    fn sample(&self, index: usize) -> String {
        sample_with_name(&self.tools[index].spec, &self.js_name(index))
    }
}
