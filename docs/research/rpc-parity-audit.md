# RPC 兼容性只读核查

## 结论

**不是完全对齐。33/33 个 RPC 命令名称已有对应实现，但字段、事件交付和扩展行为存在可证实差异。**

审计基于 2026-11-18 当前工作树，pi-rs HEAD `2863458`、本地 legacy Pi HEAD `05fd3fbda`。
包含原有未提交代码；此次只新增审计记录，未修改实现、未提交、未重跑完整门禁。
不存在 `.gitnexus` 索引，因此采用源码搜索，没有生成/覆盖 AGENTS.md。

命令级逐项矩阵见 [RPC command audit](rpc-command-parity-audit.md)。
上轮 [prompt/tool 对齐记录](../prompt-conformance.md)不是完整 RPC 等价性证明。

## 最重要的五类已证实差异

### 1. 不能直接用 switch_session 打开 legacy 原生会话

- Pi RPC：`legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts:624-627` →
  `core/agent-session-runtime.ts:215-242` → `SessionManager.open`，接受其原生 v3 `type: session` 文件。
- Rust：`crates/pi-rpc/src/rpc.rs:751-757` → `PiSession::resume_session` → `SessionTarget::Open` →
  `SessionStore::open_handle`（`crates/pi-session/src/management/replacement.rs:409-412,604-617`；
  `crates/pi-session/src/history/journal/jsonl.rs:133-136`）。
- Rust header 校验在同文件 `1185-1194` 要求 `kind: header` 和 v4；没有自动 import 分支。
  `/import` 是另外的确认/转换流程，不能算 switch_session 本身兼容。

**用户影响：** 直接替换现有 Pi 子进程后，客户端保存的旧 sessionFile 不能原样复用。

本地已构建 Node/NAPI 入口的最小隔离 smoke 确認：get_state 成功；对 v3 header 文件调用 switch_session 返回：

```json
{"id":"switch","type":"response","command":"switch_session","success":false,"error":"invalid session JSON at line 1: is not a header"}
```

随后关闭 stdin，进程 exit 0。此验证没有发送 prompt 或模型请求。

### 2. RPC 投影丢失部分真实响应/事件字段

| 字段/场景 | 当前 Pi | 当前 Rust | 客户端影响 |
| --- | --- | --- | --- |
| `tool_execution_end.durationMs` | `legacy/pi/packages/agent/src/agent-loop.ts:922-929` 发送实际执行时间；`coding-agent/src/modes/json-event.ts:47-49` 非 message_update 原样传递 | `crates/pi-rpc/src/json_wire.rs:391-402` 不发送该字段 | 工具耗时展示、性能统计缺失 |
| `tool_execution_end.result.structuredContent` | 同一 emit 发送完整 finalized.result；read/MCP 等工具实际产生此字段 | `json_wire.rs:535-552` 的 tool_result_json 白名单没有 structured_content | 依赖结构化结果的 RPC 客户端只能退回解析展示文本；这与 codemode 内部已支持结构化结果是两回事 |
| `compact` 成功数据 / `compaction_end.result.estimatedTokensAfter` | `legacy/pi/packages/coding-agent/src/core/agent-session.ts:3024,3041-3048,3350,3367-3374` 计算并填入 | `json_wire.rs:286-305` 的 compaction_result_json 未填入；命令和事件都复用它 | 压缩后上下文预算信息缺失 |

本次直接执行了 legacy `toJsonEvent`，输入带 durationMs 和 structuredContent 的工具结束事件，输出原样保留两个字段。
Rust 端缺失依据当前投影函数的完整字段列表，不冒充已经做了双方工具执行的端到端差分。

另有命令审计确认：模型响应无法保留 `promptCache` / `samplingParamsByThinkingLevel`；
get_entries/get_tree 的 compaction 投影没有 legacy 的 fromHook/systemMessage。详见命令报告。

### 3. 慢客户端下的事件交付策略不同

