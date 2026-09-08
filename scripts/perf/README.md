# 一条命令生成 Pi 性能看板

在仓库根目录执行：

```bash
./scripts/bench-perf --open
```

脚本构建当前 Rust release，顺序测试 Rust 与本地 TypeScript Pi，生成自包含的 HTML
看板并打开浏览器。看板无需服务端、CDN 或额外前端依赖，可以离线保存和转发。

快速检查整个流程：

```bash
./scripts/bench-perf --quick --open
```

快速模式只用于冒烟验证；默认完整模式更适合性能比较。首次 Rust 编译的时间不计入测试。

## 前置条件

- macOS 或 Linux；Node.js >= 22.19；仓库固定的 Rust 工具链；系统提供 `ps`。
- 本地 Pi oracle 默认位于 `legacy/pi`，也可用 `--ts-root /path/to/pi` 指定。
- TS 依赖需安装：`npm ci --prefix legacy/pi`。
- CLI 默认使用 Pi 的官方打包入口 `packages/coding-agent/dist/bundle/cli.js`。
  这是**现有构建**；脚本默认不会重新编译 TS。结果记录 oracle commit、脏工作区状态、
  入口类型、文件时间和 SHA-256。确认该构建对应你希望比较的 TS 版本。
- 如需刷新 TS 构建，执行 `./scripts/bench-perf --build-ts --open`，它会先运行上游
  `npm run build:offline`。上游依赖或类型检查失败时，脚本退出非零并保留错误看板。
  `--suite session` 直接使用 TS 源码和上游基准数据生成器，不依赖编译后的 CLI。

本仓库的 `legacy/` 不受 Git 跟踪。新环境需要先准备本地 Pi checkout。
只有 Rust 环境时，可执行 `./scripts/bench-perf --backend rust --open`；
看板中的 TS 列保持为空。

## 输出

每次运行创建一个独立目录，保留历史，不覆盖上一轮原始数据：

```text
target/perf/
  latest.html                    最新报告的自包含副本
  <时间戳-随机标识>/
    index.html                   本次交互式性能看板
    results.json                 原始样本、每轮数据、参数、环境、版本和错误
    summary.csv                  汇总：n / mean / P50 / P95 / min / max
  rust-build.json                Rust 构建产物定位信息，供 --skip-build 使用
```

看板支持切换 P50 / P95 / 平均值，按类别与规模筛选，搜索指标，查看原始样本分布、
每轮中位数，并下载 JSON。时间自动显示为 ms / μs，内存显示为 MiB。
每行柱形图单独归一化，不能跨指标比较柱长。`TS / Rust` 仅表示当前指标的数值比例。

如果没有自动打开浏览器，可直接打开 `target/perf/latest.html`。
使用 `--output /path/to/results` 可指定结果父目录；相对路径相对于仓库根目录。

## 测试内容与默认参数

| 内容 | 数据 / 参数 |
| --- | --- |
| 分散读取 100 条、最新 50 条、完整分支查询 | 1k / 10k / 100k 条 user-message |
| Fork 当前分支 | 1k / 10k 条，每次新建等量源会话 |
| 会话目录与空会话创建 | 10k 个内存会话目录；创建使用独立空仓库 |
| 会话实际 RSS 增量 | 1k / 10k / 100k 条，独立进程加载前后读数 |
| CLI `--version`、`--help`、RPC `get_state` | 两个实现各 5 次预热 + 50 次测量 |
| RPC 就绪后总 RSS | 前 5 个测量进程，成功响应后等待 250 ms |
| 会话采样 | 3 轮独立进程；每轮 10 个预热 + 30 个测量样本 |
| 微读取批次 | get100 / latest50 每个样本包含 1000 次操作 |
| 消息文本 | 确定性 ID 前缀 + ASCII 填充，共 256 字节 |

Rust 与 TS 顺序运行，轮次之间交替先后顺序。read 与 fork 使用不同的新进程，
避免已关闭但尚未释放的 TS 大会话影响后续 fork。fixture 准备、正确性校验和清理
不计入 fork/create 的耗时。源码 worker 的加载时间不计入会话指标。

