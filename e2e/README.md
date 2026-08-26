# pi-rs end-to-end validation

The E2E architecture separates fast runtime acceptance from black-box product testing. A test is
called a product E2E only when it starts a real product adapter and observes public output, provider
traffic, filesystem effects, or persisted session data.

## One command

Install the Node dependencies once, then run the complete deterministic stack:

```bash
npm ci --prefix packages/pi
npm --prefix packages/pi run e2e
```

The command builds the host NAPI binding and standalone CLI, then runs the runtime acceptance,
Node/NAPI bridge checks, and both black-box product adapters. Scenario execution never contacts a
public provider or reads a real provider credential.

## Layers

| Layer | Command | Purpose |
| --- | --- | --- |
| Runtime acceptance | `cargo test -p pi-e2e --test runtime_agent -- --nocapture` | Fast in-process coverage of prompt assembly, plugins, tool loops, real filesystem tools, settlement, and Pi v4 persistence |
| Node/NAPI bridge | `npm --prefix packages/pi run test:bridge` | Focused callback-generation and native-context integration checks |
| Product E2E | `npm --prefix packages/pi run test:e2e` | Starts the real standalone CLI and Node/NAPI launcher against a deterministic local provider |
| Complete deterministic stack | `npm --prefix packages/pi run e2e` | Builds required artifacts and runs every layer above |

`cargo test --workspace` includes the runtime acceptance layer. It does not build the host NAPI
artifact or start both product adapters, so it is not a substitute for the complete command.

## Product harness

Product scenarios use the single `runProductScenario(...)` Interface in
[`product/harness.ts`](product/harness.ts). A scenario declares only:

- the `native-cli` or `node-napi` adapter;
- the submitted input;
- deterministic provider turns;
- an optional project fixture and explicit JavaScript extensions.

The harness hides the local OpenAI-compatible SSE server, process invocation, 30-second timeout,
temporary HOME/agent/session directories, credential removal, offline mode, NDJSON decoding, and
cleanup. It rejects unused provider turns and extra provider requests, so a silently shortened or
extended agent loop fails the scenario.

The two maintained product scenarios prove different seams:

- `native-cli.test.ts` starts the standalone Rust binary and verifies NDJSON output, provider
  projection, and lazy session persistence.
- `node-napi.test.ts` starts the compiled Node launcher, loads the frontend TypeScript extension,
  executes its registered tool through NAPI/Rust, observes the tool result in the next provider
  request, and verifies the final persisted session.

Tests assert only observable evidence returned by the harness. Runtime registries, private plugin
state, and `ProductSessionFactory` internals are intentionally not part of the product E2E
Interface.

## Adding a scenario

Add a `*.test.ts` file under `product/` and call `runProductScenario`. Prefer extending the
`ProviderTurn` vocabulary only when a new wire-level behavior is genuinely required. Do not add
assertion syntax to the harness; ordinary test code keeps scenario-specific expectations local.

Use the fixture under `projects/frontend-app` for project resources and extensions. The harness
passes explicit paths and an isolated agent directory, so scenarios must not depend on a
developer's global Pi configuration.

## Opt-in and terminal coverage

Real-provider tests are intentionally outside the deterministic stack. The repository currently
has no maintained live-provider runner; when added, it must be opt-in through provider-specific
environment variables, tolerate provider nondeterminism, and never write credentials to fixtures,
logs, or artifacts.

Fullscreen TUI input still needs a separate PTY adapter. Ratatui layout tests remain focused tests;
they are not represented as product E2E coverage here.