- Pi RPC 在 `legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts:324-337` 订阅 session 事件，
  并给 agent 事件增加 `await waitForRawStdoutBackpressure()`，让输出压力回传到事件生产。
- Rust RPC `crates/pi-rpc/src/rpc.rs:392-446` 的 forward_events 等待 output.emit。
  `crates/pi-rpc/src/output.rs:171-184` 确实等待写入完成，能保留一个记录的完整字节。
- 但上游 session event hub 是 512 容量 broadcast，发布不等待读取：
  `crates/pi-session/src/runtime/events.rs:11,191,500-514`。
- reader 落后会重建状态而不是重放漏掉的事件：
  `crates/pi-session/src/management/observation.rs:19-23,144-145`。
  RPC 把它转换成 `extension_error`，要求重新 get_state/get_messages：`rpc.rs:424-435`。

**用户影响：** 客户端停止/慢速读取 stdout 时，Rust 可丢失中间 delta 或瞬时事件并要求补拉状态；
不能把“字节写入支持 backpressure”说成“与 Pi 相同的完整事件流交付”。
本次确认的是代码策略与明确的 lag 分支，没有进行新的压力测试，也未声称某个固定速率必然丢事件。

### 4. 扩展 hook 异常没有相同的 RPC 错误事件路径

- Pi RPC 在 `legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts:370-372` 将扩展错误发送为
  `extension_error {extensionPath,event,error}`。
- Rust/JS hook 失败路径调用 report_hook_error（例如 `crates/pi-js-plugin/src/lib.rs` 的 tool_result
  分发；`crates/pi-plugin/src/plugin/agent.rs:75-77`），写入
  `crates/pi-plugin/src/plugin/diagnostics.rs:38-69` 的内存诊断列表。
- 当前 RPC forward_events 没有读取/订阅这份列表。它的 extension_error 分支仅处理投影失败和订阅 lag
  (`crates/pi-rpc/src/rpc.rs:413-435`)；普通 PluginNotice 则变为 UI notify (`407-411`)。

**用户影响：** 依赖 extension_error 定位坏扩展的客户端，无法收到相同的路径/hook/错误结构。
不把用户主动 notify 与真正 hook 异常混为一谈，也不声称所有失败都完全静默：命令错误、工具结果或诊断仍可能呈现失败。

### 5. RPC 扩展 UI 的降级 API 不是完全同形

正常 dialog/notification wire 大量已对齐，但扩展代码调用的 UI facade 仍有差异：

- Pi RPC `rpc-mode.ts:169-172` 提供 onTerminalInput 的 no-op unsubscribe；
  `:286-310` 提供 getAllThemes/getTheme/setTheme 等明确降级结果。
- Rust Node host `packages/pi/src/extension-context.ts:218-221` 在 hasUI 时直接返回
  ExtensionUI.forContext；`packages/pi/src/extension-ui.ts` 没有 getAllThemes、getTheme 或 onTerminalInput 方法。
- 其 `setTheme` (`extension-ui.ts:549-553`) 只接受 light/dark/system，RPC 时命令被抑制，返回 undefined；
  Pi 返回 `{success:false,error:"Theme switching not supported in RPC mode"}`。

本次用真实 ExtensionUI 源码和无 I/O 的 RPC context stub 核查到：

```json
{"getAllThemes":"undefined","getTheme":"undefined","onTerminalInput":"undefined","setThemeResult":"<undefined>","editorText":"","toolsExpanded":false}
```

**用户影响：** 在 Pi RPC 下安全 no-op 的扩展代码，迁移到此 Node host 后可能 TypeError 或拿到不同返回值。
editorText/ToolsExpanded 的默认值则与 Pi 相同，不能一并误报。

## 已对齐范围和未验证矩阵

