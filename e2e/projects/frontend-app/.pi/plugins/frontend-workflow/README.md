# frontend-workflow native plugin

This project-local AgentPlugin registers `/frontend-check` and contributes verification guidance
to every agent run. Its behavior is configured by `pi-plugin.toml` rather than compiled into the
host application.

Build and test from this directory:

```bash
cargo test
cargo build
```

Then start `pi` from the `frontend-app` project and approve project resources. The plugin is found
automatically below `.pi/plugins`:

```bash
../../../target/debug/pi --approve
```

For explicit-path development without trusting other project resources:

```bash
../../../target/debug/pi --no-approve \
  --plugin .pi/plugins/frontend-workflow/pi-plugin.toml
```

Type `/frontend-check accessibility` to transform the command into a focused review request. After
changing Rust code, rebuild the plugin and run `/reload`; the host snapshots the new artifact by
content hash and publishes it only if the next generation initializes successfully.

`review_paths` and `ignore_paths` in the manifest bound what the model may inspect for this command.
The default fixture scope excludes `.pi/`, so verification does not recursively review the native
plugin's own source code.

The checked-in manifest currently names the macOS debug artifact. On Linux use
`target/debug/libfrontend_workflow.so`; on Windows use `target/debug/frontend_workflow.dll`.
