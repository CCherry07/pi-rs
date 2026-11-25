# pi-plugin-mcp

`pi-plugin-mcp` owns Pi's first-party Model Context Protocol integration: transports,
discovery, tool invocation, scoped local configuration, management commands and cleanup.
It consolidates the former `pi-mcp` client crate and the SDK's MCP product implementation.
Current upstream Pi also provides MCP through its built-in extension. This Rust implementation
uses generation-based connections and supports direct/codemode/deferred/hidden exposure. Omitted exposure
means `codemode`, matching Pi. Set `"exposure": "direct"` explicitly to declare tools to the model.
Codemode (including its `codemode-deferred` alias) is supported by the independent
[`pi-plugin-codemode`](codemode.md) script runtime, included in the coding product.
Top-level `autoEnableCodemode` controls automatic script-entry activation and defaults to `true`.
HTTP `oauth` settings and global-only `auth.provider` are supported by the transport layer.
Provider tokens use a host-injected resolver; Coding/CLI install their shared stored-first credential
resolver without borrowing the selected model's explicit CLI key.

## Explicit-server API

Call `McpToolSet::connect(configs)` with `McpServerConfig` values to connect stdio or
Streamable HTTP servers and discover their tools. `McpToolSet::plugin()` returns a
`Plugin` with server tools, shared resource tools when available, and usage instructions; it never reads local configuration files
or registers `/mcp`. This transport-only API explicitly supplies direct tools, including for
client-supplied ACP servers. Local `mcp.json` uses the codemode default described below.

Pool clones share connection ownership. Invocation preserves structured results and
cancellation; `shutdown()` closes transports and waits for child cleanup. Tool names use
`mcp__<server>__<tool>` with Pi's case-preserving identifier normalization and 64-character limit.
Long names and every member of a colliding name group receive a stable identity hash suffix.
Server names use letters, digits, `_` and `-`; configurations cannot share a namespace.
Legacy standalone HTTP+SSE is not supported; Streamable HTTP supports both JSON and SSE responses. Client configuration is
transient and is not serialized into Pi session history.

Both entry points attach negotiated instructions to tool namespaces. The Agent projects instructions
for active namespaces into each provider request, including immediately after discovery. Instructions
are captured during connection, replaced on reload and never accumulated in the base prompt. Official documentation tools, such as Cua Driver's
`get_guide`, remain callable through the same tool surface; native MCP skill/resource browsing
is not required for that route.

Tools with `readOnlyHint: true` may execute in parallel. Mutating and unannotated tools run
sequentially, preserving action/observation ordering. These hints do not grant permissions.
Text, screenshots and embedded text/image resources use normal Pi content blocks. Structured-only
results fall back to model-visible JSON; full raw results remain in tool-result details. Model-facing
text above 20 KiB is cut in the middle with full output saved to a private temporary file. Binary
resources also use persistent temporary files so subsequent read-tool calls can retrieve them.
Calls time out after 60 seconds without progress by default; local configuration can set `timeout`
in seconds. Matching progress notifications reset the timer and produce normal tool updates.
Abort and timeout cancel the remote request. Stable MCP roots report the session working directory.
Servers without a tools capability never receive `tools/list`; resources-only servers use the shared
resource tools below. `McpToolSet::tools()` continues to return server-tool descriptors; the plugin
registry also contains the shared resource tools.

