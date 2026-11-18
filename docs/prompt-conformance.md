# Prompt 与内置工具的 Pi 对齐

## 对齐目标

本轮以本地 `legacy/pi` 的 `05fd3fbda`（当前 Pi 1.5.2）为行为 oracle，修复
[原始 prompt 比较](research/prompt-legacy-comparison.md)和
[原始工具比较](tool-prompt-comparison-notes.md)中识别出的差异。原比较是修改前的历史证据，
不再作为当前缺口清单。

默认 Coding 行为跟随 Pi。hashline、memory、subagents、schedule 是显式可选 Rust 扩展，
默认关闭；多根 Workspace 的上下文说明仍是有意的 Rust 产品扩展。没有删除这些功能。

## 固定提示与 schema 的防漂移验证

`scripts/sync-prompt-conformance.mjs` 直接导入并执行本地 TypeScript 源码，不调用工具执行器或
provider，也不读取用户会话、凭证或项目配置。它生成
`domains/coding/tests/fixtures/prompt-conformance.json`，包含：

- 10 个工具的 description、parameters、outputSchema、constrainedSampling、promptSnippet、
  promptGuidelines：read/bash/powershell/edit/write/grep/find/ls/codemode/tool_search。
- 16 个主 prompt 场景：默认工具、全部工具、只读、两种 shell、skills、隐藏 reader、无 reader、
  project context、append/custom/forced prompt、自定义 sections 和规则去重。
- 3 个普通 MCP 结果 schema 场景，包含远端递归引用的重新定位；资源工具另有结构化输出集成测试。

只有发行物文档路径被归一化为 `/pi`。Rust 的 `prompt::oracle_tests` 对完整结果作精确比较，
而不是用 Rust 实现重新计算“期望值”。普通 Cargo 测试不要求 Node 或 legacy 依赖。

重新生成与检查：

```sh
node --import ./legacy/pi/packages/coding-agent/src/experimental/source-resolver.ts scripts/sync-prompt-conformance.mjs
node --import ./legacy/pi/packages/coding-agent/src/experimental/source-resolver.ts scripts/sync-prompt-conformance.mjs --check
cargo test -p pi-coding --lib prompt::oracle_tests
```

生成命令要求安装 legacy 的本地依赖；升级 oracle 后必须审阅 fixture diff，不能盲目覆盖。

## 本轮实现范围

| 范围 | 对齐内容 | 拥有模块 |
| --- | --- | --- |
| 模板与摘要 | 默认值占位符、空/转义/未闭合引号、turn-prefix/branch 文案、摘要内容组装 | pi-plugin-prompts、CodingCompactionPolicy、pi-session compaction |
| MCP | 按配置 exposure 生成服务器目录、普通/资源工具的完整 output schema、远端 schema 引用、默认注册但不激活 tool_search | pi-plugin-mcp、pi-plugin-tool-search、Coding composition |
| edit/write | 原文保留、换行、模糊唯一匹配、旧参数规范化、成功/错误信息、共享文件 mutation queue、Pi 路径与写入语义 | pi-plugin-edit/write、pi-tool-support |
| grep/find/ls | N+1 截断证据、原始搜索上下文、UTF-16 行截断、搜索根锚定、smart case、ICU 排序 | pi-plugin-grep/find/ls |
| read/image | 当前模型 resize profile、PNG/JPEG 和质量选择、坐标/转换说明、图片省略文本 | pi-plugin-read、pi-media、pi-core/Models metadata |
| bash/powershell | 无输出失败文案、活动后代输出、退出后 idle grace、进程树终止、超时前 shell 真实退出诊断 | pi-shell、pi-plugin-bash/powershell |
| 结果 hooks | 内容改写使旧结构化结果失效；显式结构化结果替换按序生效，codemode 不再读取旧值 | pi-plugin、pi-js-plugin、MCP result projection |

模型 `inputLimits` 是可选强类型元数据，兼容旧配置与序列化值。当前 read 工具在调用时查询模型，
所以同一 generation 内切模型也能采用新 profile。空 resize profile 明确选用 Pi 默认值，
而不是与工具 fallback profile 合并。模型目录同步仅补传这些元数据。