成功判定包含返回数量、fork 元数据和消息数检查；CLI 要求正常退出且有输出。
RPC 要求返回匹配请求 ID 的成功响应，收到响应前保持 stdin 打开。
退出过早、失败响应、无效样本或超时会使整轮标记失败，退出码为 1；
Ctrl-C 标记取消并退出 130。报告保留已经收集的样本，缺失结果不会记成零。
超时会终止该 worker 的进程组。临时运行目录结束后自动清理，报告目录保留。

## 口径限制

- **这是 API 工作负载对比，不是等量输出的语言微基准。**
  Rust get100 是 100 次单条调用并返回拥有所有权的记录；TS 是一次批量调用并返回引用。
  Rust latest50 复制完整记录，TS 返回引用。
  Rust 完整分支查询返回消息 payload；TS `scanBranchStructure` 只构造结构元数据。
  看板始终保留这些区别，不计算跨不同指标的综合跑分。
- TS fork 使用上游当前 benchmark helper，包括其源会话配置与状态初始化。
  Rust 使用当前公开的内存 SessionRepo API；两者不是相同内部数据结构。
- P50/P95 使用 nearest-rank 分位数；同一指标的所有轮次样本合并统计，也保留每轮统计。
  微读取的样本是批次平均值，其 P95 **不是单次微操作尾延迟**。
- 已加载会话内存用同一进程的 `ps RSS_after - RSS_before`。
  TS 在两个检查点之前分别执行 3 次 GC。RSS 仍包含运行时和分配器行为，可出现小幅负增量。
  这里不是 `time -l` 的峰值 RSS，也不是精确活对象内存量。
  RPC 内存是就绪后的**总 RSS**，不减基线。
- CLI 启动包括进程创建与模块/动态库加载。不会清除系统文件缓存，所以不是磁盘冷启动。
  bundled、unbundled、自定义入口和不同构建不可当作同一基线；`--skip-build` 也可能使用旧 Rust 二进制。
  SHA-256 是记录的入口/worker 文件指纹，不覆盖全部动态依赖。
- 默认不向子进程转发 provider token 或 NODE_OPTIONS。CLI 使用临时 agent 配置与 cwd，
  没有真实模型请求。本测试不覆盖 LLM 网络响应、工具 I/O 或 TUI 渲染。
- 后台进程、温度、系统调度会影响绝对值。完整模式仍是本机观测，不是 CI 硬性性能门禁。
  有其他编译或重负载时，应等其结束再测。

## 常用命令

```bash
# 完整模式，自动打开看板
./scripts/bench-perf --open

# 冒烟验证
./scripts/bench-perf --quick --open

# 只测会话，增加轮次
./scripts/bench-perf --suite session --rounds 5 --samples 50

# 只测 CLI 的已有 release 构建
./scripts/bench-perf --suite cli --skip-build --cli-samples 100

# 复现开发入口口径（看板会标注 unbundled）
./scripts/bench-perf --suite cli --ts-cli unbundled

# 指定 oracle、数据规模与 payload
./scripts/bench-perf --ts-root /path/to/pi --sizes 10000,100000 \
  --fork-sizes 10000 --payload-bytes 1024 --output target/perf-large

# 无本地 TS checkout 时只测 Rust
./scripts/bench-perf --backend rust --open

# 查看全部选项
./scripts/bench-perf --help
```

脚本可从任意 cwd 调用，不包含某台机器的绝对路径。
完整默认命令已经覆盖全部指标；无需分别手工执行 Rust 与 TS worker。

## 维护与验证

入口是 `scripts/bench-perf`，编排与统计在 `scripts/perf/`，
Rust worker 在 `crates/pi-session/examples/perf_session.rs`。
产品库的生命周期、公开 API 和存储 schema 没有为 benchmark 修改。
TS worker 直接使用当前 oracle 下 `packages/agent/src/harness/session/testing/`
的种子生成与 fork helper；oracle 升级后应核实 helper 和 API 是否仍匹配。

```bash
node --test scripts/perf/perf.test.mjs
./scripts/bench-perf --quick
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```
