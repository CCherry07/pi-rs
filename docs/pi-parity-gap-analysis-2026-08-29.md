# pi-rs 与当前 Pi 的能力差距（2026-08-29）

## 结论摘要

这次核对以 `legacy/pi` 中当前 TypeScript 源码和测试为行为基准，并逐项检查当前 Rust、Node 桥接层和现有测试；没有从 `docs/incomplete-handoff.md` 推导结论。

pi-rs 的核心已经不是“库级原型”：agent loop、基础文本交互、读写/编辑/搜索/shell 工具、项目 trust、skills / prompts / `models.json`、会话分支/压缩/恢复、插件 generation/reload，以及可用的全屏 Ratatui TUI 都已落地。最初识别出的自动化/迁移 P0 中，Pi stdin/stdout RPC、JSON/NDJSON wire，以及 coding-agent v1/v2/v3 → v4 importer 已在同日后续实现；上游仍属 experimental 且没有 pi-rs 产品入口的 protocol v1 server/client 则有意不纳入当前架构。当前最影响“可替换 Pi”的剩余差距是：

1. JavaScript 扩展的非 UI 主干能运行，但 Pi 扩展 UI、shortcut、renderer 和若干 hook/provider 能力仍是 inactive 或不识别。
2. Bedrock、Vertex、Mistral、Azure、Copilot、OpenRouter 等主流 Provider 已补入；剩余差距
   转为长尾 Provider、完整远端模型目录、Kimi/Radius OAuth 与真实账号 smoke matrix。
3. CLI 启动参数、多模态输入、settings/themes/keybindings/scoped-models 等完整产品工作流尚未对齐；v1-v4 import、JSONL/HTML export 和 GitHub Gist share 已补齐，但 Pi 的 Radius share 路径尚未实现。

优先级定义：P0 表示阻止 drop-in 替换、自动化接入、现有会话迁移或主流扩展运行；P1 表示高频产品能力明显缺失；P2 表示管理、发布或次要 SDK/体验能力。

| 优先级 | 状态             | 能力差距                                            | 直接影响                                                                          |
| ------ | ---------------- | --------------------------------------------------- | --------------------------------------------------------------------------------- |
| P0     | 部分对齐         | RPC 与 experimental server/client                  | Pi RPC 可直接运行；有意不实现没有产品入口的 experimental server/client           |
| P0     | 已对齐           | JSON/NDJSON wire format                             | JSON 与 RPC 共用 Pi projector，保留 delta、usage、tool metadata 和 entry identity |
| P0     | 已桥接           | Rust v4 与当前 coding-agent v1-v3 会话格式          | `/import` 非破坏式转换旧会话并事务式切换到 v4                                     |
| P0     | 部分对齐         | JavaScript 扩展 UI、shortcut、renderer              | 大量交互型 Pi 扩展会降级、静默 no-op 或加载失败                                   |
| P1     | 部分对齐         | provider/API/OAuth/catalog 覆盖                     | 主流专用协议已有开箱路径，但长尾 Provider、完整目录和 Kimi/Radius 仍未对齐        |
| P1     | 部分对齐         | CLI 启动参数和输入管线                              | 缺少 continue/resume/fork、prompt/tool/resource override、`@file` 等脚本接口      |
| P1     | 部分对齐         | 图片/文件用户输入与图片输出显示                     | provider/tool 类型支持图片，但 CLI/TUI 用户路径没有贯通                           |
| P1     | 部分对齐         | 完整 slash command / session workflow               | 缺少 settings、scoped-models、changelog、hotkeys 和 Radius share 等               |
| P1     | 部分对齐         | settings、themes、keybindings、模型 scope           | 一部分字段只解析不消费，TUI 仍主要是固定行为                                      |
| P1     | 部分对齐         | 扩展 hook、provider 和 ModelRegistry 高级 API       | 非 UI 扩展可用，但并非 Pi extension API 的完整实现                                |
| P1     | 部分对齐         | JavaScript programmatic SDK                         | Rust 有原生 crate API，但 npm 包不是 Pi JS SDK 的 drop-in 替代                    |
| P2     | 部分对齐         | package/config/update/auth 管理命令                 | 扩展包管理可用，自更新、模型目录刷新和资源配置 TUI 不可用                         |
| P2     | 缺失             | image-generation provider API                       | `pi-ai` 的 OpenRouter image generation 面没有 Rust 对应物                         |

