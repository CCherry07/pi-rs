//! Generation-local resource tools shared by every visible resource-capable MCP server.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;
use pi_core::{ToolCallId, ToolExecutionMode, ToolResult, ToolSpec};
use pi_plugin::{Tool, ToolContext, ToolError, ToolUpdateSink};
use rmcp::model::{
    ClientRequest, ListResourceTemplatesRequest, ListResourcesRequest, PaginatedRequestParams,
    ReadResourceRequest, ReadResourceRequestParams, ServerResult,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::client::ConnectedServer;
use crate::result::{project_resource_listing, project_resource_read};

pub(crate) const RESOURCE_TOOL_NAMES: [&str; 3] = [
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
];

const MAX_LIST_PAGES: usize = 1_000;

#[derive(Clone, Copy)]
enum ResourceOperation {
    List,
    Templates,
    Read,
}

impl ResourceOperation {
    fn name(self) -> &'static str {
        match self {
            Self::List => RESOURCE_TOOL_NAMES[0],
            Self::Templates => RESOURCE_TOOL_NAMES[1],
            Self::Read => RESOURCE_TOOL_NAMES[2],
        }
    }

    fn output_schema(self) -> Value {
        if matches!(self, Self::Read) {
            return json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "uri": {"type": "string"},
                    "contents": {"type": "array", "items": {"anyOf": [
                        {"type": "object", "properties": {
                            "uri": {"type": "string"}, "mimeType": {"type": "string"}, "text": {"type": "string"}
                        }, "required": ["uri", "text"]},
                        {"type": "object", "properties": {
                            "uri": {"type": "string"}, "mimeType": {"type": "string"},
                            "blob": {"type": "string", "description": "base64"}
                        }, "required": ["uri", "blob"]}
                    ]}}
                },
                "required": ["server", "uri", "contents"]
            });
        }
        let item = if matches!(self, Self::List) {
            json!({
                "type": "object", "properties": {
                    "server": {"type": "string"}, "uri": {"type": "string"}, "name": {"type": "string"},
                    "title": {"type": "string"}, "description": {"type": "string"},
                    "mimeType": {"type": "string"}, "size": {"type": "number"}
                }, "required": ["server", "uri", "name"]
            })
        } else {
            json!({
                "type": "object", "properties": {
                    "server": {"type": "string"},
                    "uriTemplate": {"type": "string", "description": "RFC 6570 URI template"},
                    "name": {"type": "string"}, "title": {"type": "string"},
                    "description": {"type": "string"}, "mimeType": {"type": "string"}
                }, "required": ["server", "uriTemplate", "name"]
            })
        };
        json!({
            "type": "object", "properties": {
                "server": {"type": "string"},
                (self.key()): {"type": "array", "items": item},
                "nextCursor": {"type": "string"},
                "errors": {
                    "type": "array", "description": "Servers that could not be listed",
                    "items": {"type": "object", "properties": {
                        "server": {"type": "string"}, "error": {"type": "string"}
                    }, "required": ["server", "error"]}
                }
            },
            "required": [self.key()]
        })
    }

    fn key(self) -> &'static str {
        match self {
            Self::List => "resources",
            Self::Templates => "resourceTemplates",
            Self::Read => "contents",
        }
    }
}

struct ResourceTool {
    operation: ResourceOperation,
    servers: Arc<Vec<Arc<ConnectedServer>>>,
}

/// The caller supplies only servers allowed by the generation's resource exposure policy.
pub(crate) fn create_resource_tools(mut servers: Vec<Arc<ConnectedServer>>) -> Vec<Arc<dyn Tool>> {
    if servers.is_empty() {
        return Vec::new();
    }
    // Unlike JavaScript's host-locale ordering, this is stable across embedding environments.
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    let servers = Arc::new(servers);
    [
        ResourceOperation::List,
        ResourceOperation::Templates,
        ResourceOperation::Read,
    ]
    .into_iter()
    .map(|operation| {
        Arc::new(ResourceTool {
            operation,
            servers: Arc::clone(&servers),
        }) as Arc<dyn Tool>
    })
    .collect()
}

#[async_trait]
impl Tool for ResourceTool {
    fn structured_output(&self, result: &ToolResult) -> Option<Value> {
        result.structured_content.clone()
    }