| 方面 | 已有对应实现/证据 | 不能推出的结论 |
| --- | --- | --- |
| 命令 | 33/33 名称匹配；通常的 string id、无 id 省略、未知命令关联、no-data/null 响应分支有对应实现 | 不是所有参数边界、错误文案和 downstream 语义等价；serde 与 JS cast 的非法输入行为不同 |
| 传输 | JSONL/LF、CRLF、最终无换行记录、Unicode 分隔符保留；一个共享输出 writer；Node RPC 日志转 stderr | 不是完整事件流无丢失；慢客户端策略见差异 3 |
| 生命周期 | started/queued/handled、agent_settled、abort 等待 idle、steer/follow_up、clear_queue、EOF/Unix 信号处理都有自测 | 未全面双端重放并发 prompt/切换/abort/queue 与不同 hook 的交错顺序 |
| session/model/history | new/switch/fork/clone/tree/entries/stats/model/thinking/compact 都有命令 | 原生 session 文件不互换；模型/compaction metadata 不完整；深层 branch、recovery、fromHook 语义未证明 |
| 扩展 UI | select/confirm/input/editor + notify/status/widget/title/editor text；正常 id 关联、超时、断连清理有本仓测试 | facade 降级行为仍不全同；任意第三方扩展未全部验证 |
| 入口 | Rust CLI 和 Node host 都走 pi-rpc；Node 承载 JS/TS 扩展 | native-only 入口不能直接替代 Node 扩展宿主；用户安装版本没有因源码核查自动更新 |

入口证据：`apps/pi-cli/src/lib.rs:345-350,649-660`；`packages/pi/src/index.ts:46-67`。
Native-only 拒绝已启用 JS/TS 扩展的提示在 `apps/pi-cli/src/lib.rs:604-616`。

## 测试证据层级

1. `crates/pi-rpc/tests/commands.rs` 使用 pi_rpc::rpc::dispatch、ScriptedProviderPlugin 和本仓 session manager
   （见 `1-18,101-124`）。这是本仓边界回归，不是两端 RPC differential。
2. `packages/pi/test/rpc-interop.smoke.ts:11,31-70` 启动的是 `packages/pi/dist/bin/pi.js`，使用本地假 provider、
   隔离 HOME/agent-dir；没有同时启动 legacy Pi RPC 并比较输出。上轮 40 passed/2 skipped 证明本地桥接场景可用，
   不证明全协议兼容。
3. “shared Rust/JavaScript wire corpus” 实际是 `packages/pi/test/ui-command.test.ts:7-19`，
   读取 `crates/pi-plugin/tests/fixtures/ui-commands.json` 校验本仓语义 UI 命令，不是 legacy RPC corpus。
4. `scripts/sync-prompt-conformance.mjs` 的真实 legacy differential 仅涵盖 prompt/tool/MCP schema 样本，不是 RPC。
5. 本轮新执行的是两项小型纯函数检查和一个隔离启动 smoke。Rust 本地构建的 get_state/v3-switch/EOF 已确认；
   尝试启动 legacy CLI 源码时遇到 ERR_MODULE_NOT_FOUND，因此**没有完成双进程 differential**。
   原生 v3 被 legacy 接受的结论来自明确源码链，而不是这次失败的启动尝试。

没有为此次审计重跑 cargo test、Clippy、Node 全量或 native rebuild。原有绿灯只保留其原有代码快照和范围的意义。

## 本地构建与用户安装

隔离 smoke 的 Rust 入口为：

```sh
PI_RS_NATIVE_BINDING="$PWD/target/aarch64-apple-darwin/debug/libpi_napi.dylib" \
  node packages/pi/dist/bin/pi.js --mode rpc --no-session --no-extensions
```

smoke 额外使用临时 cwd/agent-dir、PI_OFFLINE、只指向 localhost:9 的假 provider 配置，未发 prompt，
不会使用真实凭证或更新用户会话；临时目录已清理。实际请求为 get_state 与 switch_session，随后 stdin EOF，exit 0。
此为本地已构建产物的证据，不是全局安装版本证据。此次没有提交、安装、发布或重启用户现有会话。