## 已排除的过期差距

以下能力在当前代码中已经存在，不应继续沿用早期 handoff 的“未完成”判断：

- **基础 filesystem/shell 工具已经产品化。** 当前 generation 注册 read、grep、find、ls、write、edit、hashline edit 和 bash（`apps/pi-cli/src/session_factory.rs:698-714`），不是只有 mock tool。
- **skills、prompt templates、settings 和 project trust 已进入 generation 构建。** trusted project 的 scoped skill/prompt paths 在 prepare 阶段计算（`apps/pi-cli/src/session_factory.rs:141-190`）；trust service 有持久 store、交互 prompt 和缓存（`apps/pi-cli/src/project_trust.rs:70-99,160-185`）。
- **resume/fork/tree/compaction/reload 已是真实 session runtime 能力。** `PiSession` 已实现 resume/fork/reload（`crates/pi-session/src/multi_session_manager.rs:260-288`），`AgentSession` 实现 tree checkout 和 compaction（`crates/pi-session/src/agent_session.rs:1623-1640,1773-1788`）；product reload 会整体重建 runtime/provider/resource/session plugin generation（`crates/pi-session/src/agent_session_runtime.rs:343-369`）。
- **JavaScript 扩展并非整体缺失。** tools、commands、flags、许多 hooks、消息/session actions 和配置型 provider 已有真实 bridge（`packages/pi/src/extension-host.ts:500-609,611-688`）；剩余差距是 P0-4/P1-6 所列的 UI 和高级 API 子集。
- **TUI 已支持实际 fullscreen、Markdown、滚动、selection/clipboard、IME 输入和 command selectors。** 本报告只把 Pi 额外的 settings/themes/keybindings/session-management 工作流列为差距，不再把“没有 TUI”或“只有 print mode”列为缺失。

## P0

### P0-1：RPC 与 experimental server/client

**状态：RPC 已实现；experimental server/client 有意不实现；扩展 UI 通道仍归 P0-4。**

Pi 的 `--mode rpc` 是有请求关联 ID、response、event、extension UI request/response 的 stdin/stdout 双向协议；`runRpcMode()` 明确用于把 agent 嵌入其他应用（`legacy/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts:1-12,50-61`），并为扩展的 `select` / `confirm` / `input` 等 UI 建立 request-response 通道（同文件 `:79-160`）。Pi 还已有实验性 `server` 命令（`legacy/pi/packages/coding-agent/src/cli/experimental/commands/server.ts:13-44`），以及支持连接、列举/创建/attach session 的 `PiClient`（`legacy/pi/packages/client/src/client.ts:51-77,96-149`）和带 `prompt` / `steer` / `abort` / `setModel` / `setThinking` 的 session lease（`legacy/pi/packages/client/src/session-handle.ts:13-35,88-106`）。

pi-rs 现有 `CLIMode::Rpc` 与 `--mode rpc`。`crates/pi-rpc/src/rpc.rs` 实现严格 LF-delimited JSON、请求 ID 关联、同步 stdout、session replacement 事件重绑，以及 Pi 当前 prompt/queue/model/thinking/compaction/retry/bash/session/tree/export 命令；JSON 与 RPC 共用同一 projector。JavaScript extension UI request/response 尚未贯通，收到无对应请求的 `extension_ui_response` 会忽略，因此该子项仍在 P0-4 追踪。

Pi 的 `server` CLI 本身仍是 experimental parser。pi-rs 删除了没有产品调用者的 framed-CBOR protocol/server/client 库，避免同时维护第二套专有多会话传输；进程集成以已支持的 Pi stdin/stdout RPC 和 ACP 为准。这是有意的产品范围收缩，而不是 RPC 行为缺失。

