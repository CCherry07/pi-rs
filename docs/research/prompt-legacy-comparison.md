# pi-rs 与 legacy Pi prompt 对比

> 修改前的历史快照。后续对齐实现与验证状态见 [Prompt conformance](../prompt-conformance.md)。

## 范围与结论

2026-11-18 对当前工作树进行静态源码核对：pi-rs HEAD `2863458`，本地
`legacy/pi` HEAD `05fd3fbda`。工作树存在大量未提交改动，本结论包含这些改动，
不代表已发布版本，也不是对当前聊天宿主注入的全部指令的抓包。

**默认主系统 prompt 已基本对齐；共同工具的基础描述/参数基本对齐，但整个模型输入和工具行为仍不能宣称完全一致。**

## 已对齐部分

- 默认身份文案一致：`You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.`
- 主 prompt 结构一致：未包标签的 preamble，然后 `<tools>`、`<rules>`、`<docs>`，以及按条件出现的 `<addendum>`、`<project_context>`、`<skills>`、`<cwd>` 和自定义 sections。
- 工具摘要来自 `promptSnippet`；规则来自已声明工具的 `promptGuidelines`，去重后补充简洁回答、明确路径等规则。隐藏声明不进入工具列表/规则。
- 自定义 prompt 替换默认前缀，但仍可追加项目上下文、skills、cwd；`forceSystemPrompt` 是完整文本替换。
- docs 固定文案经脚本替换安装路径后逐字一致；路径分别指向各自发行物，不应强行统一。
- skills 目录的说明、XML 行格式、`read`/`bash`/间接读取提示和 `disable-model-invocation` 过滤一致。
- `SYSTEM.md`/`APPEND_SYSTEM.md` 的发现都是可信 cwd `.pi` 优先、global 回退；项目上下文不受 trust 限制，全局后按祖先到 cwd 加载。
- 普通压缩 system prompt、初次 summary prompt、更新 summary prompt 的固定文案经脚本比较逐字一致。

证据：

- `domains/coding/src/prompt.rs:73-258`；`legacy/pi/packages/coding-agent/src/core/system-prompt.ts:58-223`。
- `plugins/features/pi-plugin-skills/src/lib.rs:891-960`；`legacy/pi/packages/coding-agent/src/core/skills.ts:358-387`。
- `domains/coding/src/resources.rs:67-202`；`legacy/pi/packages/coding-agent/src/core/resource-loader.ts:197-303,697-715,1253-1280`。
- `domains/coding/src/session/compaction.rs:12-82`；`crates/pi-session/src/runtime/compaction/algorithm.rs:16-18`；`legacy/pi/packages/coding-agent/src/core/compaction/compaction.ts:507-579`；`legacy/pi/packages/coding-agent/src/core/compaction/utils.ts:162-164`。

## 确认存在的差异

### 1. Prompt 模板展开仍落后于当前 legacy

Rust 的 `substitute_args` 支持 `$N`、`$@`、`$ARGUMENTS`、`${@:N}`、`${@:N:L}`，
但没有当前 legacy 的 `${1:-default}`、`${@:-default}`、`${ARGUMENTS:-default}`。
这些默认值表达式在 Rust 中会保留为未展开文本。

参数解析也不同：Rust 在单词内部遇到引号仍开始引用、丢弃空引号参数，不支持当前 legacy 的
同类引号转义和未闭合引号保留规则。例如 `doesn't work` 在 Rust 的简易解析器里会改变参数内容/分组。
这是最终发给模型的用户 prompt 差异，而不只是 UI 提示差异。

证据：`plugins/features/pi-plugin-prompts/src/lib.rs:480-582`；
`legacy/pi/packages/coding-agent/src/core/prompt-templates.ts:25-130`。

### 2. Turn-prefix 与分支摘要 prompt 未完全同步

Rust turn-prefix 仍使用 `PREFIX`/`SUFFIX`、`Early Progress`、`Context for Suffix` 旧表述。
当前 legacy 改为 earlier/later context checkpoint，使用 `Progress So Far`、`Context Needed to Continue`，
并明确要求 `Only summarize information explicitly present above. Do not infer or recreate later messages.`

包装也不同：Rust 用 `<conversation>`；legacy 使用 `# Conversation` / `# Instructions`，并可追加 `Additional focus`。
分支摘要的差异较小：Rust 缺少 Constraints & Preferences 下的 `[Or "(none)" if none were mentioned]`。

证据：`domains/coding/src/session/compaction.rs:84-99`；
`crates/pi-session/src/runtime/compaction/algorithm.rs:741-766`；
`legacy/pi/packages/coding-agent/src/core/compaction/compaction.ts:929-942,1078-1080`；
`legacy/pi/packages/coding-agent/src/core/compaction/branch-summarization.ts:272-300`。

### 3. MCP 目录引导仍有文案和条件差异

Rust MCP 目录总带 `tool_search` 的使用说明；codemode 存在时额外提示 `searchTools()`、`describeNamespace()`。
当前 legacy 根据列出的实际 reach 类型，仅说明存在的 codemode/tool_search 路径；省略服务器的尾提示也不同。
因此仅 codemode 的配置下，Rust 仍可能提及未激活的 `tool_search`。