    fn spec(&self) -> ToolSpec {
        let name = self.operation.name();
        let description = match self.operation {
            ResourceOperation::List => {
                "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible."
            }
            ResourceOperation::Templates => {
                "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible."
            }
            ResourceOperation::Read => {
                "Read a specific resource from an MCP server given the server name and resource URI."
            }
        };
        let parameters = if matches!(self.operation, ResourceOperation::Read) {
            json!({
                "type":"object",
                "properties": {
                    "server": {"type":"string", "description":"MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources."},
                    "uri": {"type":"string", "description":"Resource URI to read. Must be one of the URIs returned by list_mcp_resources."}
                },
                "required":["server", "uri"],
                "additionalProperties":false
            })
        } else {
            json!({
                "type":"object",
                "properties": {
                    "server": {"type":"string", "description":"MCP server name. Omit to list every server with resources."},
                    "cursor": {"type":"string", "description":"Opaque cursor from a previous call with the same server; omit for the first page."}
                },
                "additionalProperties":false
            })
        };
        ToolSpec {
            output_schema: Some(self.operation.output_schema()),
            constrained_sampling: None,
            name: name.into(),
            label: name.into(),
            description: description.into(),
            parameters,
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        if !input.is_object() {
            return Err(ToolError::InvalidArguments(
                "arguments must be a JSON object".into(),
            ));
        }
        if matches!(self.operation, ResourceOperation::Read) {
            let server = string_argument(&input, "server")?
                .ok_or_else(|| ToolError::InvalidArguments("server must be provided".into()))?;
            let uri = string_argument(&input, "uri")?
                .ok_or_else(|| ToolError::InvalidArguments("uri must be provided".into()))?;
            let server = self.find_server(server)?;
            let response = server
                .request(
                    &context,
                    ClientRequest::ReadResourceRequest(ReadResourceRequest::new(
                        ReadResourceRequestParams::new(uri),
                    )),
                    self.operation.name(),
                    &updates,
                )
                .await
                .map_err(|error| error.into_tool_error(&server.name))?;
            let ServerResult::ReadResourceResult(result) = response else {
                return Err(unexpected_response(&server.name));
            };
            // Like Pi, resource reads delegate URI validation to the server. Listing is not an ACL.
            return Ok(project_resource_read(&server.name, uri, result));
        }
        let server = string_argument(&input, "server")?;
        let cursor = string_argument(&input, "cursor")?;
        let payload = if let Some(name) = server {
            let server = self.find_server(name)?;
            let page = resource_page(server, self.operation, cursor, &context, &updates).await?;
            let mut payload = json!({"server": server.name, (self.operation.key()): page.items});
            if let Some(cursor) = page.next_cursor {
                payload["nextCursor"] = json!(cursor);
            }
            payload
        } else {
            if cursor.is_some() {
                return Err(ToolError::InvalidArguments(
                    "cursor can only be used when a server is specified".into(),
                ));
            }
            let results = join_all(
                self.servers
                    .iter()
                    .map(|server| all_resources(server, self.operation, &context, &updates)),
            )
            .await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            let mut items = Vec::new();
            let mut errors = Vec::new();
            for (server, result) in self.servers.iter().zip(results) {
                match result {
                    Ok(resources) => items.extend(resources),
                    Err(ToolError::Aborted) => return Err(ToolError::Aborted),
                    Err(error) => {
                        errors.push(json!({"server":server.name,"error":error.to_string()}))
                    }
                }
            }
            let mut payload = json!({(self.operation.key()): items});
            if !errors.is_empty() {
                payload["errors"] = json!(errors);
            }
            payload
        };
        Ok(project_resource_listing(payload))
    }
}

impl ResourceTool {
    fn find_server(&self, name: &str) -> Result<&ConnectedServer, ToolError> {
        self.servers
            .iter()
            .find(|server| server.name == name)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                let available = self
                    .servers
                    .iter()
                    .map(|server| server.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                ToolError::Execution(format!(
                    "MCP server \"{name}\" has no resources{}",
                    if available.is_empty() {
                        String::new()
                    } else {
                        format!(". Servers with resources: {available}")
                    }
                ))
            })
    }
}

struct ResourcePage {
    items: Vec<Value>,
    next_cursor: Option<String>,
    unsupported_templates: bool,
}