ACP 后续作为有意新增的 Rust 产品能力落在独立 `pi-acp` crate，而没有塞进 `pi-rpc`：它通过官方 SDK 实现 stable v1 的 initialize/new/prompt/cancel/load/resume/list/close、流式消息与工具事件、模型/思考级别选项，以及 session-local stdio MCP。协议无关的 MCP client/tool adapter 位于 `pi-mcp`；ACP 只负责把 `mcpServers` 转换成不会写入 v4 的 transient generation overlay。

### P0-2：JSON/NDJSON 事件格式

**状态：已对齐。** `--json` 与 RPC 现在共用 Pi wire projector。

Pi JSON 模式先写 session header，再原样流出经 `toJsonEvent()` 处理的 `AgentSessionEvent`（`legacy/pi/packages/coding-agent/src/modes/print-mode.ts:108-132`）。其中 `message_update` 明确定义为 delta-only，并保留结构化 `usage`、assistant event、tool-call id/name（`legacy/pi/packages/coding-agent/src/modes/json-event.ts:10-18,40-60`）；回归测试还锁住了线性字节增长、usage 和 tool-call 元数据（`legacy/pi/packages/coding-agent/test/suite/regressions/7290-json-stream-linear.test.ts:22-42`、`7911-json-stream-usage.test.ts:19-30`、`7925-toolcall-start-metadata.test.ts:25-41`）。

`crates/pi-rpc/src/json_wire.rs` 现在先输出 coding-agent v3 session header，再显式投影 Pi event union。`message_update` 只携带 delta，并保留结构化 assistant event、usage 与 tool-call id/name；tool updates 保留 args/content/details/usage；tool result、compaction、retry 与 session optional fields 按 Pi 的 omission 规则编码。Rust 自有 revision envelope、snapshot、`Debug` 字符串和 `unknown` catch-all 都不会越过 wire。

v4 `EntryAppended` 与 compaction event 携带提交后的 `SessionRecord`，projector 因而能输出精确的 v3 tree ID、parent ID、ISO timestamp 和 `firstKeptEntryId`。session-owned `AgentEnd` 还提供真实 `willRetry`，避免低层 agent event 猜测 orchestration 状态。focused fixtures 覆盖 header、delta、tool metadata/update、entry identity，以及 JSON/RPC 共用路径。

### P0-3：当前 Pi coding-agent v1/v2/v3 → Rust v4 会话迁移

**状态：v4 仍是有意架构差异，迁移桥已实现。**

当前 `legacy/pi/packages/coding-agent` 仍声明 `CURRENT_SESSION_VERSION = 3`，header 是 `{type:"session", version, id, timestamp, cwd, parentSession}`（`legacy/pi/packages/coding-agent/src/core/session-manager.ts:30-39`），并内置 v1→v2→v3 迁移（同文件 `:231-295`）。与此同时，较新的低层 agent harness 已定义 `{kind:"header", version:4, createdAt, parentSessionId}`（`legacy/pi/packages/agent/src/harness/session/jsonl/types.ts:47-56`）。

pi-rs 核心继续严格使用 v4；`crates/pi-session/src/legacy_import.rs` 在存储 seam 外提供一次性 converter。`inspect_session_file` 识别 native v4 和 coding-agent v1/v2/v3，`import_session_file` 总是创建新 destination 且不修改 source。它保留 tree IDs、parent links、timestamps、parentSession/branchedFrom path、custom messages、未知 agent-message wire extensions 和 compaction context；v1 `firstKeptEntryIndex` 转换成 entry ID，v2 `hookMessage` 转换成 custom entry，v3 retained tail 显式落入 v4 compaction。

`/import`、`AgentSessionRuntime` 与 `MultiSessionManager` 已接入 converter。所有行先完整校验，malformed middle line 会硬失败；写入或 generation prepare/switch 失败会删除 staged destination 并保持当前会话不变。native v4 仍走同一事务式 copy/resume 路径。focused tests 覆盖 v1/v3 转换、未知字段保留、非法中间行回滚，以及 runtime 导入失败不改变当前会话。

### P0-4：JavaScript 扩展 UI、shortcut 和 renderer 仍未激活