shell 的正常退出和输出流结束分别观察：shell 退出后，每个输出块重新开始 100ms idle grace；
持续输出仍受原 timeout/abort 约束。Unix 使用独立进程组，macOS 先 stop 再 kill，Windows 使用
System32 taskkill。成功结束不杀掉刻意启动的静默 daemon；未完成 future 被取消/丢弃会清理其进程与读任务。
`ShellResult.shell_exit_after` 只保留超时前已退出的 shell 时间，不改变持久化 session 格式。

## 验证状态

最终按完整工作区层级验证通过（macOS aarch64 / Rust 1.98.0）：

- `cargo test --workspace --no-fail-fast`：1762 passed，2 ignored，0 failed。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`、`git diff --check`：通过。
- Oracle fixture `--check`：10 tools / 16 prompts / 3 MCP schemas 与当前 legacy 一致；对应 Rust 测试通过。
- `npm --prefix packages/pi run check` 与 `npm --prefix packages/pi test`：通过，128 个 Node 测试通过。
- 重新 `build:native` 后执行 `test:native`：40 passed，2 skipped，0 failed。
- 新增/修复的定点回归均包含在上述门禁中；未使用真实模型凭证。

完整回归同时修复了旧默认值/旧 prompt 格式的测试和 eval treatment、RPC 的 Node 默认信号处理器抢先退出、
以及 native fingerprint 未监控新增源码文件造成旧构建缓存的问题。没有放宽 ABI 检查或跳过失败用例。

上述门禁对应基于 `2863458` 的完整工作树快照（包括当时已有的未提交基线），
不能自动视作后续某个局部提交单独通过验证。本地提交和归属隔离状态以 Git 记录及交接说明为准。
已构建本地 Node/NAPI 测试产物；没有发布、全局安装或重启用户会话，不能视作用户运行版本已更新。

## 本地交付记录

- `6142bb1`：git_commit 按行归属隔离的安全部分提交。
- `7e91781`：仅补交初始干净或全新、可证明全归本轮的文件。
- `88e16a424c2e20e84c18b600ebeca5aec7e352e0`：外部 person 提交纳入其余基线，保留原作者；
  经逐文件校验，源码字节与此前保存的完整工作树快照相同。协调者已接受它为完整源码交接点。
- 后续仅收口五份本轮报告及其精确 `.gitignore` 白名单，其他 docs 仍忽略；新的 RPC 对齐是独立任务。

在仓库根目录使用本轮已构建产物的入口：

```sh
PI_RS_NATIVE_BINDING="$PWD/target/aarch64-apple-darwin/debug/libpi_napi.dylib" \
  node packages/pi/dist/bin/pi.js --mode rpc --no-session --no-extensions
```

最小确定性 smoke 使用上述入口，另设临时 HOME/cwd/agent-dir、关闭项目信任、配置只指向 localhost:9 的假 provider；
只发送 get_state、get_available_thinking_levels 和一个未知命令，核查成功响应、错误 id 关联及 stdin EOF 后 exit 0。
没有发送 prompt 或调用模型，没有使用真实凭证。Sikemux 执行记录：`839c4a7e-0e26-4a44-8449-74dfe140866e`。
本机恢复快照及 smoke 结果保存在 `target/prompt-tools-ownership-_g0n2vn6/`，不作为用户安装更新的证明。

## 不作过度承诺

- 这里证明的是上述模型可见声明、prompt 场景和已覆盖的工具契约，不是整个 Pi 产品的全功能认证。
- 外部 MCP/JS/native 工具内容由用户实际安装的插件决定；没有对任意第三方工具作一致性声明。
- Rust 编码器与 Photon 的二进制输出/逐像素舍入不保证相同；对齐的是策略、尺寸/大小约束及模型可见提示。
- Rust 文本 read 仍可全量读取后截断，搜索仍可使用本地 Rust 实现，而不是要求安装/下载 rg、fd。
  这些实现方式不必为了 TypeScript 相同而替换，资源开销和依赖安装错误也不声称相同。
- ICU/Node 版本和机器 locale 可能影响边界排序；本轮以当前 oracle 的默认 locale 样本验证。
- `maxRequestBytes` 和图片数量限制元数据的保留不自动等同于所有 provider 已实现请求侧 enforcement。
- 本轮未重做完整分支历史投影/预算选取，也未新增原始 `fromHook` 标记区分；摘要文案/输入组装通过，不等于这些 session 历史语义也已获得完整兼容认证。
- 未调用真实付费模型；Windows/macOS/Linux 的全部运行态组合仍需各平台 CI。
