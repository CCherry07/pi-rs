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
