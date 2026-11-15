# pi-plugin-mcp

`pi-plugin-mcp` owns Pi's first-party Model Context Protocol integration: transports,
discovery, tool invocation, scoped local configuration, management commands and cleanup.
It consolidates the former `pi-mcp` client crate and the SDK's MCP product implementation.
Current upstream Pi also provides MCP through its built-in extension. This Rust implementation
uses direct tool registration and generation-based connections, without Pi's codemode/deferred exposure.

## Explicit-server API

Call `McpToolSet::connect(configs)` with `McpServerConfig` values to connect stdio or
Streamable HTTP servers and discover their tools. `McpToolSet::plugin()` returns a
`Plugin` with those tools and their usage instructions; it never reads local configuration files
or registers `/mcp`. This is the API used for client-supplied ACP servers.

Pool clones share connection ownership. Invocation preserves structured results and
cancellation; `shutdown()` closes transports and waits for child cleanup. Tool names use
`mcp__<server>__<tool>` after identifier normalization. Legacy standalone HTTP+SSE is not
supported; Streamable HTTP supports both JSON and SSE responses. Client configuration is
transient and is not serialized into Pi session history.

Both entry points carry negotiated server instructions into the run-local prompt when that
server has active tools. Instructions are captured during connection, replaced on reload and
never accumulated in the base prompt. Official documentation tools, such as Cua Driver's
`get_guide`, remain callable through the same tool surface; native MCP skill/resource browsing
is not required for that route.

Tools with `readOnlyHint: true` may execute in parallel. Mutating and unannotated tools run
sequentially, preserving action/observation ordering. These hints do not grant permissions.
Text and screenshot content use normal Pi content blocks; full structured results are retained
in tool-result details.

For desktop and browser control, connect the official Cua Driver using the
[Cua Driver setup](#cua-driver).

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

The crate has no `pi-coding`, `pi-acp` or `pi-session` dependency. Frontend adapters resolve trust,
adapt their request data and choose which entry point to call. Merely depending on this crate
does not load a local library or start a connection. This migration adds no feature flag,
new plugin lifecycle, activation transaction or runtime behavior.

Deterministic client/config/library regressions remain the default validation path. The
`deepwiki_live` integration test is opt-in and ignored because it requires public network access.

## Cua Driver

Pi uses its normal model loop with the official `cua-driver mcp` tools.

### Setup

Install the official [Cua Driver](https://github.com/trycua/cua/tree/main/libs/cua-driver)
and complete its OS permission setup. On macOS, the standalone installation uses
`CuaDriver.app`; `cua-driver permissions grant` opens its permission setup. Keep a graphical
session unlocked when testing desktop input.

Find the installed executable with `command -v cua-driver`. Merge this entry into the global
`<agent-dir>/mcp.json` (normally `~/.pi/agent/mcp.json`), replacing the command with that
absolute path and preserving other servers:

```json
{
  "version": 1,
  "mcpServers": {
    "cua-driver": {
      "type": "stdio",
      "command": "/absolute/path/to/cua-driver",
      "args": ["mcp"],
      "enabled": true
    }
  }
}
```

A trusted project's `.pi/mcp.json` can also configure the server. Global and project entries
follow the ordinary MCP merge rules; a project entry replaces the complete same-name entry.

Start Pi or run `/mcp reload`, then `/mcp test cua-driver` and `/mcp status`. Use
`./scripts/pi-dev` when validating changes from this source checkout. `/mcp test` verifies
connection and tool discovery without sending desktop input. Set the entry's `enabled` to
`false` and reload to disable it.

### How it runs

The model receives Cua's official tool definitions and negotiated usage instructions.
Detailed guidance is available through `mcp__cua_driver__get_guide`. Normal Pi tool calls
provide observation, clicks, typing, browser actions and Cua's `run_steps`/`run_script`
batching. Pi does not rewrite them into a separate candidate/action vocabulary.

MCP tools explicitly marked read-only may run in parallel; mutating and unannotated tools
run sequentially. Driver instructions are added only when its tools are active. Text and
screenshots are returned through Pi's normal tool-result path; screenshot-based reasoning
requires a model that accepts images and `images.blockImages` to be disabled.

The MCP connection is long-lived for its generation. Cua creates and cleans up its implicit
session with that transport. On reload Pi prepares a new connection before replacing the
old generation. Stopping an action does not undo input already delivered to an application.

For Chromium pages, follow the driver's `get_browser_state` and `browser_prepare` guidance.
Attaching to an existing signed-in profile requires Cua's explicit authorization; the standard
entry above does not pre-grant it. Cua also supports an isolated browser profile.

### Verify

Start with a read-only prompt:

> Use the Cua Driver MCP tools to list Chrome windows and inspect the active page. Report its
> title and URL from the observed state.

Then try a small navigation task:

> Use Cua Driver to open https://github.com/CCherry07 in Chrome. Read back the page title and
> URL and confirm that the profile is visible.

An opt-in integration test checks the installed driver's official schemas, server instructions
and read-only permission/guide calls without sending desktop input:

```sh
PI_CUA_DRIVER_BIN="$(command -v cua-driver)" cargo test -p pi-plugin-mcp \
  --test cua_driver_live -- --ignored --nocapture
```

### Migration from the task engine

`gui_task`, `/computer`, the `computerUse` settings object and the in-process
`cua-driver-sdk` dependency have been removed. Configure Cua in `mcp.json`; a separate
`decisionModel`, SystemOne account and task trace settings are no longer used. Old settings
can be removed without changing the normal model catalog or session history.

Cua follows ordinary MCP trust, tool selection and session inheritance. It no longer has a
separate global-only enable switch or primary-session restriction. Existing tool-call hooks
continue to apply to its tools, and `Features::none()` leaves explicit MCP configuration active.
