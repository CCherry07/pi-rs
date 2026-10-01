# pi-acp

`pi-acp` exposes Pi as an Agent Client Protocol agent using the official Rust SDK and ACP stable
v1. `AcpServer` accepts any SDK transport; `serve_stdio` is the product entry point used by
`pi --acp`.

Implemented protocol surface:

- initialization and capability negotiation;
- new, prompt, cancel, load, resume, list, and close session methods;
- streamed assistant text/thought and tool-call updates;
- opt-in `session/request_permission` before tool execution;
- image, resource-link, and embedded text context prompts;
- model and thinking-level session configuration options;
- per-session stdio and Streamable HTTP MCP servers through `pi-plugin-mcp`'s explicit-client API.

Each ACP session is backed by `MultiSessionManager` / `PiSession`. Durable conversation state stays
in Pi v4 JSONL; MCP transports and the generation overlay are session-local and transient. The explicit-client API
does not load local `mcp.json` files or register `/mcp` management commands. Legacy HTTP+SSE MCP,
additional workspace directories, audio prompts, and JavaScript extensions in ACP mode are not
advertised.

## Tool permissions

Enable permission requests from the CLI:

```sh
pi --acp --no-extensions --acp-permissions --acp-permission-timeout 300
```

Rust hosts use `AcpOptions::request_tool_permissions(Duration)`. By default this is disabled to
preserve existing ACP execution behavior. When enabled, every tool call in an ACP prompt,
including MCP tools, asks the client using the prepared arguments after earlier tool-call hooks.
The client can present a confirmation or decide automatically according to its user settings.
Project trust (`--approve` / `--no-approve`) remains independent.

Only the offered `allow-once` option permits execution. `reject-once`, a cancelled outcome,
an unknown option, protocol errors, timeout, session cancellation/close, and disconnection
block execution. Decisions are not cached. The timeout defaults to 300 seconds in the CLI
(1–86400); the agent enforces a monotonic deadline, and late responses cannot reopen a settled call.
The optional `_meta["pi-rs.dev/permission_deadline_ms"]` is the absolute Unix-millisecond deadline
for clients that want to expire their UI at the same time. It is a Pi extension, not an ACP field,
and does not modify `rawInput` or the actual tool arguments.

Waiting tools are reported as `pending`, followed by `in_progress` only after allowance, and
then the usual completed/failed tool update. The ACP dispatcher remains responsive while the
permission request is outstanding. The gate is a transient, session-local plugin rebuilt on
reload and load/resume; the conversation journal keeps normal tool results, not cached grants.
Embedded factories must apply the session overlay after their argument-rewriting tool hooks.

This authorizes tool calls made by the attached ACP session. It does not sandbox trusted plugins
or govern execution inside an allowed tool (including delegated/background sessions). It does not
convert generic plugin UI confirmations into tool permission requests.