async fn resource_page(
    server: &ConnectedServer,
    operation: ResourceOperation,
    cursor: Option<&str>,
    context: &ToolContext,
    updates: &ToolUpdateSink,
) -> Result<ResourcePage, ToolError> {
    let params = PaginatedRequestParams::default().with_cursor(cursor.map(str::to_owned));
    let request = match operation {
        ResourceOperation::List => {
            ClientRequest::ListResourcesRequest(ListResourcesRequest::with_param(params))
        }
        ResourceOperation::Templates => ClientRequest::ListResourceTemplatesRequest(
            ListResourceTemplatesRequest::with_param(params),
        ),
        ResourceOperation::Read => unreachable!("read does not paginate"),
    };
    let response = match server
        .request(context, request, operation.name(), updates)
        .await
    {
        Ok(response) => response,
        Err(error)
            if matches!(operation, ResourceOperation::Templates) && error.is_method_not_found() =>
        {
            return Ok(ResourcePage {
                items: Vec::new(),
                next_cursor: None,
                unsupported_templates: true,
            });
        }
        Err(error) => return Err(error.into_tool_error(&server.name)),
    };
    let (items, next_cursor) = match (operation, response) {
        (ResourceOperation::List, ServerResult::ListResourcesResult(result)) => (
            listed_items(&server.name, result.resources)?,
            result.next_cursor,
        ),
        (ResourceOperation::Templates, ServerResult::ListResourceTemplatesResult(result)) => (
            listed_items(&server.name, result.resource_templates)?,
            result.next_cursor,
        ),
        _ => return Err(unexpected_response(&server.name)),
    };
    Ok(ResourcePage {
        items,
        next_cursor: next_cursor.filter(|cursor| !cursor.is_empty()),
        unsupported_templates: false,
    })
}

async fn all_resources(
    server: &ConnectedServer,
    operation: ResourceOperation,
    context: &ToolContext,
    updates: &ToolUpdateSink,
) -> Result<Vec<Value>, ToolError> {
    let mut items = Vec::new();
    let mut cursor = None;
    let mut seen = HashSet::new();
    for _ in 0..MAX_LIST_PAGES {
        let page = resource_page(server, operation, cursor.as_deref(), context, updates).await?;
        if page.unsupported_templates {
            return Ok(Vec::new());
        }
        items.extend(page.items);
        let Some(next) = page.next_cursor else {
            return Ok(items);
        };
        if !seen.insert(next.clone()) {
            return Err(ToolError::Execution(format!(
                "MCP resource listing from server {} returned a duplicate cursor",
                server.name
            )));
        }
        cursor = Some(next);
    }
    Err(ToolError::Execution(format!(
        "MCP resource listing from server {} exceeded {MAX_LIST_PAGES} pages",
        server.name
    )))
}

fn unexpected_response(server: &str) -> ToolError {
    ToolError::Execution(format!(
        "Unexpected MCP resource response from server {server}"
    ))
}

fn string_argument<'a>(input: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim()).filter(|value| !value.is_empty())),
        _ => Err(ToolError::InvalidArguments(format!(
            "{key} must be a string"
        ))),
    }
}

fn listed_items<T: Serialize>(server: &str, items: Vec<T>) -> Result<Vec<Value>, ToolError> {
    items
        .into_iter()
        .filter_map(|item| {
            let value = match serde_json::to_value(item) {
                Ok(value) => value,
                Err(_) => return Some(Err(unexpected_response(server))),
            };
            if is_mcp_app_resource(&value) {
                return None;
            }
            let Value::Object(mut item) = value else {
                return Some(Err(unexpected_response(server)));
            };
            item.remove("_meta");
            item.remove("icons");
            item.insert("server".into(), json!(server));
            Some(Ok(Value::Object(item)))
        })
        .collect()
}