**状态：部分对齐。** 工具、命令、flags、许多 agent/provider/session hooks、send/append/session control 已能跨 Node/Rust 边界工作；问题集中在 Pi 扩展生态中最常见的交互层。

Pi 扩展上下文在 TUI/RPC 中应有 `hasUI` 和真实 `ExtensionUIContext`（`legacy/pi/packages/coding-agent/src/core/extensions/types.ts:307-349`），并公开 shortcut、message/markdown/entry renderer（同文件 `:1316-1358`）。pi-rs 的 `createInactiveExtensionUI()` 则让 `select/input/editor/custom` 返回 `undefined`、`confirm` 返回 `false`、status/widget/header/footer/editor/theme 变更全部 no-op，且 `setTheme` 明确失败（`packages/pi/src/extension-context.ts:85-152`）；`hasUI` 恒为 false（同文件 `:281-286`）。

注册层同样把 `pi.registerShortcut` 和三个 renderer 标记为 inactive（`packages/pi/src/extension-host.ts:629-683`）。现有测试并非遗漏，而是明确锁住这个降级行为：TUI mode 下仍断言 `ctx.hasUI == false`，且 diagnostics 包含 inactive shortcut/message renderer（`packages/pi/test/extension-host.test.ts:731-812`）。本次执行该测试及 provider-registration 测试均通过，说明这是当前设计状态而不是偶发 bug。

另一个硬不兼容是 Pi 已公开 `ui_prompt_start` / `ui_prompt_end`（`legacy/pi/packages/coding-agent/src/core/extensions/types.ts:1085-1113,1280-1301`），但 pi-rs 的 active/known hook 集合都不含二者（`packages/pi/src/extension-host.ts:178-222`）；这两种 hook 会走“not recognized”错误，而不是温和 inactive。

**建议验收边界：**优先贯通 dialog UI、status/widget、shortcut 和 custom renderers；用 Pi 自带的交互扩展示例做真实 TUI conformance，而不仅是 host manifest 单测。

## P1

### P1-1：provider 广度、完整模型目录和 OAuth 仍未完全覆盖

**状态：部分对齐。**

Pi AI 层定义了 10 种已知 completion API，包括 Mistral conversations、Azure/OpenAI Responses、Bedrock Converse、Google Vertex 和 `pi-messages`（`legacy/pi/packages/ai/src/types.ts:17-27`），并默认构造约 40 个 provider（`legacy/pi/packages/ai/src/providers/all.ts:88-131`）。OAuth loader 覆盖 Anthropic、OpenAI Codex、GitHub Copilot、OpenRouter、Kimi Coding、xAI 和 Radius（`legacy/pi/packages/ai/src/auth/oauth/load.ts:14-67`）。

pi-rs 产品 generation 现在会同时注册 OpenAI-compatible、Anthropic、OpenAI Codex、xAI、
Google Gemini/Vertex、Mistral、Azure OpenAI Responses、OpenRouter、GitHub Copilot、Amazon
Bedrock 与 `models.json` plugin（`apps/pi-cli/src/session_factory.rs:689-928`）。`models.json`
接受 `openai-completions`、`openai-responses`、`azure-openai-responses`、
`mistral-conversations`、`anthropic-messages`、`google-generative-ai`、`google-vertex` 和
`bedrock-converse-stream` 八类 API（`plugins/providers/pi-plugin-models/src/config.rs:976-995`）；
OpenAI Codex 的专用 Responses 协议继续由内置 plugin 拥有。浏览器/device OAuth 已覆盖
Anthropic、OpenAI Codex、GitHub Copilot、OpenRouter 和 xAI（`apps/pi-cli/src/auth.rs:652-656`）。
Vertex 支持 API key、ADC 和服务账号，Bedrock 支持 bearer、AWS profile/静态凭据与
SigV4；Copilot 会读取账号 `/models` 结果过滤目录。

