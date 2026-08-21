# pi-plugin-sdk

Author-facing interface for version-locked native `pi_rs` plugins.

## Create a plugin / 创建插件

One crate exports exactly one plugin kind. Build both `cdylib` for loading and `rlib` for tests:

```toml
[package]
name = "hello"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
pi-plugin-sdk = { path = "/path/to/pi_rs/crates/pi-plugin-sdk", features = ["agent"] }
```

The SDK is currently consumed from the workspace (or a pinned Git revision). Replace the path with
an exact published version once native package distribution is released; host and plugin must use
the same SDK build fingerprint.

无配置插件实现 `Default`，作者不需要写 native constructor 或 `id()`：

```rust
use pi_plugin_sdk::agent::prelude::*;

#[derive(Default)]
pub struct HelloPlugin;

#[pi_plugin_sdk::agent]
impl AgentPlugin for HelloPlugin {
    fn register(&self, context: &mut RegisterContext<'_>) -> Result<()> {
        // context.register_tool(...)
        Ok(())
    }
}
```

Use `#[pi_plugin_sdk::provider]` with the `provider` feature or
`#[pi_plugin_sdk::session]` with the `session` feature for the other lifecycles. The macro must
annotate the matching trait impl and a dynamic library may contain only one export macro.

## Fallible/configured construction / 配置与可失败初始化

Only configured plugins implement the additional factory interface:

```rust
use pi_plugin_sdk::provider::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Options {
    base_url: String,
}

struct AcmePlugin {
    options: Options,
}

impl NativePluginFactory for AcmePlugin {
    type Options = Options;

    fn load(
        _context: &PluginLoadContext,
        options: Self::Options,
    ) -> PluginLoadResult<Self> {
        Ok(Self { options })
    }
}

#[pi_plugin_sdk::provider(factory)]
impl ProviderPlugin for AcmePlugin {}
```

`PluginLoadContext` exposes the active cwd, immutable package directory, persistent per-plugin data
directory, disposable cache directory, load scope, and generation. It deliberately omits terminal
state, mutable registries, sessions, provider credentials, and product configuration.

## Local loading / 本地加载

Build the crate, then pass the dynamic library directly while developing:

```bash
cargo build
cargo run -p pi-cli -- --plugin target/debug/libhello.dylib
```

`--plugin` may be repeated and also accepts a `pi-plugin.toml` path or its containing directory.
The platform suffix is `.so` on Linux and `.dll` on Windows.

Installed global manifests are discovered below `<agent-dir>/plugins`. Trusted project manifests
are discovered below `<project>/.pi/plugins`; an untrusted project plugin file is never opened.

```toml
schema = 1

[plugin]
id = "hello"
version = "0.1.0"
kind = "agent"
artifact = "libhello.dylib"

[options]
# Plugin-specific typed options
```

The manifest identity must match the binary descriptor. Artifacts must remain inside their package
directory. Duplicate IDs fail within each plugin kind.

## Compatibility and reload / 兼容与重载

The export macro emits a C-layout descriptor containing ABI version, plugin kind, identity,
version, and a fingerprint derived from the exact SDK version, Rust compiler, and target. The host
checks that descriptor before resolving a Rust-ABI trait-object constructor. Native libraries are
trusted in-process code and are intentionally never unloaded during the process lifetime. Before
loading, the host snapshots each artifact below `<agent-dir>/cache/plugins/artifacts/<sha256>`;
unchanged content reuses one pinned handle, while a rebuilt artifact gets a new load path on reload.

Every factory call creates a fresh instance. Runtime and session reload continue to use the existing
fallible generation factories: a constructor, registration, or validation failure leaves the active
generation unchanged.

Remote publishing, signatures, content-addressed package installation, and registry commands are a
separate distribution milestone; this crate currently provides the author interface and
local/package loading contract.
