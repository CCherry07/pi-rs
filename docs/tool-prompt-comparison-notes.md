# pi-rs 与当前 legacy Pi：内置工具 prompt / schema / 结果对照

> 修改前的历史快照。后续对齐实现与验证状态见 [Prompt conformance](prompt-conformance.md)。

## 范围与结论

本笔记对照**当前工作树（包含他人尚未提交的改动）**，不是 HEAD 或历史 handoff：

- Rust：`plugins/tools/pi-plugin-{read,bash,edit,write,grep,find,ls,powershell,hashline-edit}/src/lib.rs`。
- Pi 行为参照：`legacy/pi/packages/coding-agent/src/core/tools/` 及其直接调用的实现。
- 只讨论内置工具声明及重要执行结果；系统 prompt 的最终拼接、默认 active tools、MCP、codemode 等由主报告另行比较。**注册了工具不代表当前请求会携带该工具。**
- 下文路径均相对仓库根；链接标明本次读取时的行号。共享工作树继续变化后，应重新核对对应范围。

**核心结论：8 个共有内置工具的 description、promptSnippet、有效 promptGuidelines、基础参数 schema 已对齐；不能据此断言执行行为完全兼容。**

1. 当前 legacy `edit` **也已使用 `edits[]` 批量替换**，与 Rust 的提示词和 schema 相同；不能再用旧版 `oldText/newText` 单替换形状描述当前 Pi。
2. Rust 的额外工具是 `hashline_edit`，并可为 `read` / `grep` 增加 `hashline` 参数。**当前配置默认关闭 hashline**，不是默认提示词差异：见 [FeatureSettings::default，265–275](../domains/coding/settings/src/lib.rs#L265)；实际受 host feature 与 settings 共同限制，见 [config.rs，86–95](../domains/coding/src/config.rs#L86)。
3. 比“措辞不同”更重要的未对齐在执行语义：`edit` 对未改行的保留、`find` 的相对路径 glob / 大小写语义、`grep` 的上下文输出，以及 `grep/find` 恰好达到 limit 时的截断提示。
4. `powershell` **当前 legacy 已存在**，并非 Rust 独有。两侧都复用 shell 工具契约。

重要性用法：**高**＝会改变目标文件/匹配范围/工具调用方式；**中**＝会影响结果完整性判断、失败恢复或特定输入的可用性；**低**＝主要是文案或边界显示差异；**一致**＝本次检查未发现该声明层面的差异。执行差异不自动意味着有意产品分歧，本笔记不替实现者推断意图。

## 1. description / snippet / guidelines

以下 8 项的 description 在展开 Pi 的常量插值后与 Rust 相同；表内概述其内容，避免重复整段英文。参数字段上的 description 也相同。

| 工具 | description 内容及对照结论 | 相同的 promptSnippet | 有效 guidelines | 源码依据 |
| --- | --- | --- | --- | --- |
| `read` | **一致**：读文本/图像；jpg/png/gif/webp/bmp；文本最多 2000 行或 50KB；`offset/limit` 续读，完整文件须持续读取 | `Read file contents` | R | [Rust 70–94](../plugins/tools/pi-plugin-read/src/lib.rs#L70)；[Pi 14–23](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L14)、[229–241](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L229) |
| `bash` | **一致**：当前 cwd 执行；stdout/stderr；最后 2000 行或 50KB；截断保留完整临时文件；可选秒级 timeout | `Execute bash commands (ls, grep, find, etc.)` | S（可关闭） | [Rust 83–149](../plugins/tools/pi-plugin-bash/src/lib.rs#L83)；[Pi 39–61](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L39)、[272–288](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L272) |
| `edit` | **一致**：单文件、唯一且互不重叠的原始文本替换；合并相邻修改，不用大片不变内容连接远距离修改 | `Make precise file edits with exact text replacement, including multiple disjoint edits in one call` | E1、E2 | [Rust 47–70](../plugins/tools/pi-plugin-edit/src/lib.rs#L47)；[Pi 18–46](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L18)、[138–153](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L138) |
| `write` | **一致**：不存在则创建、存在则覆盖，自动创建父目录 | `Create or overwrite files` | W | [Rust 26–43](../plugins/tools/pi-plugin-write/src/lib.rs#L26)；[Pi 10–18](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L10)、[43–56](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L43) |
| `grep` | **一致**：按内容搜索，路径/行号，尊重 gitignore，100 个匹配或 50KB，长行 500 chars | `Search file contents for patterns (respects .gitignore)` | 无 | [Rust 62–77](../plugins/tools/pi-plugin-grep/src/lib.rs#L62)；[Pi 20–42](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L20)、[82–92](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L82) |
| `find` | **一致**：glob 文件搜索、相对路径、尊重 gitignore，1000 个结果或 50KB | `Find files by glob pattern (respects .gitignore)` | 无 | [Rust 30–42](../plugins/tools/pi-plugin-find/src/lib.rs#L30)；[Pi 26–38](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L26)、[71–81](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L71) |
| `ls` | **一致**：字母排序、目录 `/` 后缀、含 dotfiles，500 个条目或 50KB | `List directory contents` | 无 | [Rust 27–38](../plugins/tools/pi-plugin-ls/src/lib.rs#L27)；[Pi 10–18](../legacy/pi/packages/coding-agent/src/core/tools/ls.ts#L10)、[53–63](../legacy/pi/packages/coding-agent/src/core/tools/ls.ts#L53) |
| `powershell` | **一致**：与 bash 同模板，仅 shell 名为 `PowerShell` | `Execute PowerShell commands` | S（可关闭） | [Rust 106–128](../plugins/tools/pi-plugin-powershell/src/lib.rs#L106) 复用 [shell_tool_spec 105–140](../plugins/tools/pi-plugin-bash/src/lib.rs#L105)；[Pi 27–30](../legacy/pi/packages/coding-agent/src/core/tools/powershell.ts#L27)、[59–76](../legacy/pi/packages/coding-agent/src/core/tools/powershell.ts#L59) 复用 [bash.ts 272–288](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L272) |

guidelines 原文（双方相同）：

- **R**：`Use read to examine files instead of cat or sed.`
- **S**：`You can inspect PI_* environment variables for current model and session details.`
- **E1**：`Use edit for targeted changes. Batch changes to one file in a single call.`
- **E2**：`Each edits[].oldText must exactly and uniquely match the original file. Keep matches short and non-overlapping; merge nearby changes without including large unchanged regions.`
- **W**：`Use write only for new files or complete rewrites.`

补充：

- shell 的 S 由 `exposeSessionEnvironment` / `expose_session_environment` 控制，两侧默认 true，关闭时不贡献该规则（上表 shell 源码）。Pi 的 `undefined` 与 Rust 的空数组是表示形式不同，不是多出/少了一句规则。
- `grep/find/ls` 的 Pi contribution 常量含空 guidelines，但工具定义未另设 `promptGuidelines`；Rust 写入空向量。有效贡献均为空。
- `hashline_edit` **高（启用时的额外契约）**：description 为 `Apply stale-safe replace/prepend/append edits using LINE#HASH anchors from read(hashline=true).`，**没有自身 snippet / guidelines**。它直接调用 `spec()`，其默认值为 `prompt_snippet: None`、空 guidelines；不是在这个模块额外注入一段长编辑规则。[Rust 65–74](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L65)、[helper 12–33](../crates/pi-tool-support/src/lib.rs#L12)。Pi 的内置 `ToolName` / `allToolNames` 仅列前述 8 项，无 hashline：[index.ts 95–105](../legacy/pi/packages/coding-agent/src/core/tools/index.ts#L95)。

## 2. 输入 schema：必填、默认值、额外字段

### 2.1 共有的原始工具 schema

| 工具 | 必填 | 可选及执行默认 | 本次比较 |
| --- | --- | --- | --- |
| `read` | `path: string` | `offset: number` → 1；`limit: number` → 不额外限制选取范围，再受 2000 行/50KB 输出上限约束 | **一致**；不要把输出上限写成 `limit` 的 JSON Schema `default` |
| `bash` | `command: string` | `timeout: number` → 无默认超时 | **一致** |
| `edit` | `path: string`、`edits: array`；每项必填 `oldText: string`、`newText: string` | 无可选声明参数 | **一致**；两侧都未在 schema 写 `minItems: 1`，但执行拒绝空数组 |
| `write` | `path: string`、`content: string` | 无 | **一致** |
| `grep` | `pattern: string` | `path: string` → cwd；`glob: string` → 无；`ignoreCase: boolean` → false；`literal: boolean` → false；`context: number` → 0；`limit: number` → 100 | **一致** |
| `find` | `pattern: string` | `path: string` → cwd；`limit: number` → 1000 | **一致** |
| `ls` | 无 | `path: string` → cwd；`limit: number` → 500 | **一致** |
| `powershell` | `command: string` | `timeout: number` → 无默认超时 | **一致**，复用 bash schema |

schema 证据见第 1 节对应行号；执行默认值见 [Rust read 151–159](../plugins/tools/pi-plugin-read/src/lib.rs#L151)、[grep 117–140](../plugins/tools/pi-plugin-grep/src/lib.rs#L117)、[find 52–64](../plugins/tools/pi-plugin-find/src/lib.rs#L52)、[ls 48–53](../plugins/tools/pi-plugin-ls/src/lib.rs#L48)，及 [Pi read 297–316](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L297)、[grep 137–150](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L137)、[find 112–114](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L112)、[ls 82–84](../legacy/pi/packages/coding-agent/src/core/tools/ls.ts#L82)。

共同细节（**一致**）：

- 这些数值参数声明为 **`number`，不是 `integer`**；原始 schema 没有 `minimum`、`maximum` 或 `default`。默认值主要写在 description / execute 中。
- `grep/find/ls.limit` 执行时向下取整、最小为 1；`grep.context` 取整并限制到非负安全整数范围。Rust [helper 237–254](../crates/pi-tool-support/src/lib.rs#L237)、[grep 131–135](../plugins/tools/pi-plugin-grep/src/lib.rs#L131)；Pi 见上列执行行号。
- `timeout` 在执行时要求有限、> 0，最大 `2147483.647` 秒；schema 本身没表达该范围。[Rust 176–192](../plugins/tools/pi-plugin-bash/src/lib.rs#L176)、[Pi 21–36](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L21)。
- 8 个共有工具的原始 input schema **均未设置 `additionalProperties: false`**（包括 `edit` 的 item）。Rust 的 `spec()` 直接保存 parameters，不暗中补这一约束：[helper 12–23](../crates/pi-tool-support/src/lib.rs#L12)。这里比较的是工具自身声明，**不等同于供应商 strict 模式最终发出的 schema 或运行时验证策略**。
- `read/bash/edit/write/powershell` 都请求 JSON Schema constrained sampling、`strict: prefer`；`grep/find/ls` 两侧工具定义均未请求这一选项。见第 1 节各定义及 helper 默认值。

### 2.2 额外的 hashline schema 与旧形状兼容

- **高（仅启用时）**：Rust 配置工具给 `read` 和 `grep` 各加一个可选 `hashline: boolean`，无字段 description、无 schema default；调用未传则 false。`ReadTool` / `GrepTool` 裸默认实现不公布该字段。Pi 对应 schema 没有它。[read 109–159](../plugins/tools/pi-plugin-read/src/lib.rs#L109)、[grep 91–140](../plugins/tools/pi-plugin-grep/src/lib.rs#L91)。Coding 在 feature 开启时配置这两者并注册 `hashline_edit`：[composition.rs 209–221](../domains/coding/src/composition.rs#L209)。
- **高（仅启用时）**：`hashline_edit` 必填 `path: string`、`edits: array`；每项只有 `op` 必填，枚举为 `replace/prepend/append`；`pos/end` 可选 string，`lines` 为 `{}`（任意 JSON 类型）。顶层和每个 edit **均有 `additionalProperties: false`**。[65–74](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L65)。
- **中（schema 外的执行条件）**：hashline 的空 edits 会失败；replace 还要求 pos，prepend/append 未传 pos 时指向首/末行，end 未传时等于 start。lines 未传/null 表示空行列表，字符串按行拆，数组元素可转为字符串。这些条件未完整编码在 schema/description 中。[93–95](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L93)、[131–149](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L131)、[257–270](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L257)。
- **中（未公开旧参数的兼容行为）**：两侧 `edit.prepareArguments` 都接受 JSON 字符串/单对象 edits 以及顶层旧 `oldText/newText`。但若同时提供合法 `edits[]` 和顶层旧 pair，Pi 会把旧 pair **追加**到数组；Rust 检测到已有 edits 后会移除顶层 pair、**不追加**。它们不属于公开 schema，却会改变模型发出混合旧/新形状时的实际修改数。[Rust 74–108](../plugins/tools/pi-plugin-edit/src/lib.rs#L74)、[Pi 98–128](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L98)。

### 2.3 outputSchema / structuredContent

**一致**：`read` 两侧声明 `string | {type: "image", data: string, mimeType: string, note: string}`；`bash/powershell` 声明 `{output: string, truncated: boolean, full_output_path?: string, exit_code: number, wall_time_seconds: number}`。其他共有工具均未声明 outputSchema。

- [Rust read 85–94](../plugins/tools/pi-plugin-read/src/lib.rs#L85) ↔ [Pi read 28–38](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L28)。
- [Rust shell 128–138](../plugins/tools/pi-plugin-bash/src/lib.rs#L128) ↔ [Pi bash 51–63](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L51)。

不要把 structuredContent 或 `details.diff` 自动当作额外一段普通模型提示：Pi 源码明确 structured shell output 面向程序化调用，调用方决定多少送给模型。本节只确认工具返回契约；不重复主报告对 codemode / provider 投影的分析。

## 3. 逐工具的重要可见结果差异

### 3.1 read

- **一致：普通文本主路径**。两侧输出原文本；2000 行/50KB 截断；超限首行提示 `Use bash: sed ... | head -c 51200`；分页提示 `Use offset=... to continue`；offset 超出文件时报错；都给 structured 文本结果。[Rust 193–290](../plugins/tools/pi-plugin-read/src/lib.rs#L193) ↔ [Pi 297–355](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L297)。
- **中：图像处理策略不完全相同**。Rust read 只把 `auto_resize_images` 传给默认 `ImagePolicy`，固定默认 2000×2000、编码后 4,718,592 字节门槛；Pi read 使用 `ctx.model.inputLimits.images.resize`，否则使用 fallback options。特定模型下可见图像分辨率/坐标提示可能不同。[Rust read 160–192](../plugins/tools/pi-plugin-read/src/lib.rs#L160)、[ImagePolicy 20–27](../crates/pi-media/src/image.rs#L20) ↔ [Pi read 275–295](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L275)。这不是整个 provider 图片链路的最终行为结论。
- **中：图像处理失败结果**。Rust 将 `process_image` 错误变成工具执行错误；Pi `processed.ok === false` 返回成功文本 `Read image file [...]` + `[Image omitted: ...]`，没有图片 block。[Rust read 162–175](../plugins/tools/pi-plugin-read/src/lib.rs#L162) ↔ [Pi read 282–284](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L282)、[image-process.ts 72–93](../legacy/pi/packages/coding-agent/src/utils/image-process.ts#L72)。
- **实现差异、非普通文案差异**：Rust 整文件读入后切片，Pi 常规整数区间用流式读取并限制保存内容。这会影响大文件资源开销，但不应写成“2000 行文本上限不同”。[Rust 160–198](../plugins/tools/pi-plugin-read/src/lib.rs#L160) ↔ [Pi read 65–91](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L65)、[138–226](../legacy/pi/packages/coding-agent/src/core/tools/read.ts#L138)。
- hashline 的额外输出见 3.9；默认不开启。

### 3.2 bash

- **一致：主要结果契约**。普通返回为合并输出；空输出 `(no output)`；非零退出追加 `Command exited with code N` 并标记 error；尾部截断带 `Full output: ...`。structured output 单独允许最多 1MiB，不是模型直接文本的 50KB；两侧都保留 exit_code 和按 0.1 秒四舍五入的 wall time。[Rust 222–268](../plugins/tools/pi-plugin-bash/src/lib.rs#L222)、[370–428](../plugins/tools/pi-plugin-bash/src/lib.rs#L370) ↔ [Pi 366–442](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L366)。
- **中：背景进程导致超时的诊断**。Pi 若 shell 已退出、但其子进程继续占着输出，超时错误可附上 shell 真实退出码/耗时和背景进程解释；Rust 工具层只附 `Command timed out after ... seconds`。[Pi 405–413](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L405) ↔ [Rust 236–241](../plugins/tools/pi-plugin-bash/src/lib.rs#L236)。因此模型看到的失败上下文不完全相同。
- **低：无输出的取消/超时**。Rust 在检查取消/超时之前使用 `(no output)` fallback；Pi 在异常支路 `formatOutput(snapshot, "")`，没有这行。[Rust 223–245](../plugins/tools/pi-plugin-bash/src/lib.rs#L223) ↔ [Pi 399–414](../legacy/pi/packages/coding-agent/src/core/tools/bash.ts#L399)。

### 3.3 edit

- **一致**：对原文件一次匹配多个 edits、要求唯一/非重叠、成功文本 `Successfully replaced N block(s) in PATH.`，details 包含 diff/patch/firstChangedLine。[Rust 195–213](../plugins/tools/pi-plugin-edit/src/lib.rs#L195)、[255–324](../plugins/tools/pi-plugin-edit/src/lib.rs#L255) ↔ [Pi edit.ts 185–212](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L185)、[edit-diff.ts 343–405](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L343)。
- **高：fuzzy 匹配可额外改写未触及行**。Rust 任一 edit 需要 fuzzy 后，以 `fuzzy(content)` 整个文本作为输出基底；尾空白/NFKC/智能引号等规范化可能扩散到未编辑行。当前 Pi 把替换扩大到受影响行后叠回原文，未触及行保留原内容。[Rust 230–246](../plugins/tools/pi-plugin-edit/src/lib.rs#L230)、[267–324](../plugins/tools/pi-plugin-edit/src/lib.rs#L267) ↔ [Pi 166–214](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L166)、[395–405](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L395)。这会影响后续 read/diff，不能仅因 description 一样就声称安全性完全一致。
- **高：混合换行保留**。Rust 依据第一种检测到的 LF/CRLF 选择统一 ending，最后重写整个文件换行；Pi 按原始替换区间拼接，保留未修改区间的换行，新文本使用该位置所在行的 ending（含纯 CR 场景）。[Rust 166–189](../plugins/tools/pi-plugin-edit/src/lib.rs#L166) ↔ [Pi edit.ts 197–201](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L197)、[edit-diff.ts 15–67](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L15)。
- **中：唯一性判定不同**。Pi 即使找到 exact match，也在 fuzzy 规范化空间统计重复；Rust 没有触发 fuzzy 时只统计 exact occurrences。例如原文含 ASCII 引号和智能引号的两段近似文本，Rust 可能允许 exact 唯一替换，而 Pi 要求更明确上下文。[Rust 267–295](../plugins/tools/pi-plugin-edit/src/lib.rs#L267) ↔ [Pi 290–293](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L290)、[359–373](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L359)。
- **中：声明外限制**。Rust 拒绝 symlink/非普通文件、非 UTF-8 文件以及超过 10MiB 的原文件/新文本/结果，并在原子替换前校验快照。Pi 默认通过 fs access/readFile/writeFile，没有同样的 10MiB/非 symlink 限制，读文本用 Buffer UTF-8 解码。[Rust 139–164](../plugins/tools/pi-plugin-edit/src/lib.rs#L139)、[189–205](../plugins/tools/pi-plugin-edit/src/lib.rs#L189)、[helper 10](../crates/pi-tool-support/src/lib.rs#L10)、[360–403](../crates/pi-tool-support/src/lib.rs#L360) ↔ [Pi 87–91](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L87)、[169–201](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L169)。
- **低：错误文案**。重复/不存在/空 oldText/no-op 的具体文本不都相同；Pi 某些分支多有 “Please provide more context” 等恢复提示。[Rust 133–140](../plugins/tools/pi-plugin-edit/src/lib.rs#L133)、[282–321](../plugins/tools/pi-plugin-edit/src/lib.rs#L282) ↔ [Pi edit-diff.ts 296–331](../legacy/pi/packages/coding-agent/src/core/tools/edit-diff.ts#L296)。

### 3.4 write

- **低：成功文本不一致**。Rust：`Successfully wrote N bytes to PATH`；Pi：`Successfully wrote to PATH`。[Rust 68–71](../plugins/tools/pi-plugin-write/src/lib.rs#L68) ↔ [Pi 84–87](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L84)。
- **中：大小上限未在 prompt/schema 声明**。Rust content 超过 10MiB 会失败；Pi 默认写入路径无这一上限。[Rust 53–56](../plugins/tools/pi-plugin-write/src/lib.rs#L53)、[helper 10](../crates/pi-tool-support/src/lib.rs#L10) ↔ [Pi 75–87](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L75)。
- **中：写入方式/并发保证不完全相同**。Rust 使用同目录临时文件 + sync + persist 替换；Pi write 与 edit 共享 file-mutation queue，默认 fs.writeFile 写入。Rust 此 write 模块未使用 edit 的 file_queue。它们不能被概括成相同的文件对象/并发语义；本次没有做 symlink、权限或并发运行验证。[Rust 64–83](../plugins/tools/pi-plugin-write/src/lib.rs#L64)、[edit 私有 queue 215–228](../plugins/tools/pi-plugin-edit/src/lib.rs#L215) ↔ [Pi write 33–35](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L33)、[64–88](../legacy/pi/packages/coding-agent/src/core/tools/write.ts#L64)、[edit 158](../legacy/pi/packages/coding-agent/src/core/tools/edit.ts#L158)。

### 3.5 grep

- **一致**：默认 100 匹配、50KB、500 chars；空结果 `No matches found`；基础格式 `path:line: text` 和上下文 `path-line- text`；超限/长行提示语主干相同。[Rust 159–194](../plugins/tools/pi-plugin-grep/src/lib.rs#L159)、[367–389](../plugins/tools/pi-plugin-grep/src/lib.rs#L367) ↔ [Pi 220–235](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L220)、[285–323](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L285)。
- **中：恰好 N 个匹配也提示截断**。Rust 收满 limit 就停并把 `records.len() >= limit` 当作 matchLimitReached；Pi 当前要看到额外一个 match 才认定截断。因此只有 1 个命中、`limit: 1` 时，Rust 会提示增大 limit，Pi 不会。[Rust 292–299](../plugins/tools/pi-plugin-grep/src/lib.rs#L292)、[320–325](../plugins/tools/pi-plugin-grep/src/lib.rs#L320) ↔ [Pi 252–267](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L252)。
- **中：重叠上下文重复/行角色不同**。Rust 为每个 match 独立输出窗口，同一行可重复出现，别的命中行在当前窗口里也可被标成 context；Pi 直接消费 rg 的 match/context 事件，不按每个 match 重复展开。两个邻近匹配的输出长度和 50KB 触顶位置可能不同。[Rust 354–389](../plugins/tools/pi-plugin-grep/src/lib.rs#L354) ↔ [Pi 168–169](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L168)、[220–267](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L220)。
- **中：上下文不是同次搜索快照**。Rust 搜索后重读文件加载 context，文件在两阶段变化可导致结果与上下文不一致或二次读取失败；Pi 当前 context 由 rg 直接输出，已不再重读匹配文件。[Rust 396–448](../plugins/tools/pi-plugin-grep/src/lib.rs#L396) ↔ [Pi 53–64](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L53)、[168–169](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L168)。
- **低：字符处理**。Rust 删除行中全部 `\r`，Pi 只去行终止符；Rust 500 chars 按 Unicode scalar 计，Pi 按 UTF-16 code units 且不截断代理对。例如 300 个普通 supplementary-plane emoji，Rust 不因字符数截断，Pi 会。[Rust 460–485](../plugins/tools/pi-plugin-grep/src/lib.rs#L460) ↔ [Pi grep 223–226](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L223)、[truncate.ts 268–284](../legacy/pi/packages/coding-agent/src/core/tools/truncate.ts#L268)。
- **中：依赖不可用时的结果**。Pi 默认会 ensure `rg`，失败报 “not available and could not be downloaded”；Rust 用 grep/ignore crate，不存在同样的外部 rg 可用性错误。[Rust imports 1–4](../plugins/tools/pi-plugin-grep/src/lib.rs#L1)、[197–210](../plugins/tools/pi-plugin-grep/src/lib.rs#L197) ↔ [Pi 129–135](../legacy/pi/packages/coding-agent/src/core/tools/grep.ts#L129)。本次没有触发下载。

### 3.6 find

- **一致**：基本相对路径输出、隐藏文件、目录尾 `/`、空结果文本、1000/50KB 上限主干。Rust 与 Pi 当前都对 git 仓库内/外的 ignore 行为有处理。[Rust 64–132](../plugins/tools/pi-plugin-find/src/lib.rs#L64) ↔ [Pi 187–203](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L187)、[312–347](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L312)。不据此保证全部 ignore/glob 边界等价。
- **高：相对含 `/` glob 的锚点不同**。Rust 对 `src/*.ts` 变成 `**/src/*.ts` 并匹配绝对路径，可能同时命中 `src/a.ts` 与 `pkg/src/b.ts`；当前 Pi 将相对路径 pattern 锚到搜索根，只希望后者被 `**/src/*.ts` 命中。双方 schema 虽都写 “relative to the search directory”，Rust 运行语义并未完全跟上。[Rust 85–91](../plugins/tools/pi-plugin-find/src/lib.rs#L85)、[141–156](../plugins/tools/pi-plugin-find/src/lib.rs#L141) ↔ [Pi 205–248](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L205)。
- **中：smart case 不同**。Pi 使用 fd 的 smart case：全小写 pattern 不区分大小写；Windows path matcher 也传 `nocase`。Rust GlobBuilder 只设置 literal_separator，没有设置大小写不敏感，保持大小写敏感匹配。因此小写 `*.ts` 对大小写不同扩展名的结果可不同。[Rust 141–156](../plugins/tools/pi-plugin-find/src/lib.rs#L141) ↔ [Pi 209–246](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L209)。
- **中：恰好 limit 的误报**。Rust 达到 N 条就停，`entries.len() >= limit` 一律提示 limit reached；Pi 请求 N+1 并在额外一条出现后才提示。[Rust 98–121](../plugins/tools/pi-plugin-find/src/lib.rs#L98) ↔ [Pi 237–248](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L237)、[279–292](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L279)。
- **中：后端不同**。Rust 内置 WalkBuilder + GlobBuilder；Pi 默认调用外部 fd，无法 ensure 时会报可用性错误。不能把两侧搜索顺序或所有平台边界当作已经一致。[Rust 1–3](../plugins/tools/pi-plugin-find/src/lib.rs#L1)、[66–103](../plugins/tools/pi-plugin-find/src/lib.rs#L66) ↔ [Pi 176–185](../legacy/pi/packages/coding-agent/src/core/tools/find.ts#L176)。

### 3.7 ls

- **一致**：默认 500/50KB，包含隐藏文件，目录 `/`，空目录 `(empty directory)`，路径不存在/非目录错误主干，条目超限续查提示。[Rust 48–124](../plugins/tools/pi-plugin-ls/src/lib.rs#L48) ↔ [Pi 82–160](../legacy/pi/packages/coding-agent/src/core/tools/ls.ts#L82)。
- **低：非简单 ASCII 名称排序可能不同**。Rust 对 lowercase 字符串做 `cmp`；Pi 对 lowercase 字符串做 `localeCompare`。二者不能被宣称完全相同的本地化排序。[Rust 77–81](../plugins/tools/pi-plugin-ls/src/lib.rs#L77) ↔ [Pi 108–109](../legacy/pi/packages/coding-agent/src/core/tools/ls.ts#L108)。

### 3.8 powershell

- **一致**：prompt/schema/structured output 复用 shell；UTF-8 输出前缀、Windows PowerShell 5.1 的 native stderr exit-code 修复、尾部反引号不加 suffix 的判断都已在两侧实现。[Rust 14–16](../plugins/tools/pi-plugin-powershell/src/lib.rs#L14)、[115–159](../plugins/tools/pi-plugin-powershell/src/lib.rs#L115) ↔ [Pi 17–25](../legacy/pi/packages/coding-agent/src/core/tools/powershell.ts#L17)、[41–76](../legacy/pi/packages/coding-agent/src/core/tools/powershell.ts#L41)。
- **一致**：非 Windows 都报 `The powershell tool is only available on Windows.`；Windows 都优先 pwsh.exe/PowerShell 7，再 powershell.exe。[Rust 162–180](../plugins/tools/pi-plugin-powershell/src/lib.rs#L162) ↔ [Pi shell.ts 141–168](../legacy/pi/packages/coding-agent/src/utils/shell.ts#L141)。不要把“模块存在/已注册”解释成 macOS 可以执行。
- **中/低继承差异**：共享的 timeout 诊断/空输出错误文本差异同 3.2。未发现 PowerShell 自身额外 prompt 差异。

### 3.9 hashline_edit（Rust 可选扩展，Pi core/tools 无对应物）

- **高（仅启用时）**：模型可以改用 `LINE#HASH` 锚点，而不是完整 oldText。`read(hashline=true)` 会输出 `LINE#HASH:text`；编辑时验证锚点，过期会返回 `hash mismatch ... Re-read the file.`；拒绝重叠/no-op，成功只给文本 `Successfully applied hashline edits to PATH.`，没有 edit 那样的 diff details。[read 216–237](../plugins/tools/pi-plugin-read/src/lib.rs#L216)、[hashline-edit 165–222](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L165)、[239–255](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L239)。这是扩展调用契约，不能逐项标作与 Pi “一致”。
- **中：本扩展内部还有 grep/read 锚点不通用问题**。`grep(hashline=true)` 用本模块的乘数 hash 和 A–Z 两字符；`read` 与 `hashline_edit` 用 `pi_tool_support::hashline_tag` 的 xxhash/16 字符表。不能将 grep 的锚点直接视为可供 hashline_edit 使用。例：第 1 行 `a`，grep 算法产生 `1#DT`，而 hashline_edit 锚点 regex 的字符表不含 D。工具 description 正确要求来自 **read**，不要擅自扩大承诺。[grep 464–498](../plugins/tools/pi-plugin-grep/src/lib.rs#L464) ↔ [helper 334–357](../crates/pi-tool-support/src/lib.rs#L334)、[hashline-edit 226–252](../plugins/tools/pi-plugin-hashline-edit/src/lib.rs#L226)。

## 4. 如何表述“prompt 有没有区别”

建议表述：

> 当前源码中，共有内置工具的静态提示词和基础 schema 已高度对齐（本次逐项检查为一致），包含 read、bash、edit、write、grep、find、ls、powershell。默认关闭的 hashline 是 Rust 额外契约。仍有重要的执行结果/文件修改语义差异，尤其 edit、find、grep；因此“工具提示词一样”不等于“工具行为完全一样”。完整每轮 prompt 还需结合 active tools、系统 prompt 组装、插件和 provider 处理来看。

本次仅新增研究文档；未修改产品源码、未提交、未访问远端服务、未运行 cargo 全量或产品执行测试。结论依据当前本地源代码与直接调用链；文中的小例子为源码推导，不冒充双端执行测试结果。按主侧要求在证据足够后停止扩展排查，未再运行文档校验命令。
