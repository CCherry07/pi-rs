# frontend-workflow native plugin

This project-local AgentPlugin registers `/frontend-check` and contributes verification guidance
to every agent run. Its behavior is configured by `pi-plugin.toml` rather than compiled into the
host application.

Build and test from this directory:

```bash
cargo test
cargo build
```

Or, from the `frontend-app` directory, build and install it into the project's managed plugin
activation view:

```bash
cargo build --manifest-path pi-plugins/frontend-workflow/Cargo.toml
../../../target/debug/pi --approve \
  plugin install --local pi-plugins/frontend-workflow
../../../target/debug/pi --approve
```

For explicit-path development without trusting other project resources:

```bash
../../../target/debug/pi --no-approve \
  --plugin pi-plugins/frontend-workflow/pi-plugin.toml
```

Type `/frontend-check accessibility` to transform the command into a focused review request. After
changing Rust code, rebuild the plugin and run `/reload` in an active session. The automatic
reconcile detects the rebuilt local artifact, snapshots it by content hash, and publishes it only
if the next generation initializes successfully. An explicit `pi plugin sync --local` is still
available when a forced package reconciliation is useful.

`review_paths` and `ignore_paths` in the manifest bound what the model may inspect for this command.
The default fixture scope excludes `.pi/`, so verification does not recursively review the native
plugin's own source code.

The checked-in manifest currently names the macOS debug artifact. On Linux use
`target/debug/libfrontend_workflow.so`; on Windows use `target/debug/frontend_workflow.dll`.
