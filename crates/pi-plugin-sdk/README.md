# pi-plugin-sdk

Author-facing interface for version-locked native `pi-rs` plugins.

## Create a plugin / 创建插件

One crate exports exactly one plugin kind. Build both `cdylib` for loading and `rlib` for tests:

```toml
[package]
name = "hello"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
pi-plugin-sdk = { path = "/path/to/pi-rs/crates/pi-plugin-sdk", features = ["agent"] }
```

The SDK is currently consumed from the workspace (or a pinned Git revision). Replace the path with
an exact published version once native package distribution is released; host and plugin must use
the same SDK build fingerprint.

无配置插件实现 `Default`，作者不需要写 native constructor 或 `id()`：

```rust
use pi_plugin_sdk::agent::prelude::*;

#[derive(Default)]
pub struct HelloPlugin;

#[pi_plugin_sdk::agent]
impl AgentPlugin for HelloPlugin {
    fn register(&self, context: &mut RegisterContext<'_>) -> Result<()> {
        // context.register_tool(...)
        Ok(())
    }
}
```

## Runtime context / 运行时上下文

Native callbacks receive Pi plugin capabilities directly on their context. There is no `pi()`
accessor and plugins never retain or lock the concrete `AgentSession`. The host PluginContext is
implemented by `pi-session` against `AgentSession` / `PiSession` / `PiRuntime`; the native path does
not use the JavaScript `query` / `notify` / `request` wire protocol:

```rust
#[pi_plugin_sdk::agent]
impl AgentPlugin for HelloPlugin {
    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        let model = context.models.current()?;
        let session = context.session.snapshot()?;
        let trusted = context.session.is_project_trusted()?;

        context.ui.notify(
            NoticeLevel::Info,
            format!("model={model:?}, session={:?}, trusted={trusted}", session.name()),
        )?;

        Ok(BeforeAgentStartPatch {
            system_prompt: Some(event.system_prompt),
            ..BeforeAgentStartPatch::default()
        })
    }
}
```

`AgentPluginContext`, `InputContext`, `ToolContext`, `ProviderPluginContext`, and
`SessionPluginContext` expose three explicit capability fields: `session`, `models`, and `ui`.
There is no implicit `Deref` or generic `runtime` bucket. Session identity and live state,
model-catalogue queries, and semantic product interaction therefore remain visibly separate at call
sites. Their existing callback metadata such as plugin ID, run ID, provider/model ID, tool-call ID,
and session identity remains available through read-only accessors such as `plugin_id()`,
`run_id()`, `cwd()`, and `signal()`. `context.session.snapshot()` captures identity, entries, the
current branch, leaf, and labels in one coherent read; each entry has typed metadata and retains its
complete Pi wire value through `raw()`. `context.cwd()` is the callback execution directory;
`context.session.cwd()?` is the directory recorded by the active session. Standalone tool and
command tests use `ToolContext::standalone(...)` / `CommandContext::standalone(...)`, making the
absence of session, model, and presentation capabilities explicit.
Session contexts also expose typed `active_tools()`, `tools()`, and `commands()` reads. Async
`context.ui.confirm(title, message)` requests a semantic yes/no decision from an interactive
frontend and resolves to `false` in non-interactive product modes; the frontend retains all
terminal ownership.
`Tool::prepare_arguments(&ToolContext, ...)` and `Tool::execute(ToolContext, ...)` observe the same
runtime generation, including retirement; argument compatibility shims can therefore use the same
typed product capabilities as execution without a separate adapter context.

Registered commands receive the stronger `CommandContext`; session replacement and navigation are
unavailable on ordinary hooks and tools:

```rust
match context.session.create(NewSessionOptions::default()).await? {
    SessionReplacement::Cancelled => {}
    SessionReplacement::Replaced(session) => {
        session
            .send_user_message(
                CustomMessageContent::Text("Continue here".to_string()),
                SendUserMessageOptions::default(),
            )
            .await?;
    }
}
```

Contexts are generation-bound capabilities. A context retained after its runtime generation is
replaced returns `PluginContextError::Retired`; it never follows a stale native plugin into a
new generation. A successful `new_session`, `fork`, or `switch_session` carries a
`ReplacedSessionContext` bound to the replacement generation, while `reload` returns that fresh
context directly. Use the returned value for every follow-up operation. UI access is semantic
(`notify`) and never exposes terminal or Ratatui ownership to a plugin.

The agent macro derives the exact hook-interest set from the callback methods present in the impl.
Authors do not declare a parallel list and there is no catch-all `ALL` mode. A registration-only
plugin therefore participates in `register()` but receives no runtime hook calls. Statically linked
plugins inside a host use `#[pi_core::agent_plugin]` for the same derivation without exporting a
dynamic-library descriptor or constructor. Static provider and session plugins use
`#[pi_core::provider_plugin]` and `#[pi_session::session_plugin]`. These lifecycle attributes all
expand async callback methods, so plugin impls do not need a separate `#[async_trait]` attribute.
Lower-level async traits such as `Tool`, `Command`, and `Provider` still use `#[async_trait]` when
implemented directly.

