# pi-sdk

`pi-sdk` is the headless product integration layer for embedding Pi in the CLI,
desktop application, and other presentation adapters.

It owns the product runtime composition: providers and model catalogs, built-in
tools, skills, memory, subagents, settings, project trust, plugin generations,
and session construction. It does not own terminal, Tauri, RPC, or other
presentation behavior.

```rust
use pi_core::PresentationMode;
use pi_sdk::{Pi, ProductConfig};

let config = ProductConfig::new(cwd, agent_dir);
let pi = Pi::builder(config)
    .presentation_mode(PresentationMode::Rpc)
    .build()?;
let session = pi.sessions().create_session(cwd, session_path).await?;
```

The crate is currently workspace-internal (`publish = false`). `pi-plugin-sdk`
is the separate authoring interface for native plugins.