For desktop and browser control, connect the official Cua Driver using the
[Cua Driver setup](#cua-driver).

## Resource tools

A visible server advertising the MCP resources capability registers these three shared tools:

| Tool | Arguments | Behavior |
| --- | --- | --- |
| `list_mcp_resources` | Optional `server`, `cursor` | One page for a named server; all pages across servers when omitted. |
| `list_mcp_resource_templates` | Optional `server`, `cursor` | Same pagination; a server without this method returns an empty list. |
| `read_mcp_resource` | Required `server`, `uri` | Reads content through that server and projects text, images and binary files. |

Listings tag each item with its configured server name and preserve `nextCursor` for named-server
pages. A cursor requires a server. Cross-server listings run concurrently and retain successful
servers plus an `errors` array for failures. Aggregate ordering uses deterministic server-name order;
repeated cursors and excessive pagination fail rather than looping. Listing payloads and read
payloads remain complete in result details even when model-facing text is truncated.

A hidden server is excluded from all three tools, including when a `toolExposure` override makes
one of its ordinary tools direct. Disabling or hiding the last resource server removes the shared
tools on reload. Tool-only servers are unaffected. Resource namespace instructions enter each provider request
when a resource tool is active, and resource links suggest `read_mcp_resource` only when it is active
and can reach that server.

Like Pi, listings remove icons, transport `_meta`, and MCP Apps UI entries (`ui://` or
`profile=mcp-app`). Read payloads remove per-resource `_meta`; explicit URI reads delegate validation
to the selected server and do not require a previous listing. This provides resource data access;
it does not render MCP Apps. Resource requests share normal cancellation, progress and the server's
idle timeout. `/mcp status` reports resource availability and `/mcp test <server>` includes the
shared tools for a resources-only server.

## Deferred tool discovery

Set `"exposure": "deferred"` on a server or in a `toolExposure` override to register tools without
including their schemas in the initial model request. Enabled configuration containing any deferred
exposure automatically supplies `tool_search`, including overrides that match no current tool.
The model sees a bounded `mcp_servers` directory (250 UTF-16 units per summary, 4096 for the section).

`tool_search` accepts `query` and optional positive-integer `limit` (default 8). It searches tool names,
descriptions, schema fields and namespace metadata with BM25, including Unicode dictionary word
segmentation. Matches load in ranked order for the next model request. Hidden tools, inactive direct
tools and already loaded tools are excluded. The generic implementation is independently reusable as
[`pi-plugin-tool-search`](tool-search.md); it never connects to MCP itself.

Loaded tools use ordinary session selection records and survive branch navigation, resume and reload.
Hiding/removing a tool on reload removes it from the selection while retaining historical results.
Explicit tool selections may also name deferred tools. Resource tools use the widest exposure of their
visible servers: any direct resource server makes the shared tools direct; otherwise they are deferred.

Rust composition supplies the search tool from the MCP plugin only when needed; do not also register
`ToolSearchPlugin` in that same generation. Duplicate names still fail registration. This differs from
Pi's always-registered, initially inactive search extension. MCP's omitted exposure is `codemode`.

## Codemode scripts

Omit `exposure` or set `"exposure": "codemode"` to keep a server's schemas out of the initial model
request and call its tools through JavaScript. `codemode-deferred` is an alias for `codemode`. Both map to generic
deferred registration, matching Pi; an already active `tool_search` can also discover these tools.
Scripts use `searchTools()` for discovery and `describeNamespace()` for negotiated instructions.
The coding product registers `codemode` by default. Embedded hosts register `CodemodePlugin` themselves.
At session startup (including reload and resume), the MCP plugin appends `codemode` to the restored
active selection when an enabled server requests codemode exposure, including any per-tool override.
This also applies when `settings.json` `defaultTools` omits it. The registered tool must declare
`Tool::reaches_tools`; an unrelated tool with the same name is never activated.

Set top-level `"autoEnableCodemode": false` in `mcp.json` to disable this automatic addition. Omission
inherits the global value, defaulting to `true`; a trusted project's explicit boolean overrides it.
`false` does not deactivate an already selected tool. Deferred exposure independently activates
`tool_search`. If neither capable entry point is active, startup emits a warning. Invalid flag types
reject configuration preparation, leaving the current generation intact on reload.

Activation uses ordinary Session tool selection and its v4 persistence; it does not materialize a
new session file before the first assistant response. Child sessions use the same automatic activation
rules within their allowed tool catalog. A child allowed only `read` cannot gain `codemode`; a child
allowed `codemode` can reactivate it on reload even after deselection. Deferred tools stay available
behind permitted entry points. Connections still prepare before generation publication, unlike Pi's
asynchronous startup discovery.

```js
const matches = await searchTools("documentation search");
text(matches);
// Once the tool's name and arguments are known:
// const result = await tools.mcp__docs__search({ query: "generations" });
// text(result.structuredContent ?? result.content);
```

Scripts receive each MCP tool's complete `CallToolResult` (`content`, `structuredContent`, `isError`),
without the top-level transport `_meta` and without model-facing text truncation. A remote MCP
`isError` result still resolves to that object; blocked calls and transport failures reject.
Resource helpers return their complete listing/read payload. The script chooses which output reaches
the model, while nested calls retain the regular validation, hooks, cancellation and execution scope.

## Local-library API

`McpLibrary::new(agent_dir, cwd, trusted)` consumes a project-trust decision already resolved
by the host. The plugin does not evaluate or persist trust. Global `<agent-dir>/mcp.json`
is always eligible; project `<cwd>/.pi/mcp.json` is neither read nor written unless trusted.
Complete project entries replace global entries by name. A project entry without command/url/type
may override only `enabled`, `exposure`, and `toolExposure` of an existing global server, retaining
its transport and credentials. A project override cannot introduce credential fields.

- `read(scope)` returns the raw document, revision, merged server rows and diagnostics.
- `save(scope, revision, content)` validates and atomically saves with external-change checks,
  preserving unknown JSON fields supplied by the caller.
- `add`, `remove`, `set_enabled`, and `set_exposure` are the typed management seam used by
  frontends. They retain unrelated JSON and wildcard declaration order, lock per scope, detect
  external changes, and atomically replace the file. Add validates the new definition; add/remove
  preserve unrelated invalid entries so the file can be repaired. Policy edits validate inherited
  configuration before writing. `contains_definition` checks a scope without confusing inherited rows.
- `inspect().await` probes configured servers independently and returns serializable state, tools,
  resource availability, and sanitized failures. A failed server does not hide disabled or healthy
  rows, including when another entry is malformed; configuration diagnostics make the inspection
  unsuccessful. Reports include hidden tools, effective per-tool exposure differences and project
  override provenance and optional `authProvider`. Inspection does not list resource contents or
  publish tools into a registry.
- `test(name)` explicitly connects one saved, enabled server, reports its tools and shuts down.
- `prepare().await` connects the enabled servers and returns an `Arc<dyn Plugin>` with their tools
  and `/mcp` management. Interactive `/mcp` offers state-dependent actions, a read-only Tools view,
  reconnect, sign-in/out, enable/disable, and server-wide exposure. Tools retain their remote names,
  first description line and differing effective exposure, including hidden tools; selecting one
  never edits policy. Back/cancel returns to the enclosing selector without writing or reloading.
  Global servers offer trusted project-only enable/disable overrides without copying credentials.
  Omitted login/logout/reconnect names select the sole eligible (or sole pending OAuth) server,
  otherwise a selector; noninteractive ambiguous requests must name a server. Native CLI argument
  completion uses plugin-owned generation snapshots, never a live registry mutation.

Reads and saves neither connect nor construct sessions. Shell-level inspection is explicit and does
not construct a session. Preparation completes before a host publishes its runtime generation; a
failure must leave the previous generation active. Commands use ordinary Pi plugin capabilities.
Saving configuration does not mutate a live registry: enable/disable, exposure, reconnect, and
`/mcp reload` all request the existing whole-session replacement transaction after their candidate
configuration or connection check succeeds. Reload failure preserves the active generation, not
its prior disk configuration: saved edits remain on disk, and the error explains retry/revert steps.

## Shell management

The native CLI exposes the session-free control plane through `pi mcp`:

```text
pi mcp add <server> [--env KEY=VALUE] -- <command> [args...]
pi mcp add <server> --url <url> [--header KEY=VALUE]
pi mcp remove <server> [--local]
pi mcp list [--json]
pi mcp login <server> [--local]
pi mcp logout <server> [--local]
```

`--local` targets `<cwd>/.pi/mcp.json`; otherwise commands target
`<agent-dir>/mcp.json`. Explicit local authoring is allowed before trust, but list/session loading
continues to ignore that file until the normal project-trust service says the project is trusted.
`list` exits `1` when configuration or any enabled connection fails and `0` when every enabled
server connects; disabled servers are successful rows. JSON includes `servers`, `errors`, and an
optional untrusted-project `note`. Unknown login/logout names include nearby-name suggestions and
configured-name/untrusted-project guidance. Add/remove never create a session or require model credentials.
HTTP authoring accepts `--oauth-client-id`, `--oauth-client-secret`, `--oauth-callback-port`,
`--oauth-client-name` and `--oauth-client-registration auto|cimd|dcr`. Validation does not resolve
secret references, run commands, sign in, or create a credential store. Invalid options leave disk unchanged.

`McpLibrary::login` validates the defining scope, probes first, hands a typed auth challenge to the
installed `McpOAuthHost`, and reconnects after the real exchange. Already-authorized servers skip UI.
The CLI's native host owns browser launch and loopback listening, with hidden pasted-redirect fallback;
the default embedding host remains manual (`127.0.0.1:53682/callback` unless configured). Native callbacks
use an ephemeral port by default and honor configured URL/port plus the actual CIMD redirect path.
`begin_login` remains available for low-level callers. No token is fabricated when authorization starts.
Shell login supports positive finite fractional `--timeout <seconds>` (default 300, within Rust's
representable duration range); semantic login is bounded to 300 seconds and observes cancellation.
Non-TTY shell input still permits browser callbacks and already-authorized probes; only pasted input
is disabled, without consuming piped stdin or starting a terminal reader. Both semantic login and logout request a complete generation reload.
`logout` removes only the selected name/URL credentials. Provider/header auth is rejected by these
commands; `--local` requires a trusted project-defined transport rather than an inherited policy override.
Inspection and generation connections share private `<agent-dir>/mcp-auth.json` state. Coding supplies
a request-time stored-first provider-token resolver; no Coding policy or browser code lives here.

Product preparation retains only typed authorization-required servers as pending so `/mcp` remains
reachable. Inspection reports `needs-auth` as unsuccessful; unrelated config/connection failures remain
fatal to generation preparation. Strict tool-only pools, including ACP, still return a typed error.
`McpCredentialWatch` exposes opaque revisions for eligible identities, excluding pending authorization
and unrelated credentials. Coding captures revisions before connecting and asks its generic session
owner to replace the entire generation before the next idle submission if credentials changed.
ACP has no browser or local discovery and reuses credentials only when its host explicitly injects a store.

Local-library connections append server `notifications/message` to private `<agent-dir>/mcp.log`,
with a UTC millisecond timestamp, server, level, optional logger and indented continuation lines.
Before an append, a file larger than 5 MiB rotates to `mcp.log.1`. Writes and rotation are best effort;
an unavailable sink or another process holding its lock never fails an MCP request. This is a server
message file, not a live manager log viewer or a transcript of protocol traffic. Explicit transport-only
and ACP clients never infer a log path. Merely reading/saving config or preparing disabled servers
does not create log files.

Stdio stderr is continuously drained into a bounded in-memory tail, not sent to the terminal or the
log file. Initialization/discovery failures and disconnects include up to the last 2,000 UTF-16 units
of diagnostic text. The manager reports disconnected transports without changing their frozen tool
catalog. Configured env/header/argument/secret values and inherited credential-like environment
values are redacted before tail truncation; terminal control characters are removed. Redaction is
not a general detector for arbitrary secrets a server invents or transforms, so treat `mcp.log` as
private server output. Child lifetime remains rmcp-owned; the drain is cancelled on failed admission,
shutdown or owner drop, with a bounded final drain for inherited stderr pipes.

This is not a full parity claim. Connection lifecycle, extension-registration contracts, and official
conformance evidence remain separate work. Per-tool editing and an interactive live log viewer are not provided by the pinned
legacy manager either; edit `toolExposure` in the file. Tests use deterministic local fixtures,
never real-account sign-ins.

## Configuration and boundaries

The public `config` module owns parsing `mcpServers` documents, static transport validation
and connection-time environment/path expansion. BOM-prefixed documents, `{}`, omitted `version`,
and the `streamable-http` transport alias are accepted. Without a type, URL takes precedence over
command. `parse_document` returns unresolved entries plus the optional `auto_enable_codemode` flag,
without reading environment variables, opening files or connecting. The entry-only `parse_config`
rejects explicit `autoEnableCodemode` to avoid silently discarding document policy; use `parse_document`
or `McpLibrary` for full configurations. `McpConfigContext.default_cwd` is the session directory and the base for explicit
relative stdio cwd; `config_dir` remains as provenance for source compatibility. Env/header/OAuth
client-secret values support embedded `$VAR` and `${VAR}`, `$$` / `$!` escapes and the Rust
`${env:VAR}` spelling. Missing, empty or non-UTF-8 environment references fail without echoing values;
invalid reference spellings remain literal. Substituted values are never rescanned as commands.
Rust also retains its existing environment references in URL/command/args/cwd and `~` expansion in
command/args/cwd; these transport fields never run value commands. Transport-only
`McpConfigEntry::resolve` refuses entries without plain
direct policy (including omitted exposure, which now means codemode); use `McpLibrary::prepare`
to preserve exposure and request settings.

`McpLibrary::with_command_values(true)` explicitly enables leading `!command` for env/header/OAuth
client-secret values. Coding and native CLI enable it only on their trust-filtered local library.
Trusted configuration can therefore execute env/header helpers when connecting or explicitly
probing: treat it as executable configuration. OAuth client-secret helpers run only for an explicit
sign-in or a needed token refresh, not for connection setup with a usable stored token. Concurrent
requests share a refresh; cancelling one waiter does not abort its shared exchange or token save.
Each request has at most one 401-triggered retry, and refresh cannot overwrite a newer login/logout.
Each resolution runs afresh in
the session cwd with a 10-second bound and separate 1 MiB stdout/stderr limits. Timeout, cancellation
or overflow kills the process tree. Successful stdout must be UTF-8 and nonempty after trimming;
stderr never becomes the value, and errors include neither command text nor captured bytes. Capture
is in memory only, without a transcript or output file. Static Authorization bypasses OAuth helpers.
Parsing, authoring, saving and metadata/credential-identity queries do not execute helpers or resolve
secret fields. Disabled entries and untrusted project entries never execute them. Explicit transport
and ACP configurations never gain command execution from these APIs.

`exposure` supports `direct`, `codemode`, `deferred` and `hidden`; `toolExposure` keys are original remote tool names or `*`
patterns. Exact names win, then the first declared matching pattern. Hidden tools are excluded from
the immutable registry, including direct execution, not only model declarations. `description` adds
a server summary to the run-local prompt and `timeout` controls the per-call idle timeout.

For example, expose only read tools and one explicitly allowed tool:

```json
{
  "mcpServers": {
    "docs": {
      "url": "https://example.com/mcp",
      "exposure": "hidden",
      "toolExposure": { "read*": "direct", "search": "direct" },
      "timeout": 90
    }
  }
}
```

### Configuration migration

An explicit relative `cwd` now resolves from the session directory, matching Pi; use an absolute path
to retain an old configuration-directory-relative location. Canonical names now preserve case,
separators and Pi's hash suffix rules. Stored history is unchanged; old selected names that no longer
exist are treated like unavailable tools on resume. Update explicit `--tools` names and scripts to
use `/mcp test <server>` output. No aliases are registered, because an old normalized name can refer
to a different tool under the new rules.

The crate has no `pi-coding`, `pi-acp` or `pi-session` dependency. Frontend adapters resolve trust,
adapt their request data and choose which entry point to call. Merely depending on this crate
does not load a local library or start a connection. Both entry points retain the existing plugin
lifecycle and generation activation transaction.

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

With the default codemode exposure, scripts discover Cua's official tools and negotiated instructions
through `describeNamespace("mcp__cua_driver")`. Set `"exposure": "direct"` to declare the schemas
directly to the model. Detailed guidance is available through `mcp__cua_driver__get_guide`. Pi tool calls
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