fn is_mcp_app_resource(value: &Value) -> bool {
    let uri = value
        .get("uri")
        .or_else(|| value.get("uriTemplate"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uri.starts_with("ui://") {
        return true;
    }
    value
        .get("mimeType")
        .and_then(Value::as_str)
        .is_some_and(|mime| {
            mime.split(';').skip(1).any(|parameter| {
                let parameter = parameter.trim_start().to_ascii_lowercase();
                parameter
                    .strip_prefix("profile")
                    .and_then(|value| value.trim_start().strip_prefix('='))
                    .is_some_and(|value| {
                        let value = value.trim_start();
                        value
                            .strip_prefix('"')
                            .unwrap_or(value)
                            .starts_with("mcp-app")
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_output_schemas_describe_complete_pi_payloads() {
        // Oracle: extensions/mcp/resources.ts LIST*_OUTPUT_SCHEMA and READ_OUTPUT_SCHEMA.
        for (operation, key, uri_key) in [
            (ResourceOperation::List, "resources", "uri"),
            (
                ResourceOperation::Templates,
                "resourceTemplates",
                "uriTemplate",
            ),
        ] {
            let tool = ResourceTool {
                operation,
                servers: Arc::new(Vec::new()),
            };
            let mut properties = json!({
                "server":{"type":"string"}, "name":{"type":"string"},
                "title":{"type":"string"}, "description":{"type":"string"},
                "mimeType":{"type":"string"}
            });
            properties[uri_key] = if uri_key == "uriTemplate" {
                json!({"type":"string","description":"RFC 6570 URI template"})
            } else {
                properties["size"] = json!({"type":"number"});
                json!({"type":"string"})
            };
            assert_eq!(
                tool.spec().output_schema,
                Some(json!({
                    "type":"object", "properties":{
                        "server":{"type":"string"}, "nextCursor":{"type":"string"},
                        (key):{"type":"array","items":{
                            "type":"object", "properties":properties,
                            "required":["server",uri_key,"name"]
                        }},
                        "errors":{"type":"array","description":"Servers that could not be listed","items":{
                            "type":"object","properties":{"server":{"type":"string"},"error":{"type":"string"}},
                            "required":["server","error"]
                        }}
                    },"required":[key]
                }))
            );
            let payload = json!({(key):[], "errors":[{"server":"docs","error":"unavailable"}]});
            assert_eq!(
                tool.structured_output(&project_resource_listing(payload.clone())),
                Some(payload)
            );
        }
        let read = ResourceTool {
            operation: ResourceOperation::Read,
            servers: Arc::new(Vec::new()),
        };
        assert_eq!(
            read.spec().output_schema,
            Some(json!({
                "type":"object", "properties":{
                    "server":{"type":"string"}, "uri":{"type":"string"},
                    "contents":{"type":"array","items":{"anyOf":[
                        {"type":"object","properties":{
                            "uri":{"type":"string"},"mimeType":{"type":"string"},"text":{"type":"string"}
                        },"required":["uri","text"]},
                        {"type":"object","properties":{
                            "uri":{"type":"string"},"mimeType":{"type":"string"},"blob":{"type":"string","description":"base64"}
                        },"required":["uri","blob"]}
                    ]}}
                },"required":["server","uri","contents"]
            }))
        );
        let result = project_resource_read(
            "docs",
            "doc://guide",
            serde_json::from_value(json!({
                "contents":[{"uri":"doc://guide","text":"Guide","_meta":{"private":true}},
                    {"uri":"doc://image","mimeType":"image/png","blob":"aW1hZ2U="}]
            }))
            .unwrap(),
        );
        assert_eq!(
            read.structured_output(&result),
            Some(json!({
                "server":"docs","uri":"doc://guide","contents":[
                    {"uri":"doc://guide","text":"Guide"},
                    {"uri":"doc://image","mimeType":"image/png","blob":"aW1hZ2U="}
                ]
            }))
        );
    }

    #[test]
    fn listing_filters_ui_entries_and_private_metadata_for_resources_and_templates() {
        let entries = vec![
            json!({"uri":"file:///readme", "name":"readme", "icons":[{}], "_meta":{"private":true}}),
            json!({"uri":"ui://app", "name":"app"}),
            json!({"uriTemplate":"ui://app/{id}", "name":"template"}),
            json!({"uri":"https://app", "name":"app", "mimeType":"text/html; PROFILE = \"MCP-APP\""}),
            json!({"uri":"https://docs", "name":"docs", "mimeType":"text/html; profile=ordinary"}),
        ];
        assert_eq!(
            listed_items("docs", entries).unwrap(),
            vec![
                json!({"server":"docs","uri":"file:///readme","name":"readme"}),
                json!({"server":"docs","uri":"https://docs","name":"docs","mimeType":"text/html; profile=ordinary"}),
            ]
        );
    }

    #[test]
    fn string_arguments_follow_pi_trimming_and_empty_value_semantics() {
        assert_eq!(
            string_argument(&json!({"server":" docs "}), "server").unwrap(),
            Some("docs")
        );
        for input in [json!({}), json!({"cursor":null}), json!({"cursor":" \t"})] {
            assert_eq!(string_argument(&input, "cursor").unwrap(), None);
        }
        assert!(matches!(
            string_argument(&json!({"cursor":42}), "cursor"),
            Err(ToolError::InvalidArguments(_))
        ));
    }
}