因此 Bedrock、Vertex、Mistral、Azure 等不再是“只能用 OpenAI-compatible 手工接入”的
缺口。尚未对齐的是 Pi 约 40 个内置 Provider 的完整广度、各 Provider 的完整/动态模型
目录、Kimi Coding 和 Radius OAuth、`pi-messages`、Radius 远端目录，以及 OpenRouter image
generation。当前新目录是经过选择的高价值子集，不能写成完整 Pi catalog parity。

**建议验收顺序：**先用真实隔离账号跑新增 Provider/OAuth smoke matrix，再从 Pi 当前模型
生成源补齐目录和长尾 Provider；随后单独实现 Kimi/Radius 的 OAuth、远端目录与
`pi-messages` 生命周期。

### P1-2：CLI 启动参数和输入管线只覆盖了 Pi 的子集

**状态：部分对齐。**

Pi 参数面包括 `--mode text|json|rpc`、`--continue/-c`、`--resume/-r`、system/append prompt、session name/no-session/id/fork/dir、model scope、tools include/exclude、thinking、export、skill/prompt/theme override、context-file/resource disables、list-models、offline 和 `@file`（`legacy/pi/packages/coding-agent/src/cli/args.ts:11-58,95-245`）。未知 long flag 还会交给扩展 flags（同文件 `:227-239`）。

pi-rs 当前顶层参数主要是 prompt、print/json、fullscreen、cwd、精确 session path、model/provider/base-url/api-key、agent-dir、native plugin、extension、trust（`apps/pi-cli/src/config.rs:8-84`）。`resolve_input()` 只拼 positional text 或读取 stdin（`apps/pi-cli/src/lib.rs:443-455`），没有 `@file`、多消息或图片附件组装。

因此启动时尚不能等价完成：继续最近会话/交互选择 resume、fork 指定会话、禁用 session、临时替换 system prompt、选择/排除工具、限定 model scope、单次关闭 skills/prompts/themes/context files、离线和 list-models。部分能力能在启动后通过 slash command 或 settings 间接完成，但不兼容 Pi 脚本。

### P1-3：用户图片/文件输入和图片结果显示没有贯通

**状态：部分对齐。** core message/provider/tool 类型已支持 image，read tool 也会生成图片内容；缺的是用户入口和前端呈现。

Pi 会把 `@file` 文本包装进 prompt，并把支持的图片处理成 `ImageContent`（`legacy/pi/packages/coding-agent/src/cli/file-processor.ts:24-87`），再与 stdin/首条消息组合提交（`legacy/pi/packages/coding-agent/src/cli/initial-message.ts:16-42`）。交互 TUI 还支持从 clipboard 读取图片并插入临时路径（`legacy/pi/packages/coding-agent/src/modes/interactive/interactive-mode.ts:2927-2969`）。

Rust 公开的普通提交入口只有 `submit(text)`（`crates/pi-session/src/agent_session.rs:745-779`）；CLI input 也只有文本。TUI 格式化 tool result 时只采集 `ContentBlock::Text`（`apps/pi-cli/src/tui/view.rs:2022-2038,2065-2079`），print 最终 assistant 输出同样忽略非文本 block（`apps/pi-cli/src/output.rs:272-286`）。因此“扩展 hook 内注入图片”可工作，不等于终端用户拥有 Pi 的图片输入/查看能力。

### P1-4：slash commands 与会话管理工作流不完整

**状态：部分对齐。**

Rust 已有 `/new`、`/resume`、`/reload`、`/trust`、`/login`、`/logout`、`/model`、`/thinking`、`/compact`、`/fork`、`/clone`、`/tree`、`/name`、`/session`、`/copy`、`/export`、`/import`、`/share`、`/clear`、`/help`、`/quit`；相关 effect 是真实 session 操作，不只是 UI stub。

Pi 还提供 `/settings`、`/scoped-models`、`/changelog`、`/hotkeys`（`legacy/pi/packages/coding-agent/src/core/slash-commands.ts:19-42`），这些命令在 Rust 中尚无对应实现。当前 Rust `/export` 可将活动分支导出为 portable v4 JSONL 或安全的单文件 HTML；`/import` 会确认后非破坏式转换 Pi v1/v2/v3 或复制 native v4，再事务式切换并在 generation 重建失败时回滚；`/share` 通过已登录的 `gh` 上传 non-public Gist。Pi 优先使用 Radius artifact、再 fallback 到 Gist，Rust 当前只有 Gist 路径。Pi 的 resume selector 还支持 named filter、path/sort toggle、rename/delete；tree selector 支持 label/timestamp/filter，而 Rust selector主要完成搜索和选择。

