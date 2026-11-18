# Plugins and extensions

### Pi JavaScript/TypeScript extensions

The npm launcher delegates discovery to the Rust `pi-js-package-manager`, using current Pi
PackageManager precedence:

1. repeated `-e` / `--extension` local, npm, or git sources;
2. trusted project `settings.json` extension entries;
3. trusted project `.pi/extensions` auto-discovery;
4. user `settings.json` extension entries;
5. user `extensions` auto-discovery;
6. extension resources from configured local, npm, or git packages.

Package manifests, package filters, ignore files, canonical-path deduplication, missing-package
installation, and `PI_OFFLINE` behavior follow the same discovery layer. `--no-extensions` keeps
explicit `-e` sources but disables settings, package, and automatic discovery.

The Node host receives only the resolved ordered paths; Jiti import and JavaScript callbacks remain
in Node while discovery and install policy remain in Rust.

Package configuration and managed npm/git installs use the same Rust PackageManager:

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

`install` and `remove` use user scope by default; `--local` selects trusted project scope. Exact npm
versions stay pinned during update, while npm ranges and unversioned packages update in their
managed scope. Git updates reconcile the configured ref or the checkout's upstream branch. Bare
`pi update` retains current Pi's self-update meaning; pi-rs does not yet implement self-update, so
use `--extensions` for all JavaScript packages.

```bash
# Installed npm launcher
pi --cwd /path/to/project
pi --no-extensions -e /path/to/extension.ts

# Development launcher
npm install --prefix packages/pi
./scripts/pi-dev --cwd /path/to/project
```

See [`packages/pi/README.md`](../../packages/pi/README.md) for the supported Pi extension API and
explicit compatibility gaps.

### Native Rust plugins

Author commands scaffold the plugin and provider kinds, build on the current native target, and generate
verified local/remote manifests:

~~~bash
pi plugin new hello --kind plugin
pi --cwd hello plugin package
pi plugin verify hello/dist
pi plugin merge bundles/mac bundles/linux --output release
pi plugin publish github --bundle release --repo OWNER/REPO --tag v0.1.0
~~~

Use --sdk /path/to/pi-rs with new for a local SDK checkout. Builds default to release; --debug
uses the dev profile and --locked requires the completed Cargo.lock. Outputs must not exist.
See [native author tools](../../crates/pi-plugin-manager/docs/authoring.md) for CI, registry metadata and
exact-build SDK compatibility. These commands do not require provider credentials.

Native plugins are version-locked dynamic libraries loaded from global manifests, trusted project
manifests, or explicit `--plugin` paths.

```bash
pi --plugin /path/to/plugin.dylib
pi plugin install /path/to/package
pi plugin install https://example.com/pi-plugin-release.json
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
pi plugin list
pi plugin sync
pi plugin remove <plugin-id>
```

Pass `-l` to plugin-management commands to operate on the trusted project's `.pi` configuration
instead of the global agent directory. See
[`crates/pi-plugin/docs/native.md`](../../crates/pi-plugin/docs/native.md) for the native author API and
[`crates/pi-plugin-manager/README.md`](../../crates/pi-plugin-manager/README.md) for package and
registry formats.


See [extension compatibility](extensions.md), [skills](skills.md), and [prompt templates](prompt-templates.md).
