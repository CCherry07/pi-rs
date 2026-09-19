# pi-mcp

`pi-mcp` is Pi's protocol-neutral Model Context Protocol client adapter. It starts configured
stdio MCP servers, discovers their tools, exposes those tools through one reload-safe
`AgentPlugin`, forwards calls and structured results, honors Pi cancellation, and owns transport
cleanup.

The crate deliberately does not know about ACP, CLI arguments, project trust, or Pi session
storage. A frontend converts its own MCP configuration into `McpServerConfig` and injects
`McpToolSet::plugin()` through a session generation overlay. MCP process configuration is
therefore transient and is never serialized into Pi v4 JSONL.

The `config` module owns parsing `mcpServers` documents, static transport validation, and
connection-time environment/path expansion. `parse_config` returns unresolved `McpConfigEntry`
values without reading environment variables, opening files, or starting transports. After
selecting entries, a caller invokes `resolve(McpConfigContext)` with the configuration directory
and default stdio working directory; it returns a ready-to-connect `McpServerConfig`. Resolution
supports `${VAR}`, `${env:VAR}`, whole-value `$VAR`, and `~` in stdio paths, without shell evaluation.
Discovery, global/project precedence, trust checks before reads, raw-document persistence and
commands remain with the product adapter. MCP remains a deliberate Rust product extension, not
upstream Pi built-in behavior.

Tool names are qualified as `mcp__<server>__<tool>` after identifier normalization, preventing
different servers from silently replacing each other's tools.
