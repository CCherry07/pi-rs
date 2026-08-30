# pi-acp

`pi-acp` exposes Pi as an Agent Client Protocol agent using the official Rust SDK and ACP stable
v1. `AcpServer` accepts any SDK transport; `serve_stdio` is the product entry point used by
`pi --acp`.

Implemented protocol surface:

- initialization and capability negotiation;
- new, prompt, cancel, load, resume, list, and close session methods;
- streamed assistant text/thought and tool-call updates;
- image, resource-link, and embedded text context prompts;
- model and thinking-level session configuration options;
- per-session stdio MCP servers through `pi-mcp`.

Each ACP session is backed by `MultiSessionManager` / `PiSession`. Durable conversation state stays
in Pi v4 JSONL; MCP transports and the generation overlay are session-local and transient. HTTP/SSE
MCP, additional workspace directories, audio prompts, and JavaScript extensions in ACP mode are not
advertised.
