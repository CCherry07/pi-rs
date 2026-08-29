# pi-rs

[English](README.md) | **简体中文**

`pi-rs` 是 Pi 终端 coding agent 面向生产的 Rust 实现。它以当前 TypeScript Pi 为行为
参照，提供可实际使用的全屏 TUI、模型与 Provider 配置、工具调用、Skills、项目 Trust，
以及可恢复的 Pi v4 会话。

这不是只有 agent loop 的库移植：交互 TUI、单次输出、NDJSON 事件流、会话恢复、上下文
压缩和插件热重载共享同一套产品运行时。

> 当前状态：已经形成可运行的产品基线，仍在持续补齐 current-Pi conformance、中断操作的
> 显式安全重放，以及具备发布者认证的原生插件分发。

## 致敬

没有 [Pi](https://pi.dev)，就不会有 `pi-rs`。这个项目首先是一份用 Rust 写给 Pi 的致敬：
Pi 清晰的产品理念、克制的核心设计与 extension-first 架构，正是我们愿意重新实现它的
原因。谨向 Mario Zechner、Earendil Works，以及
[Pi 原项目](https://github.com/earendil-works/pi)的每一位贡献者致以最深的感谢。

`pi-rs` 是独立实现，并非 Pi 官方发行版。我们希望用认真对待兼容性的方式表达敬意：始终
阅读当前上游源码、保留 Pi 有意设计的行为，并在 Rust 产品需要分歧时明确说明。

## 主要能力

- **终端产品**：基于 Ratatui、Crossterm 和 Tokio 的全屏 TUI，支持 Markdown、代码高亮、
  中文 IME、复制选择、历史记录、滚动和命令选择器。
- **三种运行模式**：交互 TUI、`--print` 单次输出、`--json` NDJSON 产品事件流。
- **插件优先**：`AgentPlugin`、`ProviderPlugin`、`SessionPlugin` 三套窄生命周期；插件按
  generation 构建并原子 reload，失败时保留上一代。
- **原生插件**：版本锁定的 Rust `cdylib` 可从全局 manifest、可信项目 manifest 或重复
  `--plugin` 路径加载；本地/HTTP/GitHub package 与静态 Registry 通过精确 lock 和内容
  寻址仓库安装。
- **Pi JS/TS extensions**：可选的 Node 20 + NAPI-RS 启动层负责发现和 reload Pi 风格
  extension，包括托管的本地/npm/git package；Rust runtime 与 Ratatui 产品仍是唯一权威
  实现。
- **模型与 Provider**：内置 OpenAI-compatible、OpenAI Codex、Anthropic/Claude Code、
  Google Gemini/Vertex、xAI Grok、Mistral、Azure OpenAI Responses、Amazon Bedrock、
  OpenRouter 和 GitHub Copilot；`models.json` 统一管理自定义模型、endpoint、请求参数、
  headers 和凭据解析。
- **凭据管理**：`/login`、`/logout` 与 `pi auth` 管理 Pi 兼容的 API key 和 OAuth 凭据，
  不会在 TUI 中回显 secret。
- **内置工具**：`read`、`write`、`edit`、`hashline_edit`、`bash`、`grep`、`find`、`ls`。
- **Skills 与 prompt templates**：自动发现全局和项目 Skills、注册 `/skill:<name>`，并从
  Markdown prompt template 注册 slash command，在每次 agent run 生成系统 prompt
  contribution。
- **Pi v4 会话**：支持延迟首次落盘、`/resume`、队列、分支/树语义、压缩、上下文修复和
  recovery reduction。
- **项目 Trust**：使用 `<agent-dir>/trust.json` 保存最近祖先决策，统一控制项目 settings、
  prompt、skills、extensions 和原生插件的加载。

## 快速开始

### 通过 npm 安装

发布到 npm 的 package 是完整产品的推荐入口。它要求 Node.js 20 或更新版本，会同时安装
`pi` 与 `pi-rs` 命令，并自动选择当前平台对应的 native package。

```bash
npm install --global @pi-rs/cli
npm list --global @pi-rs/cli --depth=0
pi --version
pi
```

首次使用时，在 TUI 中配置凭据并选择模型：

```text
/login
/model
```

### 从源码运行

仓库开发要求 Rust 1.98 或更新版本以及 Node.js 20 或更新版本。仓库通过
`rust-toolchain.toml` 固定使用 Rust 1.98.0。

```bash
git clone https://github.com/CCherry07/pi-rs.git
cd pi-rs
npm install --prefix packages/pi
./scripts/pi-dev
```

下面的示例使用已安装的 `pi` 命令；从源码运行时，可将 `pi` 替换为 `./scripts/pi-dev`。

全屏 alternate screen 是默认模式。如果希望保留在终端主屏幕中：

```bash
pi --no-fullscreen
```

其他常用方式：

```bash
# 单次请求，只输出最终 assistant 文本
pi --print "summarize this repository"

# 输出 NDJSON 产品事件
pi --json "list the Rust crates"

# 从 stdin 读取 prompt
printf 'explain this project' | pi --print

# Shell shorthand；无需模型凭据
pi --print '!git status --short'
pi --print '!!git status --short'

# 在指定项目目录启动
pi --cwd /path/to/project
```

查看全部 CLI 参数：

```bash
pi --help
```

直接运行 `cargo run -p pi-cli -- --no-extensions` 是有意使用 native-only adapter 的方式；
完整的 JavaScript/TypeScript extension 支持应使用 npm launcher 或源码脚本：

```bash
npm install --prefix packages/pi
./scripts/pi-dev --cwd /path/to/project

# 或加载明确的 extension 路径
./scripts/pi-dev --no-extensions -e /path/to/extension.ts
```

它按 Pi 顺序发现可信项目的 `.pi/extensions`、全局 `<agent-dir>/extensions` 与显式 `-e`
路径。`/reload` 会在同一个产品 generation 事务中一起重建 JavaScript callbacks、Rust
插件、模型、资源和 session plugins。支持范围与明确未支持项见
[packages/pi/README.md](packages/pi/README.md)。

## 凭据管理

`/login` 与 `/logout` 使用和命令行相同的 Pi 兼容 `<agent-dir>/auth.json`。写入过程使用隐藏
输入、文件锁、原子替换，并在 Unix 上设置为 `0600`：

```bash
# 交互选择 Provider 与认证方式
pi auth login

# 启动内置 Provider 的浏览器或 device OAuth
pi auth login anthropic --oauth
pi auth login openai-codex --oauth
pi auth login github-copilot --oauth
pi auth login openrouter --oauth
pi auth login xai --oauth

# 配置云厂商 credential chain
pi auth login amazon-bedrock
pi auth login google-vertex

# 不回显地输入 API key
pi auth login anthropic --api-key
pi auth login google --api-key

# 只查看凭据类型和状态，不输出 secret
pi auth status
pi auth logout anthropic
```

也可以使用 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`GEMINI_API_KEY`、
`GOOGLE_CLOUD_API_KEY`、`XAI_API_KEY`、`MISTRAL_API_KEY`、`AZURE_OPENAI_API_KEY`、
`OPENROUTER_API_KEY` 和 `COPILOT_GITHUB_TOKEN` 等环境变量。Vertex 支持 ADC/服务账号，
Bedrock 支持 AWS profile/静态凭据链和 bearer token。显式 `--api-key` 优先于已保存凭据与
Provider 环境变量；Anthropic、Codex、Copilot 和 xAI 的 OAuth 凭据会在临近过期时刷新。

## JavaScript extension package

Node launcher 把 Pi 兼容的 extension 发现和 package 状态交给 Rust package manager，支持
用户级或可信项目级的本地、npm 与 git source：

```bash
pi install npm:example-extension
pi install --local git:github.com/example/project-extension@v1 --approve
pi list
pi update --extensions
pi update npm:example-extension
pi remove npm:example-extension
```

默认操作用户级配置；`--local` 写入可信项目的 `.pi/settings.json`。精确 npm 版本保持
锁定，range、未指定版本的 npm package 和 git package 可以更新。裸 `pi update` 预留给
产品自更新，目前尚未实现。详细的发现顺序、filter、offline 行为和 settings 格式见
[packages/pi/README.md](packages/pi/README.md#pi-javascripttypescript-extensions)。

## 配置模型

默认 agent 目录为 `~/.pi/agent`，也可以通过 `PI_AGENT_DIR` 或 `--agent-dir` 修改。
推荐在 `<agent-dir>/models.json` 中注册模型：

```jsonc
{
  // models.json 支持注释
  "providers": {
    "openai-compatible": {
      "api": "openai-completions",
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "$OPENAI_API_KEY",
      "models": [
        {
          "id": "gpt-4o-mini",
          "name": "GPT-4o mini",
          "reasoning": false,
          "input": ["text", "image"],
          "contextWindow": 128000,
          "maxTokens": 16384
        }
      ]
    }
  }
}
```

`apiKey`、headers 和其他字符串配置在请求发送时才解析环境变量；也支持以 `!` 开头的
shell command value。凭据不会进入公开的模型目录。

自定义路由支持 `openai-completions`、`openai-responses`、`azure-openai-responses`、
`mistral-conversations`、`anthropic-messages`、`google-generative-ai`、`google-vertex` 和
`bedrock-converse-stream` 八种 wire API，并保留各协议自己的 endpoint、认证、thinking、
图片、工具调用和流式语义。

模型选择优先级为：

1. CLI 显式指定的 `--model` / `--provider`；
2. 恢复会话中仍然有效的模型；
3. `models.json` 目录中的第一个模型；
4. `OPENAI_MODEL` 或 CLI fallback。

TUI 中使用 `/model` 查看和切换当前 generation 注册的模型。修改 `models.json` 后可用
`/reload` 原子重建插件、模型和资源；配置错误不会替换正在使用的 generation。

## Agent 目录与项目资源

默认目录布局：

```text
~/.pi/agent/
├── auth.json            # Pi 兼容的 API key 与 OAuth 凭据
├── models.json          # Provider 与模型目录
├── settings.json        # 全局产品设置
├── trust.json           # 项目 Trust 决策
├── SYSTEM.md            # 全局系统 prompt（可选）
├── APPEND_SYSTEM.md     # 全局追加 prompt（可选）
├── skills/              # 全局 Skills
├── prompts/             # 全局 Markdown prompt templates
├── extensions/          # 全局 JS/TS extensions
├── plugins.json         # 有序的原生插件意图
├── plugins.lock         # 当前 target 的精确解析与安装记录
├── plugins/
│   ├── store/sha256/    # 以 digest 命名的不可变 CAS blob
│   └── installed/       # 当前有序的原生插件激活视图
├── plugin-data/         # 插件持久数据
└── sessions/            # Pi v4 JSONL 会话

~/.agents/skills/        # 始终可信的用户 Skills 根目录
```

项目可提供：

```text
project/
├── AGENTS.md            # 项目上下文；不受 Trust gating
├── CLAUDE.md            # 项目上下文；不受 Trust gating
├── .agents/skills/      # 从 cwd 向 git root 搜索
└── .pi/
    ├── settings.json    # 项目 extensions 与 packages
    ├── SYSTEM.md
    ├── APPEND_SYSTEM.md
    ├── prompts/         # 项目 Markdown prompt templates
    ├── extensions/      # 项目 JS/TS extensions
    ├── plugins.json      # 可共享的项目插件意图
    ├── plugins.lock      # 项目精确解析结果
    ├── plugins/
    │   ├── store/sha256/ # 本地不可变 CAS blob，应忽略于版本控制
    │   └── installed/    # 当前有序的项目插件激活视图
    └── skills/
```

项目 `.pi` settings、prompt、extensions、Skills 和原生插件只会在项目被信任后加载；
`AGENTS.md` 与 `CLAUDE.md` 上下文发现不受 Trust 决策影响。交互模式会展示 Trust 选择器；
非交互模式默认不信任，可通过 `--approve` / `-a` 或 `--no-approve` 显式决定。

全局 `settings.json` 可以配置默认行为：

```json
{
  "defaultProjectTrust": "ask"
}
```

可选值为 `ask`、`always`、`never`。

> Project Trust 不是文件系统沙箱。与 Pi 一样，文件工具支持 cwd 相对路径、绝对路径、
> `~`、`file://` 和越过 cwd 的父级路径；实际边界是运行进程的操作系统权限。

## Native Plugin Package

```bash
pi plugin install ./path/to/package
pi plugin install https://example.com/pi-plugin-release.json
pi plugin install registry:frontend-check@^1 \
  --registry https://plugins.example/index.json
pi plugin list
pi plugin sync --registry https://plugins.example/index.json
pi plugin remove frontend-check
```

传入 `-l` 会管理可信当前项目的 `.pi/plugins.json` 与 `.pi/plugins.lock`，否则管理全局
Agent 状态。Manager 会选择准确的 host target、保留声明顺序、校验 artifact SHA-256、
写入 lock，并把不可变 CAS package 激活给现有 native loader。发布清单与静态 Registry 格式见
[crates/pi-plugin-manager/README.md](crates/pi-plugin-manager/README.md)。当前 SHA-256 只提供
完整性，不证明发布者身份；签名与 OCI source 仍是后续里程碑。

正常启动会自动同步全局 intent 与可信项目 intent，运行中的 session 可通过 `/reload` 做
同样的同步。已锁定版本保持不变；修改后的 options 与重新编译的本地 artifact 会以事务
方式生效，如果下一代 generation 加载失败则恢复原状态。

## TUI 命令

内置命令包括：

```text
/new [path]                 新建会话
/resume [query|path]        列出、过滤或恢复会话
/reload                     重建插件、模型和资源 generation
/trust                      修改当前项目 Trust
/login [provider]           配置 OAuth 或 API key 认证
/logout                     删除已保存的 Provider 凭据
/model [provider/model|id]  查看或切换模型
/thinking <level>           设置 thinking level
/compact [instructions]     压缩上下文，可附带指导
/fork                       从之前的 user message 创建分支
/clone                      在当前位置克隆会话
/tree                       浏览当前会话树
/name [name]                查看或设置会话名称
/session                    查看会话路径、用量与成本
/copy                       复制最近完成的 assistant 回复
/clear                      清空当前显示
/help                       显示命令
/quit                       退出
/skill:<name> [task]        显式调用已发现 Skill
```

输入 `/` 后可用上下键选择命令，`Tab` 补全。完整快捷键见
[apps/pi-cli/README.md](apps/pi-cli/README.md)。

## 会话行为

- 新会话先存在于内存中，只有收到第一个 assistant `message_end` 后才创建 JSONL 文件。
- 启动后直接退出、首个响应前中断、仅执行 shell shorthand 都不会污染 `/resume` 列表。
- 每个 assistant tool call 都会在下一次 Provider 请求前保存对应 tool result，避免恢复后
  出现 dangling tool call。
- `/resume` 会恢复会话 cwd、模型、消息树、队列和压缩状态；插件代码与资源始终从当前
  generation 重新构建，不从会话文件反序列化。
- 打开中断会话时会协调已接受的 deferred write 与未投递输入，并将中断操作关闭为
  aborted；打开过程不会发起 Provider I/O，也不会盲目重放外部副作用未知的工具。

## 架构

| 目录                                        | 职责                                                                |
| ------------------------------------------- | ------------------------------------------------------------------- |
| `apps/pi-cli`                               | CLI、TUI、终端生命周期、Project Trust 和产品装配                    |
| `crates/pi-core`                            | 强类型 contracts、registries 和插件 drivers                         |
| `crates/pi-agent`                           | Agent façade、agent loop、stream assembly 和工具调度                |
| `crates/pi-runtime`                         | generation 构建、prompt 装配和原子 reload                           |
| `crates/pi-session`                         | Pi v4 JSONL、树/分支、压缩、恢复 reducer 和 session runtime         |
| `crates/pi-telemetry`                       | 强类型 Provider/harness span schema 与 sink adapter                 |
| `crates/pi-provider`                        | Provider-neutral HTTP transport 与 SSE                              |
| `crates/pi-prompt` / `pi-resources`         | 系统 prompt 和项目上下文发现                                        |
| `apps/pi-md`                                | TUI 所有的 Markdown 解析、streaming mend、语法高亮和 Ratatui 渲染   |
| `crates/pi-plugin-sdk` / `pi-plugin-loader` | 原生插件作者 interface、兼容校验、发现与 factory adapter            |
| `crates/pi-plugin-manager`                  | Package intent/lock、静态 Registry、target 选择和 CAS 安装          |
| `crates/pi-js-package-manager`              | Pi 兼容的 JS/TS 发现与本地/npm/git package 管理                     |
| `crates/pi-js-plugin` / `bindings/pi-napi`  | 强类型 JS lifecycle adapter 与 Node/NAPI 边界                       |
| `packages/pi`                               | Node 启动层、Pi extension 发现、Jiti loader 和 callback generations |
| `plugins/`                                  | Prompt/Skill features、Provider catalog 和独立生产工具插件          |
| `legacy/pi`                                 | 当前 TypeScript Pi 行为参照                                         |
| `e2e`                                       | runtime acceptance、黑盒产品 E2E 与示例项目                         |

依赖保持向内：核心 contracts 不拥有终端、文件发现、会话存储或厂商路由策略。详细设计、
hook 顺序和持久化不变量见 [docs/architecture.md](docs/architecture.md)。

## 开发与验证

Pi 核心行为的测试子集及其与 TypeScript oracle 的映射见
[docs/pi-core-test-matrix.md](docs/pi-core-test-matrix.md)。聚焦运行入口是：

```bash
./scripts/test-core.sh
```

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

下面的统一入口会构建 standalone CLI 与 Node/NAPI，并运行完整的 deterministic 黑盒产品 E2E：

```bash
npm ci --prefix packages/pi
npm --prefix packages/pi run e2e
```

真实 Provider 测试不要把 API key 写入源码、日志或 fixtures。默认验证路径使用
`pi-test-support` 中的 deterministic scripted provider。

## 多平台打包

安装发布工具后，在与 target 匹配的宿主机上生成 standalone archive 与 NAPI artifact：

```bash
cd packages/pi && npm install && cd ../..
./scripts/package-target.sh aarch64-apple-darwin
```

macOS 与 Linux 可安装 `dist/release/` 中匹配当前宿主的最新 archive：

```bash
./scripts/install-package.sh
```

发布矩阵覆盖 macOS arm64/x64、Linux glibc arm64/x64 与 Windows MSVC arm64/x64。GitHub
archive 是纯 Rust standalone 版本；npm 使用一个 JavaScript 根包和一个按 OS/CPU/libc 选择的
NAPI 可选包。Release Please 维护版本/changelog PR，npm Trusted Publishing 使用短期 OIDC
身份并自动生成 provenance，不保存长期 npm token。当前产物已有 checksum 和 native smoke
test，但尚未签名或 notarize。发布 Interface 与产物布局见
[apps/pi-cli/README.md](apps/pi-cli/README.md#multi-platform-packaging)。

## 尚未完成的边界

- 增加显式的 safe-tool/deferred replay adapter，且打开会话时不重放副作用；
- 为原生插件分发增加发布者签名、Git/OCI source、update/rollback 与 CAS 垃圾回收；
- 继续以 `legacy/pi` 为 oracle 补齐用户可见行为和跨平台终端兼容。
