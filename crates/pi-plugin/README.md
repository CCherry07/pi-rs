# pi-plugin

Shared plugin contracts for pi-rs. One `Plugin` owns registration, Agent hooks and Session hooks.
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
`prepare_plugin::<P>(context, options)`. `AgentSessionOptions` has no separate plugin list.
`PluginDriver` owns the immutable registration set and directly dispatches both callback families.
Session dispatch receives a `SessionDispatchContext` containing identity and generation; capabilities
come from the driver's generation-bound context. Agent, Session and Provider hooks return `PluginError`.
`diagnostics()` / `take_diagnostics()` expose one ordered stream for Agent and Session failures.
Each `PluginDiagnostic` carries a typed `PluginHook` and optional generation metadata (present
for Session and Provider callbacks). Hook names keep their string representation in JSON;
custom background-operation names are preserved. Callbacks retain registration order and error
isolation; a before-hook cancellation still stops subsequent callbacks.

Configured plugins implement the construction trait separately from their callbacks:

```rust
use pi_plugin::{PluginFactory, PrepareContext, PrepareError};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
struct Options {
    enabled: Option<bool>,
}

struct Example { enabled: bool }

impl PluginFactory for Example {
    type Options = Options;

    fn prepare(context: &PrepareContext, overrides: Options) -> Result<Option<Self>, PrepareError> {
        // A real plugin reads its own config.json here and validates the merged result.
        // context exposes cwd, package_dir, data_dir, cache_dir, scope and generation.
        let _config_path = context.package_dir().join("config.json");
        let enabled = overrides.enabled.unwrap_or(true);
        Ok(enabled.then_some(Self { enabled }))
    }
}
```

Recommended precedence: explicit optional overrides > plugin config file > defaults. No `new()` is
required and no shared product settings schema is imposed. `None` disables the plugin for that
candidate. The callback trait stays object-safe; typed options stay at the factory boundary.

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
