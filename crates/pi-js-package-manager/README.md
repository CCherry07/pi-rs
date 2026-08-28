# pi-js-package-manager

`pi-js-package-manager` is the Rust discovery and package-management Module for Pi-compatible
JavaScript and TypeScript extensions. Its public Interface is deliberately small:

```rust
PackageManager::new(request).resolve().await -> Result<Resolution, PackageManagerError>
PackageManager::new(request).manage(operation).await -> Result<ManageResult, PackageManagerError>
```

The request contains cwd, agent directory, the Rust-owned project trust decision, explicit CLI
sources, and the automatic-discovery switch. The result is one ordered, canonical-path-deduplicated
extension load list plus its deduplicated source identities. Package entries retain the effective
source string from `settings.json`; direct and automatically discovered entries retain their path.
Callers therefore do not need to recover package names from installation paths.

`manage` accepts one of four typed operations: install, remove, update, or list. It owns user versus
project scope, project-trust enforcement, physical npm/git changes, settings persistence, pinned
npm behavior, git ref/branch reconciliation, and installed-path reporting. Settings writes merge
only the `packages` field into the latest file so unrelated and future settings survive.

Behind that Interface the crate owns settings parsing, project/user/temporary source scopes,
PackageManager precedence, `pi.extensions` manifests, filters and autoload deltas, ignore files,
local/npm/git resolution, managed install layouts, custom `npmCommand`, semver checks, and
`PI_OFFLINE`. It has no Node, Jiti, NAPI, terminal, or plugin-lifecycle dependency. The CLI remains
an Adapter: it maps Pi's command shape into `ManageOperation` and renders `ManageResult`.

`apps/pi-cli::ProductSessionFactory` is the production Adapter. It resolves trust, calls this crate
while preparing a candidate session generation, and passes only `extensionPaths` to the Node host.
Node remains the JavaScript VM and owns Jiti import and callback lifetimes.
