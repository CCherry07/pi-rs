# Pi 生态现状

> 调研快照：2026-08-21。源码基线为本仓库 `legacy/pi` 的
> [`59a71b2`](https://github.com/earendil-works/pi/tree/59a71b235dadb4ad0d67557a8abb0aaa093e68b4)。
> 本文只使用 Pi 官方源码、官方站点、npm Registry 和 GitHub API 等一手来源。

## 结论

Pi 已经不是单独的终端聊天 CLI，而是一个**以 coding-agent 产品为中心、以 TypeScript
Extension 和 Pi Package 为主要扩展单元、以独立 AI/Agent SDK 为底座**的生态。它的强项是
扩展面宽、模型覆盖广、嵌入方式完整、第三方包分发活跃；主要短板是扩展代码没有沙箱、包目录
主要解决发现而不是信任、远程协议仍处于实验期。

从产品成熟度看，它已是可日常使用并可被其他产品嵌入的成熟 harness；从稳定平台的角度看，
它仍是快速演进中的 pre-1.0 生态。当前 npm 版本为 `0.84.2`，官方 Package Catalog 显示
5,393 个条目；npm 官方下载接口记录最近一个月约 704 万次下载，GitHub API 显示约 9.49 万
stars。这些数据能证明活跃度和传播规模，但不能证明每个社区包的质量。
[来源：npm latest](https://registry.npmjs.org/@earendil-works%2Fpi-coding-agent/latest)、
[npm downloads](https://api.npmjs.org/downloads/point/last-month/@earendil-works%2Fpi-coding-agent)、
[GitHub API](https://api.github.com/repos/earendil-works/pi)、
[Package Catalog](https://pi.dev/packages)

## 1. 核心产品与包

Pi 采用 monorepo 和同版本发布。主要层次如下：

| 层次 | 官方包 | 主要职责 |
|---|---|---|
| 模型/API | `@earendil-works/pi-ai` | Provider、模型目录、认证/OAuth、流式调用、tool calling、thinking、图像与跨 Provider 上下文 |
| Agent runtime | `@earendil-works/pi-agent-core` | 有状态 agent loop、消息与工具执行、steer/follow-up 队列、事件流、可替换 session backend |
| 产品 | `@earendil-works/pi-coding-agent` | CLI/TUI、会话、压缩、资源发现、Extensions、Skills、Prompts、Themes、SDK、RPC |
| 终端 UI | `@earendil-works/pi-tui` | 差分渲染、编辑器、Markdown、选择器、overlay、IME、图片和自定义组件 |
| 可观测性 | `@earendil-works/pi-telemetry` | vendor-neutral telemetry contract、typed schema、adapter conformance |
| 会话存储 | `@earendil-works/pi-session-backend-sqlite-node` | v4 lane session repository、迁移、物化视图、FTS、writer lease |
| 远程化 | `@earendil-works/pi-protocol` / `pi-client` / `pi-server` | framed CBOR 协议、transport-neutral client、可嵌入 session server |
| 评测 | `@earendil-works/pi-evals`（private） | 真实模型驱动的端到端行为评测、对比 prompts/tools/skills/models |

产品入口仍是 coding-agent：默认工具、交互 TUI、会话树/分支/压缩、项目上下文和多种运行模式
都在这一层；`pi-ai` 与 `pi-agent-core` 可以独立作为 SDK 使用。
[来源：根 README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/README.md)、
[agent README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/agent/README.md)、
[coding-agent README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/README.md)、
[SQLite backend README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/session-backends/sqlite-node/README.md)

## 2. Extension、Package、Skill：生态的三个主要单元

### TypeScript Extension

Extension 是最强的可执行扩展机制。它由 `jiti` 直接加载 `.ts`/`.js`，工厂可以同步或异步，
自动发现目录中的扩展支持 `/reload` 热重载。API 覆盖：

- project trust、resource、session、agent/turn/message、provider request、tool、model、input 等事件；
- 注册/替换工具、命令、快捷键、CLI flag、Provider；
- 修改 prompt/context/compaction/tool result/provider payload；
- 自定义消息、Markdown、entry、工具结果、footer/header/editor/overlay 等 TUI 表现；
- session entry 持久化和扩展间 event bus。

官方仓库本身提供 79 个顶层扩展示例、85 个 TypeScript 示例源文件，覆盖权限门、plan mode、
subagent、SSH/沙箱、动态工具、自定义 Provider、git checkpoint、复杂 TUI 乃至小游戏。这说明
Extension 已是产品功能实验与社区创新的主要落点，而不是简单的 tool registration。
[来源：Extension 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/extensions.md)、
[官方示例目录](https://github.com/earendil-works/pi/tree/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/examples/extensions)

### Pi Package

Pi Package 是**分发与聚合单元**，不是另一套运行时。一个包可以同时带
`extensions/`、`skills/`、`prompts/`、`themes/`，通过 `package.json.pi` 显式声明或按约定目录发现。
安装源支持 npm、git 和本地路径；可以全局安装，也可以写入项目 `.pi/settings.json`，项目被信任后
自动补装。`pi install/remove/list/update/config` 构成基本包管理体验。

官方 Gallery 通过 npm 的 `pi-package` keyword 发现包，并展示类型、下载量、仓库、预览和举报入口。
因此它已经具备低门槛的大规模分发网络。根据官方公开的准入和安装机制可以推断，它目前更接近
“公开目录 + npm/git 安装器”，而不是经过安全审核、签名或兼容性认证的插件商店。
[来源：Package 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/packages.md)、
[官方 Gallery](https://pi.dev/packages)

### Skills、Prompt Templates 与 Themes

Skills 实现 Agent Skills 标准，采用 progressive disclosure：启动时只把 name/description 放进
system prompt，匹配任务后由模型读取完整 `SKILL.md`；同时注册 `/skill:name` 以便用户强制调用。
Pi 会发现自身目录和 `.agents/skills`，也能配置 Claude Code/Codex skill 目录，所以 Skill 是
跨 harness 复用面最强的一层。Prompt Templates 是 Markdown slash command，Themes 是 JSON
主题；三者都能随 Pi Package 分发。

官方文档也明确承认模型并不总会主动读取匹配 Skill，因此“目录提示 + 显式 skill command”仍需
共同存在。
[来源：Skills 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/skills.md)、
[Prompt Templates 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/prompt-templates.md)

## 3. Provider 与模型生态

`pi-ai` 把 Provider 定义为运行时所有者：每个 Provider 持有模型目录、认证/OAuth 和 stream 行为，
不同 Provider 可以复用 OpenAI Responses/Completions、Anthropic Messages、Google 等 wire API。
源码当前注册 40 个 built-in Provider factory，其中官方静态模型页展示 39 个 Provider、1,306 个
provider/model 条目；`radius` 是纯动态 Provider，因此不出现在静态目录。条目数包含同一模型经
不同云或路由商提供的重复入口，不等于 1,306 个独立模型家族。
[来源：Provider factories](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/ai/src/providers/all.ts)、
[官方 Models Catalog](https://pi.dev/models)

覆盖面包括 OpenAI/OpenAI Codex、Anthropic、Google/Vertex、Bedrock、Azure、OpenRouter、
GitHub Copilot、xAI、DeepSeek、Mistral、Groq、Cerebras、NVIDIA、Cloudflare、Vercel、
Moonshot/Kimi、MiniMax、Qwen、ZAI、Xiaomi 等；认证同时支持 API key、环境变量、存储凭据、
OAuth/订阅账号和云平台 ambient credential。

自定义模型有两档：

1. `models.json` 配置 OpenAI/Anthropic/Google 兼容 endpoint、模型元数据和兼容开关；
2. Extension 或 SDK 注册完整 Provider，实现自定义 OAuth、动态模型发现、过滤和 stream。

这个设计使“catalog/config overlay”和“真正的新 Provider 实现”分开，同时仍落到统一的
`Models` 查询与路由面。
[来源：pi-ai README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/ai/README.md)、
[Providers 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/providers.md)、
[models.json 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/models.md)

## 4. 嵌入与外部集成方式

Pi 提供由近到远的多层集成面：

1. **CLI 模式**：interactive TUI、一次性 print、JSON event stream；
2. **同进程 SDK**：`createAgentSession()` 管单会话，`AgentSessionRuntime` 管 new/resume/fork/import
   等会话替换，并复用产品层 resource/model/session 行为；
3. **子进程 RPC**：stdin/stdout JSONL，适合非 TypeScript 语言、IDE 和自定义 UI；
4. **远程 session 协议**：`pi-protocol` 用 length-prefixed CBOR，`pi-client` 提供 transport-neutral
   client，`pi-server` 提供需要宿主应用实现 service 的 server core；
5. **底层库嵌入**：只使用 `pi-ai` 或 `pi-agent-core` 构建不同产品。

SDK 官方示例已有 13 个，从最小会话到自定义 model/prompt/skill/tool/extension/settings/session 和
完整 runtime。远程 protocol/client/server 是独立于旧 RPC 的新方向，但官方明确标为 experimental：
protocol 没有兼容保证，server 没有 standalone CLI，也没有内置 coding-agent service。
[来源：SDK 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/sdk.md)、
[RPC 文档](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/rpc.md)、
[protocol README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/protocol/README.md)、
[server README](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/server/README.md)

## 5. 成熟度与限制

**成熟的部分：** 核心包边界清楚；Provider/模型与认证覆盖广；Extension hook、TUI customization、
资源发现和 reload 已形成闭环；SDK/RPC/JSON 都复用同一个 `AgentSessionRuntime`；Package Gallery、
npm/git 安装和跨 harness Skills 已形成实际分发生态；仓库还有 telemetry conformance、faux provider
测试和 model-backed eval 基础设施。

**仍需谨慎的部分：**

- Pi 本身没有文件系统、进程、网络和凭据权限系统；Extension 以启动用户的完整权限执行，Skill
  也可以指示模型运行任意代码。官方建议审查源码并用 Docker、Gondolin 或 OpenShell 隔离。
- Gallery 的进入条件主要是 npm keyword，5,393 个条目是生态规模而不是审核结果；第三方包必须
  被视为供应链代码。
- 远程 CBOR protocol/client/server 仍明确 experimental；pre-1.0 版本和高频演进意味着宿主集成
  应固定版本并做兼容测试。
- Pi 刻意不把 subagent、plan mode 等工作流做成内建产品策略，而是交给扩展；这让核心更小，
  但也使不同包之间的行为、质量和 UX 一致性依赖作者。
- Skill 的自动触发仍受模型行为影响，不能仅凭 catalog prompt 假设一定会加载全文。

[来源：权限与容器化说明](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/README.md)、
[Package 安全说明](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/docs/packages.md)、
[coding-agent 产品哲学](https://github.com/earendil-works/pi/blob/59a71b235dadb4ad0d67557a8abb0aaa093e68b4/packages/coding-agent/README.md)

## 6. 对 `pi_rs` 的直接启示

1. Pi Package 是分发概念，不应映射成第四种运行时插件或跨生命周期 wrapper。未来的
   manifest/loader 可以从一个包解析出彼此独立的 agent/provider/session factories；完整
   generation preparation 负责跨生命周期的原子切换。
2. Pi 真正成熟的是“资源发现和 Extension surface”，不是复杂依赖求解。因此 `pi_rs` 先做
   manifest、来源/provenance、发现、factory 构建和原子 generation reload，比先做拓扑排序更贴近 Pi。
3. Provider 应继续拥有 catalog、auth 和 request routing；`models.json` 是配置 overlay，完整自定义
   Provider 才是可执行插件。这与当前 `ProviderPlugin + ModelRuntime` 方向一致。
4. 动态插件一旦允许第三方 native code，就会继承 Pi 的最大风险。`pi_rs` 若要比 Pi 更适合产品化，
   应把“可信进程内插件”和“非可信进程外/WASM 插件”设计成 loader 层策略，而不是污染核心 Driver。
5. SDK、RPC、TUI 应继续消费相同的 session runtime 和 semantic event stream；Pi 的生态扩展性很大
   一部分来自这条统一产品主干。
