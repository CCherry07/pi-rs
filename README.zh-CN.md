# pi_rs

[English](README.md) | **简体中文**

`pi_rs` 是一个用 Rust 实现的 Pi 风格终端 coding agent。它以当前 TypeScript Pi 为行为
参照，提供可实际使用的全屏 TUI、模型与 Provider 配置、工具调用、Skills、项目
Trust，以及可恢复的 Pi v4 会话。

这不是只有 agent loop 的库移植：交互 TUI、单次输出、NDJSON 事件流、会话恢复、上下文
压缩和插件热重载共享同一套产品运行时。

> 当前状态：已经形成可运行的产品基线，仍在持续补齐 Pi conformance、崩溃恢复编排和
> 原生插件分发。

## 主要能力

- **终端产品**：基于 Ratatui、Crossterm 和 Tokio 的全屏 TUI，支持 Markdown、代码高亮、
  中文 IME、复制选择、历史记录、滚动和命令选择器。
- **三种运行模式**：交互 TUI、`--print` 单次输出、`--json` NDJSON 产品事件流。
- **插件优先**：`AgentPlugin`、`ProviderPlugin`、`SessionPlugin` 三套窄生命周期；插件按
  generation 构建并原子 reload，失败时保留上一代。
- **原生插件**：版本锁定的 Rust `cdylib` 可从全局 manifest、可信项目 manifest 或重复
  `--plugin` 路径加载；本地/HTTP/GitHub package 与静态 Registry 通过精确 lock 和内容
  寻址仓库安装。
- **模型与 Provider**：支持 OpenAI-compatible API；`models.json` 统一管理模型目录、
  endpoint、请求参数、headers 和凭据解析。
- **内置工具**：`read`、`write`、`edit`、`hashline_edit`、`bash`、`grep`、`find`、`ls`。
- **Skills 与资源**：自动发现全局和项目 Skills，注册 `/skill:<name>`，并在每次 agent
  run 生成系统 prompt contribution。
- **Pi v4 会话**：支持延迟首次落盘、`/resume`、队列、分支/树语义、压缩、上下文修复和
  recovery reduction。
- **项目 Trust**：使用 `<agent-dir>/trust.json` 保存最近祖先决策，统一控制项目 prompt、
  skills 和原生插件的加载。

## 快速开始

要求 Rust 1.85 或更新版本。

```bash
git clone <your-repository-url>
cd pi_rs

# 配置一个 OpenAI-compatible API
export OPENAI_API_KEY="..."
export OPENAI_MODEL="gpt-4o-mini"
export OPENAI_BASE_URL="https://api.openai.com/v1"

# 启动全屏 TUI
cargo run -p pi-cli --
```

全屏 alternate screen 是默认模式。如果希望保留在终端主屏幕中：

```bash
cargo run -p pi-cli -- --no-fullscreen
```

其他常用方式：

```bash
# 单次请求，只输出最终 assistant 文本
cargo run -p pi-cli -- --print "summarize this repository"

# 输出 NDJSON 产品事件
cargo run -p pi-cli -- --json "list the Rust crates"

# 从 stdin 读取 prompt
printf 'explain this project' | cargo run -p pi-cli -- --print

# Shell shorthand；无需模型凭据
cargo run -p pi-cli -- --print '!git status --short'
cargo run -p pi-cli -- --print '!!git status --short'

# 在指定项目目录启动
cargo run -p pi-cli -- --cwd /path/to/project
```

查看全部 CLI 参数：

```bash
cargo run -p pi-cli -- --help
```

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
├── models.json          # Provider 与模型目录
├── settings.json        # 全局产品设置
├── trust.json           # 项目 Trust 决策
├── SYSTEM.md            # 全局系统 prompt（可选）
├── APPEND_SYSTEM.md     # 全局追加 prompt（可选）
├── skills/              # 全局 Skills
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
├── .agents/skills/      # 从 cwd 向 git root 搜索
└── .pi/
    ├── SYSTEM.md
    ├── APPEND_SYSTEM.md
    ├── plugins.json      # 可共享的项目插件意图
    ├── plugins.lock      # 项目精确解析结果
    ├── plugins/
    │   ├── store/sha256/ # 本地不可变 CAS blob，应忽略于版本控制
    │   └── installed/    # 当前有序的项目插件激活视图
    └── skills/
```

项目 `.pi` prompt 和项目 Skills 只会在项目被信任后加载。交互模式会展示 Trust 选择器；
非交互模式默认不信任，可通过 `--approve` / `-a` 或 `--no-approve` / `-na` 显式决定。

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
/model [provider/model|id]  查看或切换模型
/thinking <level>           设置 thinking level
/compact                    手动压缩上下文
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

## 架构

| 目录 | 职责 |
| --- | --- |
| `apps/pi-cli` | CLI、TUI、终端生命周期、Project Trust 和产品装配 |
| `crates/pi-core` | 强类型 contracts、registries 和插件 drivers |
| `crates/pi-agent` | Agent façade、agent loop、stream assembly 和工具调度 |
| `crates/pi-runtime` | generation 构建、prompt 装配和原子 reload |
| `crates/pi-session` | Pi v4 JSONL、树/分支、压缩、恢复 reducer 和 session runtime |
| `crates/pi-provider` | Provider-neutral HTTP transport 与 SSE |
| `crates/pi-prompt` / `pi-resources` | 系统 prompt 和项目上下文发现 |
| `apps/pi-md` | TUI 所有的 Markdown 解析、streaming mend、语法高亮和 Ratatui 渲染 |
| `crates/pi-plugin-sdk` / `pi-plugin-loader` | 原生插件作者 interface、兼容校验、发现与 factory adapter |
| `crates/pi-plugin-manager` | Package intent/lock、静态 Registry、target 选择和 CAS 安装 |
| `plugins/` | Skills、Provider catalog 和独立生产工具插件 |
| `legacy/pi` | 当前 TypeScript Pi 行为参照 |
| `e2e` | deterministic 全链路测试与示例项目 |

依赖保持向内：核心 contracts 不拥有终端、文件发现、会话存储或厂商路由策略。详细设计、
hook 顺序和持久化不变量见 [docs/architecture.md](docs/architecture.md)。

## 开发与验证

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

真实 Provider 测试不要把 API key 写入源码、日志或 fixtures。默认验证路径使用
deterministic faux provider。

## Apple Silicon 打包

在 Apple Silicon macOS 上生成 release 包：

```bash
./scripts/package-macos-arm64.sh
```

安装 `dist/` 中最新的包：

```bash
./scripts/install-package.sh
```

当前产物未签名、未 notarize。更多打包参数见
[apps/pi-cli/README.md](apps/pi-cli/README.md#package-for-apple-silicon-macos)。

## 尚未完成的边界

- 将 recovery reducer 接入完整的崩溃后 operation replay 执行编排；
- 增加带签名、内容寻址的原生插件安装与远端分发；
- 继续以 `legacy/pi` 为 oracle 补齐用户可见行为和跨平台终端兼容。
