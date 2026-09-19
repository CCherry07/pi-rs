# pi-sdk

`pi-sdk` is the headless product integration layer for embedding Pi in the CLI,
desktop application, and other presentation adapters.

It owns the product runtime composition: providers and model catalogs, built-in
tools, skills, memory, subagents, settings, project trust, plugin generations,
and session construction. This is selection, settings adaptation, cross-plugin wiring,
and transactional generation activation—not ownership of each component's implementation.
Domain configuration and validation stay with the corresponding crates, whose APIs accept
their own typed inputs rather than `pi_sdk::Config`. It does not own terminal, Tauri, RPC,
or other presentation behavior; Desktop extension package discovery lives in the Desktop app.
MCP client, local configuration management and `/mcp` commands live together in `pi-plugin-mcp`.
The SDK only supplies the resolved trust decision and prepares/registers the candidate plugin;
callers managing MCP files use `pi_plugin_mcp::McpLibrary` directly.

```rust
use pi_core::PresentationMode;
use pi_sdk::{Pi, Config};

let config = Config::new(cwd, agent_dir);
let pi = Pi::builder(config)
    .presentation_mode(PresentationMode::Rpc)
    .build()?;
let session = pi.sessions().create_session(cwd, session_path).await?;
```

## Internal responsibilities

- `session_factory`: complete generation preparation, trust gates, context binding and staged
  product activation/rollback.
- `runtime_composition`: plugin selection and registration order, cross-plugin wiring, memory
  preparation and default/extension tool activation policy.
- `configuration`: settings and explicit-selection adapters into domain-owned options; no
  resource discovery or plugin activation.
- `runtime_inventory`: labels for resolved JavaScript sources and loaded configured native plugins.

Runtime and session registration share one borrowed `GenerationComponents` view of already-prepared
native/JavaScript/MCP/memory/subagent components. It carries no credentials, configuration or
activation state. Runtime capabilities and overlays remain explicit inputs; built-in provider
preparation owns the shared transport and keeps test credential overrides out of the runtime seam.

These modules are private. `Pi`, `Config`, `ProductSessionFactory` and managed-session entry points
remain unchanged. Configuration/registration tests live beside their owners; factory tests focus
on lifecycle, trust and transactional preparation.

## Runtime features

`Config::features` selects first-party plugins before a runtime generation is built.
All flags default to `true`, preserving the standard product. For example:

```rust
use pi_sdk::{Config, Features, Pi};

let mut config = Config::new(cwd, agent_dir);
config.features = Features {
    memory: true,
    skills: true,
    ..Features::none()
};
let pi = Pi::builder(config).build()?;
```

The available flags are `memory`, `subagents`, `schedule`, `skills`,
`prompt_templates`, and `session_transfer` (export/import/share commands).
`Features::all()` is equivalent to `Features::default()`; `Features::none()` disables
these six features while retaining providers, filesystem/shell tools, core session
management, and independently configured native/JavaScript/MCP plugins.

Disabling a feature omits its built-in registration and corresponding session hooks,
not just its tools. Disabled memory does not load `memory.json` or start memory work;
disabled scheduling does not start its scheduler. Skills remain usable without
subagents or memory. Feature-specific settings still apply when enabled.
The host captures this selection and reuses it for new/resumed/forked sessions and
reloads; it is not a live mutable registry or a persisted session setting.

These are **runtime flags, not Cargo features**: dependencies are still compiled.
They select first-party composition, not a security boundary for explicitly loaded
plugins. Existing extension/MCP configuration remains independent. Read-only Desktop
skill-file management also remains available independently of session features.
Disabling `skills` or `prompt_templates` removes the corresponding built-in plugin
entirely, including explicit resource paths; this is a deliberate Rust SDK policy,
not Pi's automatic-discovery-only `noSkills`/`noPromptTemplates` behavior.
AGENTS.md/CLAUDE.md and general system-prompt resources are unaffected.

The crate is currently workspace-internal (`publish = false`). `pi-plugin-sdk`
is the separate authoring interface for native plugins.