### P1-5：settings、themes、keybindings 和 scoped models 多为子集或 parse-only

**状态：部分对齐。**

Pi settings 还包含 per-model thinking、theme、thinking block/cache notice、external editor、startup/changelog、terminal、analytics、model scope、double-escape/tree filter、editor/output layout、markdown、warning 和 fullscreen 行为等（`legacy/pi/packages/coding-agent/src/core/settings-manager.ts:94-146`）。Pi keybinding manager 暴露 suspend、thinking/model cycle/select、external editor、image paste、queue restore、session rename/delete/sort、tree label/filter、scoped model 编辑等动作（`legacy/pi/packages/coding-agent/src/core/keybindings.ts:14-57,92-232`）。

pi-rs 的 typed settings 当前消费 default provider/model/thinking、transport、queues、compaction/retry/trust、shell、packages/extensions/skills/prompts/themes、enabledModels/defaultTools、timeouts 和 images（`crates/pi-settings/src/lib.rs:236-266`；decoder 在 `crates/pi-settings/src/document.rs:57-142`）。但 `themes` 和 `enabled_models` 在 Rust 仓库中只有 settings 定义/解析，没有产品消费路径；`AppConfig` 只保留 skill/prompt scoped paths，没有 theme paths（`apps/pi-cli/src/config.rs:210-230`），Ratatui 仅按终端背景选固定 light/dark Markdown theme（`apps/pi-cli/src/tui.rs:220-235`）。

输入控制也仍是固定 match：Ctrl+C、Ctrl+D、Tab、Alt+Enter、Enter、Esc 等（`apps/pi-cli/src/tui/controller.rs:46-120`），没有读取 Pi keybindings 配置。因此 custom theme、`/settings`、`/scoped-models`、model cycling、external editor、custom keybindings、tree/session selector 管理等尚未对齐。

### P1-6：扩展的非 UI 高级 hook、provider 与 ModelRegistry API 仍有缺口

**状态：部分对齐。**

pi-rs active hooks 已覆盖 input、agent/turn/message/tool/context/tool-call/tool-result、provider request/header/response 和主要 session lifecycle（`packages/pi/src/extension-host.ts:178-212`），这是很大的已对齐面。但 `project_trust`、`resources_discover`、`model_select`、`thinking_level_select`、`user_bash` 仅被识别后标成 inactive（同文件 `:213-222,559-568`）。

Provider 注册只转发 name/baseUrl/apiKey/api/headers/authHeader/models；`streamSimple`、`refreshModels`、`oauth` 被标成 inactive，整个 `Provider` object overload 也 inactive（`packages/pi/src/extension-host.ts:145-167,655-683`），而 Pi 明确支持 native Provider overload 和带 OAuth 的 ProviderConfig（`legacy/pi/packages/coding-agent/src/core/extensions/types.ts:1460-1496`）。现有 provider registration 测试也明确断言 `streamSimple` diagnostic，而不执行 callback（`packages/pi/test/extension-host.test.ts:624-702`）。

扩展 `modelRegistry` 在 pi-rs 只有 getAll/getAvailable/find/hasConfiguredAuth/displayName（`packages/pi/src/extension-api.ts:35-41`），Pi 的 facade 还支持 refresh/error、auth/key/header resolution、complete、OAuth 状态和动态 provider 管理（`legacy/pi/packages/coding-agent/src/core/model-registry.ts:32-64,95-147`）。

### P1-7：npm 包不是 Pi JavaScript programmatic SDK 的 drop-in 替代

**状态：部分对齐。** Rust crate 内已有强类型 runtime/session/provider/plugin API，但 JavaScript 用户看到的表面明显更窄。