证据：`plugins/features/pi-plugin-mcp/src/directory.rs:3-35`；
`legacy/pi/packages/coding-agent/src/extensions/mcp/index.ts:166-228`。

### 4. MCP 资源工具缺 output schema

`list_mcp_resources`、`list_mcp_resource_templates`、`read_mcp_resource` 的描述和输入字段基本一致。
但 Rust `ToolSpec.output_schema` 为 `None`，legacy 为三个工具分别提供结构化 `outputSchema`。
Rust codemode 的声明生成器把缺 schema 的返回值描述为 `string`，资源工具本身却通过 `structured_output`
提供 details 对象。因此 codemode 可见的返回类型说明与 legacy 不同，且 Rust 本身有声明与返回不一致的风险。

证据：`plugins/features/pi-plugin-mcp/src/resources.rs:83-129`；
`plugins/features/pi-plugin-codemode/src/declarations.rs:44-82`；
`legacy/pi/packages/coding-agent/src/extensions/mcp/resources.ts:89-166,257-312`。

### 5. 工具注册范围差异

- `codemode` 本身不是 Rust 独有：当前 legacy 内置扩展也提供它。两边基础描述、snippet、guideline、on/only 设计和默认 3000 token inline budget 基本一致。
- `tool_search` 的描述、输入参数和加载结果文案基本一致，但 legacy 内置扩展默认注册为 inactive，Rust Coding 产品主要在 MCP 配置需要它时注册。
- 第三方 MCP/JS/native 工具内容来自实际配置和插件，不能仅比较内置源码就宣称所有第三方工具一致。

证据：`plugins/features/pi-plugin-codemode/src/prompt.rs:9-58`；
`legacy/pi/packages/coding-agent/src/extensions/codemode/tool.ts:113-142`；
`legacy/pi/packages/coding-agent/src/extensions/codemode/index.ts:1-45`；
`plugins/features/pi-plugin-tool-search/src/lib.rs:23,57-78,107-166`；
`plugins/features/pi-plugin-mcp/src/client.rs:727-728`；
`legacy/pi/packages/coding-agent/src/extensions/tool-search/index.ts:1-18`。

### 6. Rust 可选产品扩展会增加 prompt

- hashline：扩展 `read`/`grep` 的参数、结果和说明，并提供 `hashline_edit`。
- Hermes memory：增加 memory/session_search 等能力、记忆上下文、托管 skill 根；启用时 `HERMES.md` 的目录内优先级高于 AGENTS 候选文件。
- subagents：增加六个协作工具及父/子代理协作指导。
- schedule：增加调度工具及独立会话/时区/常驻运行等指导。
- 多根 workspace：增加根目录列表和相对路径解释。

**当前代码默认 `hashline/memory/subagents/schedule = false`，skills/promptTemplates/sessionTransfer 为 true。**
`Features::all()` 是宿主允许上限，实际还与 settings 相交；不能据此或旧 README 说前四项默认开启。

证据：`domains/coding/settings/src/lib.rs:265-275`；`domains/coding/src/config.rs:87-98`；
`domains/coding/src/composition.rs:167-222,261-276`；
`domains/coding/src/resources.rs:182-201`；
`plugins/features/pi-plugin-memory-hermes/src/lib.rs:207-259`；
`plugins/features/pi-plugin-subagents/src/lib.rs:83-149`；
`plugins/features/pi-plugin-schedule/src/interface.rs:185-195`；
`domains/coding/src/workspace_prompt.rs:13-45`。

## 内置工具

8 个共同工具是 `read`、`bash`、`edit`、`write`、`grep`、`find`、`ls`、`powershell`。
基础 description、promptSnippet、promptGuidelines 和 input schema 已基本对齐。
当前 legacy 的 `edit` 已经支持 `edits[]`，`powershell` 也存在，不能将二者误报为 Rust 独有。
默认激活的基础工具两边均是 `read,bash,edit,write`；另外的配置/扩展可改变最终集合。

工具结果/执行语义不等于工具 prompt：共同文案不保证相同查找结果、截断提示、文件归一化行为。
逐工具详情见 [工具对比记录](../tool-prompt-comparison-notes.md)。

证据：`domains/coding/src/composition.rs:43-63,261-307`；
`legacy/pi/packages/coding-agent/src/core/tools/index.ts:107-123,176-183`。

## 验证与边界

- 静态阅读当前源代码，并用临时 Python 脚本抽取固定字符串作相等/差异比较。
- 未执行真实 provider 请求；未审计全部 provider 的最终 wire shaping、所有第三方工具或安装目录的实时配置。
- 未修改产品实现；仅增加比较记录。未跑 Rust 编译/测试/Clippy 或全工作区门禁。
- 建议优先补 MCP 资源 output schema、模板展开、turn-prefix prompt，再处理工具执行边界差异；可选 Rust 产品扩展应独立于兼容性缺口说明。
