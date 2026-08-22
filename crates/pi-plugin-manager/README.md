# pi-plugin-manager

Native plugin distribution and installation module for `pi_rs`. It is deliberately separate from
`pi-plugin-loader`: the manager resolves and materializes trusted local packages, while the loader
only validates and loads local native code into a runtime generation.

原生插件的分发与安装模块。它与 `pi-plugin-loader` 分离：manager 负责解析、下载、校验和
物化本地 package，loader 只负责校验并把本地 native code 装入 runtime generation。

## Commands / 命令

```bash
# Local development package
pi plugin install ./path/to/package

# Project-local installation (requires project trust)
pi plugin install -l ./path/to/package --approve

# A release manifest hosted as a GitHub Release asset or any HTTPS endpoint
pi plugin install https://example.com/frontend-check/pi-plugin-release.json
pi plugin install github:owner/frontend-check@v1.2.0

# A package selected from a static registry index
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json

pi plugin list
pi plugin list -l
pi plugin sync --registry https://plugins.example/index.json
pi plugin remove frontend-check
```

`PI_PLUGIN_REGISTRY` supplies the registry URL for `install` and `sync`. Global state lives under
`<agent-dir>`; `-l` uses `<cwd>/.pi`.

## Consumer state / 使用者状态

`plugins.json` is editable intent and its array order is the native plugin registration order.
`plugins.lock` is the exact target-specific resolution and durable installation record; there is no
second mutable installation database. It records an `intent_sha256` digest so normal startup can
skip resolution and network access when intent is unchanged.

Package manifest `options` provide defaults. A root plugin's `plugins.json` options recursively
override those defaults, so `{}` keeps the package configuration instead of erasing it.

```json
{
  "schema": 1,
  "plugins": [
    {
      "id": "frontend-check",
      "source": "registry:frontend-check",
      "version": "^1",
      "options": {}
    }
  ]
}
```

```json
{
  "schema": 1,
  "target": "aarch64-apple-darwin",
  "intent_sha256": "...",
  "plugins": [
    {
      "id": "frontend-check",
      "version": "1.2.0",
      "kind": "agent",
      "source": "https://plugins.example/frontend-check-1.2.json",
      "target": "aarch64-apple-darwin",
      "sha256": "...",
      "artifact": "libfrontend_check.dylib",
      "options": {}
    }
  ]
}
```

Artifacts are immutable single-file CAS blobs at `plugins/store/sha256/<digest>`, so the address
depends only on bytes rather than a publisher-selected file name. A generated
`plugins/installed/<order>-<id>` view preserves the `plugins.json` declaration order for the current
directory-scanning loader. Transaction staging directories are hidden and ignored by discovery.
Native package manifests intentionally have no runtime plugin `dependencies`: normal Rust crate
dependencies are resolved at build time, while runtime hook order remains explicit consumer policy.

## Automatic reconcile / 自动同步

The CLI reconciles global intent and trusted project intent during initial session preparation and
again on `/reload`. Existing lock versions are preferred whenever they still satisfy the declared
constraints, so editing `plugins.json` does not silently upgrade packages. Local package roots are
checked even when intent is unchanged, allowing a rebuilt development artifact or changed package
option defaults to be picked up by `/reload`.

Reconciliation is prepared transactionally: the new lock and activation view are visible to native
discovery, but the previous state is retained until the complete runtime/session generation has
initialized. A loading or initialization failure restores the previous package state. Explicit
`pi plugin sync` forces resolution and materialization while still preserving locked versions; a
future update command will own intentional version upgrades.

CLI 会在初始 session 准备阶段以及 `/reload` 时自动同步全局 intent 和可信项目 intent。
只要已有 lock 版本仍满足约束，就继续复用该版本；因此修改 `plugins.json` 不会隐式升级
package。即使 intent 未变化，本地 package 的重编译 artifact 和 manifest 默认 options 也会
被检测。新的 lock/activation 只有在完整 runtime/session generation 初始化成功后才提交，
加载失败会恢复原状态。

## Release manifest / 发布清单

A remote source is a small JSON release manifest. Artifact URLs may be absolute or relative to the
manifest URL. The manager selects the entry matching its exact Rust target triple.

```json
{
  "schema": 1,
  "id": "frontend-check",
  "version": "1.2.0",
  "kind": "agent",
  "options": {},
  "artifacts": [
    {
      "target": "aarch64-apple-darwin",
      "url": "libfrontend_check.dylib",
      "sha256": "..."
    },
    {
      "target": "x86_64-unknown-linux-gnu",
      "url": "libfrontend_check.so",
      "sha256": "..."
    }
  ]
}
```

## Static registry / 静态 Registry

The first registry adapter needs only a static JSON file, so GitHub Pages, object storage, or an
ordinary HTTP server can host it:

```json
{
  "schema": 1,
  "plugins": {
    "frontend-check": [
      {
        "version": "1.2.0",
        "manifest": "releases/frontend-check-1.2.0.json"
      }
    ]
  }
}
```

The client selects the highest semver matching a requested constraint, verifies the selected
artifact SHA-256, and writes an exact lock. SHA-256 protects integrity but does not prove publisher
identity. Signed release metadata, Git repository sources, OCI artifacts, update, rollback, and
store garbage collection remain later distribution milestones.