Use `#[pi_plugin_sdk::provider]` with the `provider` feature or
`#[pi_plugin_sdk::session]` with the `session` feature for the other lifecycles. The macro must
annotate the matching trait impl. Each macro also supplies its async-trait expansion, and a dynamic
library may contain only one export macro.

## Fallible/configured construction / 配置与可失败初始化

Only configured plugins implement the additional factory interface:

```rust
use pi_plugin_sdk::provider::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Options {
    base_url: String,
}

struct AcmePlugin {
    options: Options,
}

impl NativePluginFactory for AcmePlugin {
    type Options = Options;

    fn load(
        _context: &PluginLoadContext,
        options: Self::Options,
    ) -> PluginLoadResult<Self> {
        Ok(Self { options })
    }
}

#[pi_plugin_sdk::provider(factory)]
impl ProviderPlugin for AcmePlugin {}
```

`PluginLoadContext` exposes the active cwd, immutable package directory, persistent per-plugin data
directory, disposable cache directory, load scope, and generation. It deliberately omits terminal
state, mutable registries, sessions, provider credentials, and product configuration.

## Local loading / 本地加载

Build the crate, then pass the dynamic library directly while developing:

```bash
cargo build
cargo run -p pi-cli -- --plugin target/debug/libhello.dylib
```

`--plugin` may be repeated and also accepts a `pi-plugin.toml` path or its containing directory.
The platform suffix is `.so` on Linux and `.dll` on Windows.

Installed global manifests are discovered below `<agent-dir>/plugins`. Trusted project manifests
are discovered below `<project>/.pi/plugins`; an untrusted project plugin file is never opened.

```toml
schema = 1

[plugin]
id = "hello"
version = "0.1.0"
kind = "agent"
artifact = "libhello.dylib"

[options]
# Plugin-specific typed options
```

The manifest identity must match the binary descriptor. Artifacts must remain inside their package
directory. Duplicate IDs fail within each plugin kind.

## Compatibility and reload / 兼容与重载

The export macro emits a C-layout descriptor containing ABI version, plugin kind, identity,
version, and a fingerprint derived from the SDK/core/session sources, workspace lockfile,
Rust compiler, target, panic strategy, target features, and encoded Rust flags. The host checks that
descriptor before resolving a Rust-ABI trait-object constructor. Session plugin authors consume
the lifecycle contract and durable wire types from the single `pi-session` crate; its source keeps
those surfaces grouped under `plugin/` and `types/` beside the runtime implementation. Native libraries are
trusted in-process code and are intentionally never unloaded during the process lifetime. Before
loading, the host snapshots each artifact below `<agent-dir>/cache/plugins/artifacts/<sha256>`;
unchanged content reuses one pinned handle, while a rebuilt artifact gets a new load path on reload.
The SDK also pins the `serde_json::Value` map representation used by constructor options, so feature
unification in a host workspace cannot silently change that Rust-ABI type's layout.

The current contract is native ABI **8**. ABI 8 adds async semantic confirmation to `UiContext`.
Each `message_update` carries a constant-size shared
`AssistantStream` handle and the current `StreamEvent` delta. Native hooks call `snapshot()` only
when they need cumulative content; cloning the event no longer clones the accumulated message.
Completed `turn_end` assistant messages remain shared.
ABI 6 shared one immutable cumulative partial instead of deep-cloning it per plugin.
Native hooks normally match on `event.update()`; `event.snapshot()` materializes the cumulative
assistant message only for hooks that need it.
ABI 7 also made command-session reload return the fresh `ReplacedSessionContext` instead of
silently retiring the caller with no continuation handle.
ABI 5 added the generation-bound Pi product context shared by agent, tool preparation/execution,
command, provider, and session callbacks. ABI 4 added provider header/response hooks,
ABI 3 added macro-derived agent hook interests, and ABI 2 added the shared `AgentContext` /
`added_tool_names` surface. Older artifacts are rejected before any Rust-ABI constructor is
resolved. The stable C descriptor remains `pi_plugin_descriptor_v1`; ABI 8 constructors use the
`pi_{agent,provider,session}_plugin_create_v8` symbols. Rebuild every native plugin against the
current SDK after upgrading the host.

Every factory call creates a fresh instance. Runtime and session reload continue to use the existing
fallible generation factories: a constructor, registration, or validation failure leaves the active
generation unchanged.

`pi-plugin-manager` adds local and HTTP/GitHub Release installation, static Registry resolution,
exact target selection, dependency locking, SHA-256 verification, and a content-addressed package
store. See [`../pi-plugin-manager/README.md`](../pi-plugin-manager/README.md) for the release and
Registry formats. Publisher signatures, Git repository sources, OCI artifacts, update, rollback,
and store garbage collection remain later distribution milestones.
