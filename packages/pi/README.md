# pi_rs Node host / Node 宿主

`@pi-rs/cli` is the Node launcher for Pi-compatible JavaScript and TypeScript extensions. Node owns
extension discovery, `jiti` module loading, and callback execution; every CLI mode is delegated
through NAPI to the same Rust runtime. Interactive launches use the Ratatui frontend in `pi-cli`,
while print, JSON, plugin-management, and piped-input modes use their matching Rust paths.

`@pi-rs/cli` 是 Pi 兼容 JavaScript/TypeScript extension 的 Node 启动层。Node 负责 extension
发现、`jiti` 加载和回调执行；所有 CLI 模式都通过 NAPI 进入同一个 Rust runtime。交互模式使用
`pi-cli` 的 Ratatui 前端，print、JSON、插件管理和管道输入使用各自对应的 Rust 路径。

## Develop / 开发运行

Node 20+ and Rust 1.98+ are required. The repository pins Rust 1.98.0 through its root
`rust-toolchain.toml`. The Node host is authored in TypeScript; development commands use `tsx`, and
`npm run build` emits publishable JavaScript and declarations under `dist/`.

需要 Node 20+ 和 Rust 1.98+；仓库通过根目录的 `rust-toolchain.toml` 固定 Rust 1.98.0。Node
宿主使用 TypeScript 编写，开发命令通过 `tsx` 运行，`npm run build` 会把发布用 JavaScript 和
声明文件生成到 `dist/`。

```bash
cd packages/pi
npm install
npm run check
npm run build
npm run build:native

# Interactive Ratatui frontend with automatic extension discovery
npm start -- --cwd /path/to/project

# Load an exact extension and disable discovery
npm start -- --no-extensions -e /path/to/extension.ts

# One-shot mode
npm start -- --print "summarize this repository"
```

Development builds create `pi-napi.<platform>-<arch>-<abi>.node` in this package (macOS has no ABI
suffix). A release artifact can be built with `npm run build:native:release`.
`PI_RS_NATIVE_BINDING` may point at an exact `.node`, `.dylib`, `.so`, or `.dll` during development.

开发构建会在本目录生成带平台、架构和 Linux/Windows ABI 后缀的 `.node`；
`npm run build:native:release` 生成 release binding。开发时也可以用
`PI_RS_NATIVE_BINDING` 指向明确的 `.node`、`.dylib`、`.so` 或 `.dll`。

## Distribution / 发布

Published npm installations use a small JavaScript root package plus one exact-version native
optional package selected for the current OS, CPU, and Linux libc. The root package never embeds
all native binaries. Supported target definitions are shared by the loader and release tooling, so
an unsupported runtime fails with the exact missing platform package instead of a generic `dlopen`
error.

发布后的 npm 安装由一个轻量 JavaScript 根包和一个匹配当前 OS、CPU、Linux libc 的原生可选
包组成；根包不会携带全部平台二进制。loader 与发布工具共享同一份 target 定义，不支持的
runtime 会直接报告所需的平台包。

Release commands run through one Module:

```bash
# Validate Cargo/npm versions and target coverage
npm run release:check

# Build the standalone archive and NAPI artifact for this native host
npm run release:dist -- --target aarch64-apple-darwin

# After all CI target artifacts have been collected
npm run release:assemble -- --artifacts dist/release
npm run release:verify -- --npm-dir dist/npm

# Protected workflow only: publish the verified tarballs, then verify npm itself
npm run release:publish -- --npm-dir dist/npm
npm run release:verify-published -- --npm-dir dist/npm
```

`release:publish` publishes every platform package before `@pi-rs/cli`; it is intended only for the
protected release workflow. It publishes the `.tgz` files produced and smoke-tested by
`release:assemble`, not a newly packed directory. `release:verify-published` then requires every npm
package to have the expected identity, platform selectors, optional dependency matrix, and exact
tarball integrity. GitHub archives are the Rust-only delivery channel and do not provide a
JavaScript VM. The npm channel provides the Node host and JS/TS extension support.

Release Please maintains a Conventional-Commit release PR that updates Cargo/npm versions and the
changelog together. Before a locked native build, the Release Module synchronizes the generated
`Cargo.lock` entries for the two crates that inherit the product version. Merging the PR creates a
draft GitHub Release and explicitly dispatches the native release workflow; successful npm
publication and registry verification publish that draft.

