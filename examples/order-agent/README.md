# Order agent

A minimal non-Coding domain built with the generic `pi-sdk` host. It composes:

- An explicit order-assistance system prompt and model selection.
- A unified `OrdersPlugin` registering one `lookup_order` tool.
- A factory-backed deterministic provider that requests the tool and completes the answer.
- The shared session manager, tool execution, provider continuation, and lazy JSONL persistence.

```sh
cargo run -p pi-order-agent-example
```

Expected output:

```text
Order ORD-42 has shipped. Tracking number: DEMO-0042.
```

The tool looks up an in-memory sample record. The provider's response is scripted so the example
is repeatable without credentials or network access; it demonstrates composition rather than
language-model reasoning. Replace the provider factory and order-service implementation to
connect a real application. The service owns its access checks and business rules.

Session storage uses a temporary directory removed when the example exits. An application
supplies a durable JSONL path and can reopen it through `host.sessions().open_session(path)`.
No Coding tools, project resources, settings, or credentials are discovered by the SDK.

SDK integration tests cover actual tool-result continuation, saved-session resume, independent
session capabilities, model/workspace retention, and successful and failed reloads:

```sh
cargo test -p pi-sdk --test domain_sessions
```
