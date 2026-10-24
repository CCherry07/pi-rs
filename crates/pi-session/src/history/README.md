# Session History

The internal `pi-session/src/history/` module owns durable session data. It uses foundational
values from `pi-core` and `pi-utils` without importing Runtime, Management or plugin contracts.
Its public values are exported through the `pi-session` crate root.

- `SessionStore`: validated append batches, JSONL persistence, incremental queries, branch/tree forks.
- `SessionDocument`: immutable replay/inspection values and selected-branch projection.
- Context projection: edits, compaction, custom and unknown messages, provider input conversion.
- Recovery reduction: reconstruct interrupted operations without executing tools or providers.
- Isolated provenance: captured context seeds and parent identity in existing custom entries.

`SessionStore` retains the Rust v4 header, four mutation kinds and shared sequence, lazy materialization
support, torn-tail repair, unknown message extensions, and tree/lane semantics. New product sessions
are still materialized by Session Runtime only after the first assistant `message_end`.
The current local TypeScript session manager uses v3; v1-v3 import belongs to the session-transfer
plugin, and this module does not import legacy formats or product policy.

```rust,ignore
use pi_session::{HistoryError, SessionStore};

fn inspect(path: &std::path::Path) -> Result<(), HistoryError> {
    let (store, _) = SessionStore::open(path)?;
    let context = store.context()?;
    println!("{} projected messages", context.messages.len());
    Ok(())
}
```

`pi_session::SessionStore` returns `HistoryError`. The runtime converts history failures into the
existing product `SessionError` variants. Live product
mutations must go through `AgentSession`, which owns activity admission, context activation and
semantic events; standalone history users can use the journal directly.

See [the architecture diagram](../../../../docs/architecture.md#session-modules) and
[Session Runtime / Management](../../README.md).