Pi coding-agent 根包导出 `AgentSession`、extension runtime、`ModelRuntime`/`ModelRegistry`、`AgentSessionRuntime`、`createAgentSession*`、tool factories、`SessionManager`、`SettingsManager`、compaction/resource/trust helpers 等（`legacy/pi/packages/coding-agent/src/index.ts:1-50,156-218,232-338`）。pi-rs 的 `@pi-rs/cli` 根包只导出 extension host/types 和负责启动 CLI 的 `PiNodeHost`（`packages/pi/src/index.ts:1-45`）。

因此 Rust-native embedding 是新增能力，但现有依赖 Pi JS SDK 的 Node 应用仍无法只换包名迁移。需要明确产品目标：若承诺 Pi JS SDK compatibility，则应导出可编程 session/runtime client；若只承诺 CLI+extension compatibility，应在兼容矩阵中明确排除。

## P2

### P2-1：package/config/update 管理命令不完整

**状态：部分对齐。** pi-rs 已支持 JavaScript extension install/remove/list/update，并另有 native plugin 管理。缺失部分在代码中是显式错误：`pi update --models` 返回“不属于 JavaScript package management”，self/all/bare update 返回 self-update not implemented（`apps/pi-cli/src/package_commands.rs:45-100`）。

Pi 会实际调用 `ModelRuntime.refresh()` 更新远程 model catalogs（`legacy/pi/packages/coding-agent/src/package-manager-cli.ts:585-608`），也提供 `pi config [-l]` TUI 管理 package resources（`legacy/pi/packages/coding-agent/src/cli/args.ts:267-275`；入口 `legacy/pi/packages/coding-agent/src/main.ts:585-599`）。Rust `CliCommand` 目前没有 `Config`（`apps/pi-cli/src/config.rs:86-137`）。

### P2-2：auth 管理 CLI 语义不同

**状态：有意差异 + 部分缺失。** pi-rs 提供 login/logout/status（`apps/pi-cli/src/config.rs:139-167`），这对用户更直接；当前 Pi 的顶层 `auth` 则是给脚本用的 `check`、`print-api-key`、`print-bearer-token`，支持 JSON、refresh 控制和最小有效期（`legacy/pi/packages/coding-agent/src/cli/auth-command.ts:5-45,48-117`）。

这不是简单谁包含谁：Rust 的交互登录是新增/重排，Pi 的 credential readiness/export 自动化仍缺。若目标是 CLI 兼容，应补 Pi 子命令，同时保留更安全的 login/logout/status。

### P2-3：image-generation API 缺失

**状态：缺失。** Pi AI 层定义 `openrouter-images`（`legacy/pi/packages/ai/src/types.ts:31-33`）并内置 OpenRouter image provider（`legacy/pi/packages/ai/src/providers/all.ts:143-153`）。当前 Rust provider/runtime 没有独立 image-generation 模型/API 生命周期。这不是 coding-agent 文本主路径的 blocker，但属于 `pi-ai` 能力差距。

## 有意差异（不应误报成“尚未实现”）

1. **fullscreen 默认值不同。** Pi settings 默认 `regular`（`legacy/pi/packages/coding-agent/src/core/settings-manager.ts:1202-1209`，TUI 在 `interactive-mode.ts:567-571` 读取）；pi-rs 默认 alternate-screen，除非 `--no-fullscreen`（`apps/pi-cli/src/config.rs:347-349`），且测试明确锁定此行为（同文件 `:388-400`）。这是产品选择，除非目标改为严格 CLI UX parity。
2. **v4 session 是主动架构选择。** 参见 P0-3；应补 importer，而不是把 Rust 核心倒退成 v3。
3. **native Rust plugins / generation reload 是 pi-rs 增量能力。** `--plugin` 和 native plugin 管理直接出现在 Rust CLI（`apps/pi-cli/src/config.rs:65-67,92-96,170-207`）；这不是 Pi 缺口。
4. **Rust 默认工具多了 `hashline_edit`。** generation 同时注册 edit 和 hashline edit（`apps/pi-cli/src/session_factory.rs:708-714`），属于扩展而非 parity 问题。

