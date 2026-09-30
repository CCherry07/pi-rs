# pi-plugin

Shared plugin contracts for pi-rs. One `Plugin` owns typed preparation, registration, Agent hooks and Session hooks.
`ProviderPlugin` remains a separate interface for providers, routing and model catalogs.

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use pi_plugin::{Plugin, PluginId, SessionPluginContext, PluginError, SessionStartEvent};

#[derive(Default)]
struct Example {
    started: AtomicBool,
}

#[pi_plugin::plugin]
impl Plugin for Example {
    fn id(&self) -> PluginId { PluginId::new("example") }

    async fn session_start(
        &self,
        _: &SessionPluginContext,
        _: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.started.store(true, Ordering::Release);
        Ok(())
    }
    // register(), before_agent_start(), tool_call(), session_shutdown(), etc.
    // use the same instance and state.
}
```

Register once on `PiRuntime::builder()` using `plugin_factory`, `try_plugin_factory`, or typed
`prepare_plugin::<P>(agent_dir, project_trusted, options)`. `AgentSessionOptions` has no separate plugin list.
`PluginDriver` owns the immutable registration set and directly dispatches both callback families.
Session dispatch receives a `SessionDispatchContext` containing identity and generation; capabilities
come from the driver's generation-bound context. Agent, Session and Provider hooks return `PluginError`.
`diagnostics()` / `take_diagnostics()` expose one ordered stream for Agent and Session failures.
Each `PluginDiagnostic` carries a typed `PluginHook` and optional generation metadata (present
for Session and Provider callbacks). Hook names keep their string representation in JSON;
custom background-operation names are preserved. Callbacks retain registration order and error
isolation; a before-hook cancellation still stops subsequent callbacks.

Configured plugins define construction alongside their callbacks in the same trait impl:

```rust
use pi_plugin::{Plugin, PluginId, PrepareContext, PrepareError};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
struct Options {
    enabled: Option<bool>,
}

struct Example { enabled: bool }

#[pi_plugin::plugin]
impl Plugin for Example {
    type Options = Options;

    fn id(&self) -> PluginId { PluginId::new("example") }

    fn prepare(context: &PrepareContext, overrides: Options) -> Result<Option<Self>, PrepareError> {
        // A real plugin reads its own config.json here and validates the merged result.
        // The plugin chooses its own layout within the host's agent profile.
        let _config_path = context.agent_dir().join("example/config.json");
        let enabled = overrides.enabled.unwrap_or(true);
        Ok(enabled.then_some(Self { enabled }))
    }
}
```

Recommended precedence: explicit optional overrides > plugin config file > defaults. No `new()` is
required and no shared product settings schema is imposed. `None` disables the plugin for that
candidate. `Options` and `prepare` require `Self: Sized`, so the runtime still uses `Arc<dyn Plugin>`
without carrying an options type. `ProviderPlugin` owns the same construction members independently.
`Options` describes user-configurable plugin inputs. Native loaders pass manifest `[options]`
without knowing individual fields; plugins own the type, validation, defaults and config-file
loading. Host-derived facts belong in `PrepareContext`. Product wiring should use default options
when it has no user overrides, leaving private configuration resolution inside the plugin.
The static plugin macros supply `type Options = ()` when omitted. A plugin constructed through a
host closure can omit `prepare`; calling its default typed preparation returns an initialization
error explaining that a host-provided factory is required. Native plugins without `factory` get
both unit options and a `prepare` implementation using `Default`.

To migrate, move `type Options` and `fn prepare` from the former `PluginFactory` impl into the
`Plugin` or `ProviderPlugin` impl, and import that trait at direct preparation call sites.
`prepare_plugin` requires only `P: Plugin + 'static` and cloneable options.

`PrepareContext::new(workspace, agent_dir, project_trusted)` exposes the immutable `workspace()`,
its derived `cwd()`, the host-supplied profile root through `agent_dir()`, and resolved project trust
through `project_trusted()`. The host decides whether project-local settings and resources may be
enabled; plugins read that decision from the context rather than their own `Options`. It is not a
filesystem or tool-execution permission. Runtime typed registration takes the profile directory
and resolved trust, then constructs the context from the candidate workspace on every preparation.
The framework does not discover a default profile or create plugin data/cache directories. Plugins
own those paths and may accept explicit resource paths in their options. Package resolution, load
scope and generation bookkeeping remain host concerns, outside the preparation interface.

Preparation occurs before registration and complete generation validation. Failure preserves the
active generation. A successful product reload shuts down the old instances, starts the new ones,
then publishes the replacement. Prepare only unpublished state; start workers in `session_start`
and clean them up in `session_shutdown`/`Drop`. Factories can retain resource caches across reloads;
reuse must account for all relevant inputs, including config, code, resources, paths and trust.

`pi-plugin` depends inward on `pi-core`, which holds messages, models, tool data and shared Pi v4
wire values. Executable Tool/Command/Provider contracts, capabilities, registries and ModelRuntime
live here. Storage and orchestration remain in `pi-session`, `pi-agent` and `pi-runtime`.

Native authors enable the `native` feature and use `pi_plugin::prelude` and `#[pi_plugin::native_plugin]`. Configured native plugins
use `#[pi_plugin::native_plugin(factory)]` and additionally derive `schemars::JsonSchema` for options.
ABI 24 has two kinds (`plugin`, `provider`); older native libraries must be rebuilt. The SDK owns
exports, macros generate their glue, and `pi-plugin-manager::loader` owns discovery and library lifetime.

See the [native author guide](docs/native.md) and [host manager](../pi-plugin-manager/README.md).
