# pi-plugin-manager

Host-side plugin loading, installation and authoring in one crate. Plugin authors depend on
`pi-plugin`; runtime generation builders use this outer host layer.

| Module / feature | Responsibility |
| --- | --- |
| `loader` | Local discovery, native ABI/fingerprint validation, process-pinned libraries and generation factories |
| `install` | Download, integrity verification, plugin intent/lock files, installation and transactional reconciliation |
| `authoring` | Scaffolding, Cargo builds, packaging, release verification and publication |

The default features are `loader` and `install`. Enable `authoring` in CLI/tooling consumers;
it also enables loading and installation. Loader-only consumers can disable default features to
avoid the installer HTTP dependencies. Installation does not load executable plugin code.

```toml
pi-plugin-manager = { path = "../pi-plugin-manager", features = ["authoring"] }
```

```rust
use pi_plugin_manager::loader::{NativePluginLoader, NativePluginLoaderOptions};
use pi_plugin_manager::install::{PluginManager, PluginManagerOptions, InstallScope};
use pi_plugin_manager::authoring::{NewOptions, PackageOptions, new_plugin, package};
```

Read the [installation guide](docs/install.md), [authoring guide](docs/authoring.md), and
[native plugin API](../pi-plugin/docs/native.md). Loading retains the existing generation factory
seam, compatibility checks and process-long library lifetime. Installation and packaging preserve
their existing intent, lock, manifest, trust and transactional activation rules.
