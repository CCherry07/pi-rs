# pi-plugin-mcp

`pi-plugin-mcp` owns Pi's first-party Model Context Protocol integration: transports,
discovery, tool invocation, scoped local configuration, management commands and cleanup.
It consolidates the former `pi-mcp` client crate and the SDK's MCP product implementation.
MCP remains a deliberate Rust product extension: upstream Pi has no built-in MCP.

## Explicit-server API

Call `McpToolSet::connect(configs)` with `McpServerConfig` values to connect stdio or
Streamable HTTP servers and discover their tools. `McpToolSet::plugin()` returns an
`Plugin` that registers those tools only; it never reads local configuration files
or registers `/mcp`. This is the API used for client-supplied ACP servers.

Pool clones share connection ownership. Invocation preserves structured results and
cancellation; `shutdown()` closes transports and waits for child cleanup. Tool names use
`mcp__<server>__<tool>` after identifier normalization. Legacy standalone HTTP+SSE is not
supported; Streamable HTTP supports both JSON and SSE responses. Client configuration is
transient and is not serialized into Pi session history.

## Local-library API

`McpLibrary::new(agent_dir, cwd, trusted)` consumes a project-trust decision already resolved
by the host. The plugin does not evaluate or persist trust. Global `<agent-dir>/mcp.json`
is always eligible; project `<cwd>/.pi/mcp.json` is neither read nor written unless trusted.
Project entries replace complete global entries by name, including disabled entries.

- `read(scope)` returns the raw document, revision, merged server rows and diagnostics.
- `save(scope, revision, content)` validates and atomically saves with external-change checks,
  preserving unknown JSON fields supplied by the caller.
- `test(name)` explicitly connects one saved, enabled server, reports its tools and shuts down.
- `prepare().await` connects the enabled servers and returns an `Arc<dyn Plugin>` with
  their tools and `/mcp status|paths|test|reload` commands.

Reads and saves neither connect nor construct sessions. Preparation completes before a host
publishes its runtime generation; a failure must leave the previous generation active.
Commands use ordinary Pi plugin capabilities. Saving configuration does not mutate a live
registry; `/mcp reload` requests the existing whole-session replacement transaction.

## Configuration and boundaries

The public `config` module owns parsing `mcpServers` documents, static transport validation
and connection-time environment/path expansion. `parse_config` returns unresolved entries
without reading environment variables, opening files or connecting. After selecting entries,
a caller resolves them with `McpConfigContext`, specifying the configuration-directory and
default stdio cwd. Resolution supports `${VAR}`, `${env:VAR}`, whole-value `$VAR`, and `~` in
stdio paths, without shell evaluation.

The crate has no `pi-sdk`, `pi-acp` or `pi-session` dependency. Frontend adapters resolve trust,
adapt their request data and choose which entry point to call. Merely depending on this crate
does not load a local library or start a connection. This migration adds no feature flag,
new plugin lifecycle, activation transaction or runtime behavior.

Deterministic client/config/library regressions remain the default validation path. The
`deepwiki_live` integration test is opt-in and ignored because it requires public network access.
