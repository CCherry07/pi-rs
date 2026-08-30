# Pi Coding Agent

This context names the product-level session concepts shared by the Rust runtime, frontends, and Pi-compatible JavaScript extensions.

## Language

**MultiSessionManager**:
The product-level owner of multiple active `PiSession` handles and their collective lifecycle.
_Avoid_: PiApplication, SessionRegistry

**PiSession**:
A stable frontend handle whose current `AgentSession` may be atomically replaced by new, resume, fork, or reload operations.
_Avoid_: SessionManager, AgentSession

**AgentSession**:
One concrete generation of an active coding-agent conversation.
_Avoid_: PiSession

**PluginContext**:
The generation-scoped capability source shared by Rust plugins and Pi-compatible extensions.
_Avoid_: ProductContext, AgentPluginContext

**AgentPluginContext**:
The callback context for one agent-plugin hook invocation, including run identity, cancellation, diagnostics, and typed capabilities.
_Avoid_: PluginContext, AgentContext

**PluginContextError**:
A failure to use a plugin capability because its generation retired, its session is unbound, its scope is insufficient, or the requested operation failed.
_Avoid_: ExtensionContextError, ContextError

**PresentationMode**:
The product surface currently presenting a plugin-backed session, such as TUI, print, JSON, or RPC.
_Avoid_: ExtensionMode, UiMode

**SessionSnapshot**:
An immutable plugin-facing view of one session's identity, current branch, entries, labels, and unknown wire data.
_Avoid_: SessionDocument, SessionState

**SessionManager**:
The Pi-compatible view of one session's entries, tree, branch, labels, and persistence identity, exposed to JavaScript extensions as `ctx.sessionManager`.
_Avoid_: MultiSessionManager, SessionRegistry

**PiNodeHost**:
The Node.js product entry point that owns JavaScript extension execution and launches the native Pi product.
_Avoid_: PiApplication

**NativePiHost**:
The private N-API adapter that sends generation operations to Node and injects a generation-scoped `NativeExtensionContext` into callbacks.
It is not a session manager and owns no product or terminal policy.
_Avoid_: NativePiApplication, JsContextBroker