## 不确定，需实机验证

### 1. 真实 provider/OAuth 流的完整兼容性

**状态：不确定需实机验证。** 当前产品 E2E 使用本地 scripted OpenAI server，测试 harness 明确注入 provider turns（`e2e/product/harness.ts:60-132`）；Rust runtime agent 测试也使用 scripted provider（`e2e/tests/runtime_agent.rs:123-167`）。这能验证协议编排，但不能证明 Anthropic、Codex、xAI、Google/Vertex、Mistral、Azure、Bedrock、OpenRouter 和 Copilot 的当前线上流式、错误、限流、credential refresh 与账号模型策略都与 Pi 相同。需要用隔离凭证做 smoke matrix，且不能把凭证写入 fixture/log。

### 2. 真实 PTY 下的跨平台 TUI parity

**状态：不确定需实机验证。** Rust 有大量 Ratatui layout/input 单测，但现有 product E2E 通过子进程 + scripted provider 跑非交互路径（`e2e/product/native-cli.test.ts:1-20`、`e2e/product/harness.ts:60-138`），未见 pseudo-terminal 驱动的 fullscreen/main-screen/IME/mouse selection/clipboard/suspend/resize conformance。至少需要 macOS、Linux、Windows Terminal 的 PTY/人工矩阵。

### 3. 真实 Pi 扩展集合的端到端兼容率

**状态：不确定需实机验证。** Host 单测已很好地覆盖 non-UI imports、工具、provider hooks 和 generation retirement；但 UI 被明确 stub。应从 `legacy/pi/packages/coding-agent/examples/extensions` 选代表性扩展，按“可运行 / 降级 / fatal”统计，而不是仅根据 API 名称推断。

## 建议实施顺序

1. **已完成协议主干：**Pi JSON projector 与 stdin/stdout RPC；experimental framed-CBOR server/client 已从产品范围移除。
2. **已完成迁移桥：**v1/v2/v3→v4 converter 已接入 `/import`，现有会话可非破坏式切换。
3. **下一步打通扩展交互层：**dialog/status/widget/shortcut/renderer，再补 UI prompt hooks；用 Pi 示例做 E2E。
4. **继续扩 provider breadth：**补完整 catalog、Kimi/Radius OAuth、`pi-messages` 与真实账号 smoke matrix。
5. **补产品工作流：**CLI override、`@file`/图片、Radius share、settings/themes/keybindings/scoped-models。
6. **最后补管理/SDK：**model refresh、config TUI、自更新、auth automation 和 JS programmatic SDK。

## 本次验证记录

- `cargo fmt --all -- --check`：通过。
- `cargo test --workspace`：通过，包含 JSON/RPC projector、legacy importer 与现有全 workspace 测试。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `git diff --check`：通过。
- `cargo test -p pi-cli config::tests::fullscreen_is_default_and_can_be_disabled_explicitly -- --exact`：通过（1 passed）。
- 执行 `packages/pi` tests 时，相关的 provider-registration 与 inactive JavaScript UI tests 均通过，确认报告所述 inactive 行为；但命令同时跑了完整 npm 测试集，其中 2 个 release tests 因 `pi-settings` workspace/Cargo.lock 预期不一致失败（31 passed, 2 failed）。这是现有 release-test 基线问题，不作为上述 Pi parity 结论的证据，也未在本次调研中修改。

### 实现更新

同日后续先实现了原生 v4 `/import`、活动分支 `/export`（JSONL/HTML）和 GitHub Gist `/share`，随后补齐 Pi JSON/RPC projector、`--mode rpc`，以及 legacy v1/v2/v3 → v4 transactional importer。曾加入但没有产品入口的 protocol v1 CBOR/server/client/session lease 后续作为架构减法删除。RPC/JSON 位于 `pi-rpc`；其后的 ACP stable-v1 adapter 与 MCP client 分别位于 `pi-acp`、`pi-mcp`，共享 protocol-neutral `PiSession` seam，但不共享 wire enum。Radius share 和 P0-4 扩展交互层仍是上述差距。
