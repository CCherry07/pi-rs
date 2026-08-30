# pi-mcp

`pi-mcp` is Pi's protocol-neutral Model Context Protocol client adapter. It starts configured
stdio MCP servers, discovers their tools, exposes those tools through one reload-safe
`AgentPlugin`, forwards calls and structured results, honors Pi cancellation, and owns transport
cleanup.

The crate deliberately does not know about ACP, CLI arguments, project trust, or Pi session
storage. A frontend converts its own MCP configuration into `McpServerConfig` and injects
`McpToolSet::plugin()` through a session generation overlay. MCP process configuration is
therefore transient and is never serialized into Pi v4 JSONL.

Tool names are qualified as `mcp__<server>__<tool>` after identifier normalization, preventing
different servers from silently replacing each other's tools.
