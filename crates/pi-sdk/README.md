# pi-sdk

`pi-sdk` is the headless product integration layer for embedding Pi in the CLI,
desktop application, and other presentation adapters.

It owns the product runtime composition: providers and model catalogs, built-in
tools, skills, memory, subagents, settings, project trust, plugin generations,
and session construction. It does not own terminal, Tauri, RPC, or other
presentation behavior.

```rust
use pi_core::PresentationMode;
use pi_sdk::{Pi, Config};

let config = Config::new(cwd, agent_dir);
let pi = Pi::builder(config)
    .presentation_mode(PresentationMode::Rpc)
    .build()?;
let session = pi.sessions().create_session(cwd, session_path).await?;
```

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