The `@pi-rs` packages must configure npm Trusted Publishing for this repository,
`.github/workflows/release.yml`, and the protected `npm-publish` environment. The workflow uses only
the short-lived OIDC identity—no `NPM_TOKEN` or `NODE_AUTH_TOKEN` secret—and npm records provenance
automatically.

Release Please 会维护 Conventional Commit 驱动的 release PR，并一次性同步 Cargo/npm 版本与
changelog。合并后先创建 draft GitHub Release，再显式触发多平台构建；所有 npm 包发布并从
registry 校验成功后，GitHub Release 才会公开。npm 发布只使用 Trusted Publishing 的短期
OIDC 身份，不保存长期 npm token。

## Discovery and reload / 发现与重载

Load order follows current Pi: trusted `<cwd>/.pi/extensions`, `<agent-dir>/extensions`, then
repeated `-e/--extension` paths. Files may be `.ts`, `.js`, `.mts`, `.mjs`, `.cts`, or `.cjs`;
directories may expose `index.*` or `package.json#pi.extensions`. Project-local discovery is gated
by the shared project-trust service; explicit paths are an explicit user choice.

加载顺序对齐当前 Pi：可信项目的 `<cwd>/.pi/extensions`、`<agent-dir>/extensions`、最后是重复
传入的 `-e/--extension`。支持 TS/JS 文件、`index.*` 目录入口，以及
`package.json#pi.extensions`。项目自动发现受统一 Project Trust 控制；显式路径视为用户的
明确选择。

`/reload` creates a new Node callback generation with `jiti` module caching disabled, converts each
source into separate Rust `AgentPlugin`, `ProviderPlugin`, and `SessionPlugin` adapters, validates
the entire candidate, and only then swaps the active session generation. Failed loading or
registration leaves the old generation active. Retiring a generation aborts its active callbacks
and releases all retained JavaScript functions.

`/reload` 会关闭 `jiti` module cache，创建新的 Node callback generation；同一个源码会分别
物化为 Rust `AgentPlugin`、`ProviderPlugin`、`SessionPlugin` adapter。完整候选代验证成功后
才切换；失败继续使用旧代。旧代释放时会 abort 活跃回调并清空其 JavaScript function。

## Compatibility surface / 兼容范围

Supported today:

- `registerTool`, including schema, prompt snippet/guidelines, execution mode, cancellation, and
  final tool results;
- `registerCommand` (normal Pi `void` handlers become handled commands; pi_rs additionally accepts
  `{ action: "transform", text }`);
- agent hooks backed by the Rust contract: `input`, `before_agent_start`, `agent_start/end`,
  `turn_start/end`, `message_start/update/end`, `tool_execution_start/update/end`, `context`,
  `tool_call`, and `tool_result`;
- `before_provider_request` as the provider lifecycle hook;
- all ten Rust/Pi session hooks from `session_start` through `session_tree`;
- runtime-neutral imports `defineTool`, `CONFIG_DIR_NAME`, `VERSION`, `getAgentDir`, and the tool
  event type guards from both current and legacy Pi package names.

当前已支持工具、命令、Rust contract 已有的 agent hooks、Provider 的
`before_provider_request`、全部十个 session hooks，以及常用的纯函数 runtime imports。

Capabilities that need a richer product bridge fail explicitly instead of being ignored:
`registerProvider`, shortcuts/flags, UI dialogs and action methods, custom renderers/Markdown
transformers, resource/project-trust hooks, provider header/response hooks, model/thinking/bash
events, tool progress callbacks, `prepareArguments`, image replacement, and custom message
injection/replacement. JavaScript runs in-process with Node and is trusted code, not a sandbox.

需要更丰富产品桥接的能力会明确报错，而不是静默失效：JS Provider 注册、快捷键/flag、UI
与 action、定制 renderer、资源/Trust hooks、Provider header/response hooks、模型/思考/bash
事件、工具进度、`prepareArguments`、图片替换和自定义消息注入。JavaScript 与 Node 同进程，
属于可信代码，不是 sandbox。

## Tests / 测试

```bash
npm test                 # TypeScript host discovery, loading, manifest and callback lifecycle
npm run test:native      # real Node -> NAPI -> Rust session -> Node callback smoke test
```
