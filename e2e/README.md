# Pi Rust end-to-end tests

## Deterministic full-agent E2E

Runs in the normal workspace test suite without network access:

```bash
cargo test -p pi-e2e --test full_agent -- --nocapture
```

It covers resources/system prompt, plugin lifecycle and modifying hooks, provider tool loops, real filesystem read/write tools, transcript ordering, idle settlement, and JSONL session persistence.
