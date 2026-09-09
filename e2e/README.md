# pi-rs runtime acceptance

This directory contains in-process Rust acceptance tests and a frontend example project. It does
not contain a black-box CLI or Node-launcher product suite.

## Run

```bash
cargo test -p pi-e2e --test runtime_agent -- --nocapture
```

`cargo test --workspace` includes this test. It uses a deterministic scripted provider and temporary
project/session directories; it does not need real provider credentials.

The acceptance test covers prompt assembly, project skills, plugin hooks, production read/write
tools, the agent tool loop, and Pi v4 session persistence. It bypasses CLI argument parsing and
`ProductSessionFactory`, so it does not prove the complete product launch path.

## Node/NAPI checks

The separate Node host and native bridge tests remain in `packages/pi/test`:

```bash
npm ci --prefix packages/pi
npm --prefix packages/pi run check
npm --prefix packages/pi test
npm --prefix packages/pi run build:native
npm --prefix packages/pi run test:native
```

CI runs the Rust acceptance through the workspace suite, Node host tests separately, and native
bridge smoke tests against a freshly built binding. Each bridge scenario starts the Node launcher
with an explicitly closed stdin pipe, a 15-second process deadline, isolated HOME/agent state,
and no inherited provider credentials. This prevents node:test's open worker stdin from blocking
the native CLI's piped-input reader. Temporary directories are removed after each test.
Release builds also run the native bridge tests. These checks do not replace the removed black-box
provider/tool-loop scenarios.

## Example project

`projects/frontend-app` is a React/TypeScript/Vite example with project prompt/skill resources,
a TypeScript extension, and a native Rust plugin. Runtime acceptance and Node host tests reuse its
resources. Use npm and the checked-in `package-lock.json` for the frontend; the unrelated copied
`.agents/skills` collection is not maintained as part of the fixture.

## Coverage limits

Full CLI/Node process-to-provider tool-loop coverage is no longer maintained here. Real-provider
compatibility checks and fullscreen TUI PTY tests are separate, currently unimplemented layers.
Ratatui layout/input tests remain focused tests, not end-to-end terminal coverage. Never put real
provider credentials in source, logs, or fixtures.
