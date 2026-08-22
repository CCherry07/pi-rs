# pi_rs Node host / Node 宿主

`@pi-rs/cli` is the Node launcher for Pi-compatible JavaScript and TypeScript extensions. Node owns
extension discovery, `jiti` module loading, and callback execution; the NAPI-RS addon runs the same
Rust `AgentSessionRuntime`, providers, tools, session store, and Ratatui UI as `pi-cli`.

`@pi-rs/cli` 是 Pi 兼容 JavaScript/TypeScript extension 的 Node 启动层。Node 负责发现、
`jiti` 加载和执行回调；NAPI-RS addon 内仍运行与 `pi-cli` 相同的 Rust
`AgentSessionRuntime`、Provider、工具、会话存储与 Ratatui UI。

## Develop / 开发运行

Node 20+ and Rust 1.98+ are required. The repository pins Rust 1.98.0 through its root
`rust-toolchain.toml`.

需要 Node 20+ 和 Rust 1.98+；仓库通过根目录的 `rust-toolchain.toml` 固定 Rust 1.98.0。

The Node host is authored in TypeScript. Development commands run the `.ts` sources through `tsx`;
`npm run build` emits the publishable JavaScript and declarations under `dist/`.

Node 宿主使用 TypeScript 编写。开发命令通过 `tsx` 直接运行 `.ts` 源码；`npm run build` 会把
用于发布的 JavaScript 和声明文件生成到 `dist/`。

```bash
cd packages/pi
npm install
npm run check
npm run build
npm run build:native

# Fullscreen TUI, with automatic extension discovery
npm start -- --cwd /path/to/project

# Load an exact extension and disable discovery
npm start -- --no-extensions -e /path/to/extension.ts

# One-shot mode
npm start -- --print "summarize this repository"
```

Development builds create `pi-napi.<platform>-<arch>.node` in this package. A release artifact can
be built with `npm run build:native:release`. `PI_RS_NATIVE_BINDING` may point at an exact `.node`,
`.dylib`, `.so`, or `.dll` during development.

开发构建会在本目录生成 `pi-napi.<platform>-<arch>.node`；`npm run build:native:release`
生成 release binding。开发时也可以用 `PI_RS_NATIVE_BINDING` 指向明确的 `.node`、`.dylib`、
`.so` 或 `.dll`。

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
