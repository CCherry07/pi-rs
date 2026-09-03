# LongMemEval-V2 对 `pi-rs` 记忆评测的参考

> 调研快照：2026-09-01。论文基线为
> [arXiv v1（2026-05-12）](https://arxiv.org/abs/2605.12493)，代码基线为官方仓库
> [`2cc8c54`](https://github.com/xiaowu0162/LongMemEval-V2/tree/2cc8c540bdb87fe6761629b585e727e1c4704520)，
> 数据基线为 Hugging Face
> [`f152293`](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/tree/f152293e235517d504809563c833d7190b8c713b)。
> 本文只使用 UCLA 作者的论文、项目页、官方代码和官方数据集卡。

## 结论

LongMemEval-V2（LME-V2）对 `pi-rs` 最有价值的不是把 451 道网页题原样跑一遍，而是它把记忆质量
拆成了一个可控的链路：**顺序摄入历史 → 只凭当前问题召回有界证据 → 固定 reader 作答 → 同时计算
正确率、延迟和上下文成本**。这个评测形态能把本地记忆召回质量与主 Agent 的规划、工具
执行能力分开，适合作为 `pi-rs` 的评测骨架。

但官方数据不能直接代表当前 `pi-rs` 语义记忆的质量。LME-V2 摄入的是带截图、可访问性树和动作的
完整网页轨迹；`pi-rs` 摄入的是用户批准、写入 session journal 的紧凑 `MemoryRecord`。前者同时评估
自动经验抽取、组织和检索，后者刻意只做显式记忆。最稳妥的路径是：

1. 直接复用 LME-V2 的任务分类、隔离协议、噪声分层、证据标注和准确率—延迟思路；
2. 用 `pi-rs` 自己的 session、scope、纠正、遗忘和显式授权语义重做数据；
3. 将来若实现“轨迹经验记忆”，再用独立 benchmark Adapter 接官方 LME-V2，不为适配评测而扩大
   生产插件 Interface。

官方论文和项目均来自 UCLA，代码与数据标为 Apache-2.0；论文当前仍是 arXiv preprint。
[来源：论文首页](https://arxiv.org/html/2605.12493)、
[官方项目页](https://xiaowu0162.github.io/longmemeval-v2/)、
[代码许可证](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/LICENSE)、
[数据集卡](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/README.md)

## 1. 它到底评什么

LME-V2 把“有经验的同事”应掌握的环境经验分成五类。公开数据把 premise awareness 表示成前三类
问题的 `-abs` 反事实变体，因此实际有七个 `question_type`。以下计数由固定 revision 的
[`questions.jsonl`](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/questions.jsonl)
逐行统计：451 题中 240 题来自 web 域，211 题来自 enterprise 域，29 题带问题截图。

| 核心能力 | 公开类型与题数 | 官方语义 | 对 `pi-rs` 的自然映射 |
| --- | ---: | --- | --- |
| Static state recall | `static-environment` 134，`static-environment-abs` 55 | 页面地标、布局、控件、状态差异 | 项目事实、用户偏好、稳定配置、精确代码标识符 |
| Dynamic state tracking | `dynamic-environment` 86，`dynamic-environment-abs` 41 | 动作前后状态变化、环境 world model | 事实修正、决策演进、`supersedes` 后只召回新值 |
| Workflow knowledge | `procedure` 74，`procedure-abs` 32 | 重复任务的可靠步骤 | `instruction`、`decision`、`summary` 类型的项目工作流 |
| Environment gotchas | `errors-gotchas` 29 | 本地反复出现的失败模式和规避方法 | 构建/测试陷阱、工具限制、失败后的安全恢复提示 |
| Premise awareness | 上述三个 `-abs` 类型，共 128 | 识别“别处成立、当前环境不成立”的前提 | 拒绝陈旧、已纠正、跨 scope 或根本不存在的记忆 |

五类定义和网页示例来自论文 §3.1；数据字段定义来自官方 schema。
[来源：论文 §3.1](https://arxiv.org/html/2605.12493#S3.SS1)、
[官方数据 schema](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/SCHEMA.md)

这里最值得借鉴的是 premise awareness。它不是简单输出 `UNKNOWN`：评测要求回答明确指出错误前提；
跟随错误前提作答、只说不知道、或者一边否认一边仍给出错误前提下的具体答案都会得 0。这比普通
“召回命中率”更接近生产中的陈旧记忆风险。
[来源：官方 abstention judge](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/qa_eval_metrics.py)、
[论文评测 rubric](https://arxiv.org/html/2605.12493#A5.SS1)

## 2. 数据是怎样构造的

### 2.1 轨迹来源

作者从 WebArena、WorkArena 和 WorkArena++ 的定制网页环境采集轨迹，覆盖 OneStopShop、CMS、
Postmill/Reddit 和 ServiceNow。每条轨迹包含任务目标、结果，以及按顺序排列的状态—动作记录；状态
含 URL、agent thought、accessibility tree 和截图。论文中的采集池为 599 条 WebArena 轨迹和
941 条 WorkArena/WorkArena++ 轨迹，平均每条 28.1 个状态，总成功率 52.0%。失败轨迹被保留，且
不少问题只能从失败轨迹中回答。
[来源：论文 §3.2 与 Appendix A.1](https://arxiv.org/html/2605.12493#S3.SS2)、
[公开 trajectory schema](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/SCHEMA.md)

### 2.2 问题和证据

四步构造流程是：采集轨迹、人工编写问题、标注答案轨迹、组装 haystack。问题由人类专家撰写，
并用四个强模型做无历史筛查，至少两个模型答错才保留，以降低参数知识直接命中的概率。Gotchas
问题带截图；premise awareness 问题从已有 static、dynamic、workflow 问题改造成错误前提版本。
普通问题平均需要 1.4 条答案轨迹，最多 5 条。
[来源：论文 §3.2 Question Annotation](https://arxiv.org/html/2605.12493#S3.SS2.SSS2)

答案轨迹先由 Codex 协助提出候选，再由人类验证。多跳问题为每一跳单独保存候选轨迹集合；静态题
要求截图中直接可见，动态题要求动作前后变化，流程题要求实际展示关键步骤。这样的“问题 → 所需
证据 hop → 可接受来源”标注，值得 `pi-rs` 原样借鉴，因为它允许把失败进一步区分为**没有召回证据**
和**召回了证据但 reader 读错**。
[来源：论文 Appendix A.3](https://arxiv.org/html/2605.12493#A3.SS3)

### 2.3 噪声层级

- Small：每个 domain 各有一个所有问题共享的 100-trajectory haystack；约 25M token。
- Medium：每题单独组装，通常为 500 条轨迹；约 115M token。公开数据实际范围为 387–500 条，
  平均约 498 条。
- 两层使用同一个最小答案 seed；Medium 添加 filler 时排除该题所有其他答案轨迹，避免随着规模增加
  意外增加“针”的数量。
- filler 强调任务族多样性、低答案重叠和约 1:1 成功/失败比例，最后确定性打乱顺序。

这是一种比随机塞无关文本更好的压力测试：干扰项与答案来自同一环境和相似任务族，容易产生“看似
相关但并非精确证据”的误召回。
[来源：论文 Appendix A.4](https://arxiv.org/html/2605.12493#A4.SS4)、
[公开 haystack schema](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/SCHEMA.md)

## 3. 评测协议和指标

### 3.1 Context gathering，而非直接完成任务

每个 backend 实现两个动作：`Insert(trajectory)` 和 `Query(question, optional_image)`。Harness 按
顺序插入该题的全部轨迹，然后只把问题文本、可选问题图片和一个不含语义的 invocation id 提供给
backend。`Query` 返回有序的 text/image context；harness 以 Qwen3.5-9B tokenizer 计算上下文并按
**item 前缀**截断到 200K token，固定 Qwen3.5-9B reader 再根据问题与 context 作答。
[来源：论文 §3.3](https://arxiv.org/html/2605.12493#S3.SS3)、
[官方 Memory API](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/memory.py)、
[官方 harness](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/harness.py)

当前官方代码有专门的 privacy regression：backend 不得看到 question id/type、gold answer、评测器、
原始 goal 或其他 benchmark metadata，只能看到 query input 和随机 invocation id。这个约束应直接
移植到 `pi-rs` 评测，防止评测 Adapter 从 fixture 元数据“作弊”。
[来源：官方 query privacy tests](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/tests/test_query_privacy.py)

### 3.2 正确率

结构化答案使用确定性 evaluator：归一化短语集合、保序短语、单选或多选；gotchas 和错误前提题使用
GPT-5.2 binary judge。所有题最终记 0/1，报告全量、非 premise、premise，以及按 static、dynamic、
procedure、gotchas 的 breakdown。Reader 输出 `UNKNOWN` 一律记错；错误前提题必须说明具体错误。
[来源：官方 scoring code](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/qa_eval_metrics.py)、
[官方聚合代码](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/harness.py#L958-L1027)

### 3.3 延迟、Token 和 LAFS

Harness 记录 `memory.query` 的平均、p50、p95、max 和总时间，并单独记录 context 截断前后 token、
reader prompt/completion token。官方 leaderboard 的主要延迟只取 `memory_query_avg_seconds`，不计
轨迹插入、reader 作答和 `post_query_hook`。
[来源：官方 harness 指标聚合](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/harness.py#L1383-L1526)、
[leaderboard 协议](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/leaderboard/README.md)

Leaderboard 用 LAFS（Latency-Aware Frontier Score）衡量准确率—延迟 Pareto frontier：在 1–200 秒
的对数均匀延迟预算上，积分“该预算内可达到的最高准确率”；提交分数是加入新 operating points 后
相对固定 baseline frontier 的增益。一个被现有点在准确率和延迟上同时支配的方法得 0。这个思想
值得复用，但 1–200 秒区间不适合 `pi-rs` 的 50ms 在线 recall SLA，必须用产品实测确定本地预算区间，
且不能把修改后的分数称为官方 LAFS。
[来源：官方 LAFS 实现](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/leaderboard/compute_lafs.py)

## 4. Baseline 给出的工程信号

论文主结果如下。正确率是 451 题全量 accuracy，延迟是平均 query latency；reader 固定为
Qwen3.5-9B。

| 方法 | Small accuracy / latency | Medium accuracy / latency | 工程含义 |
| --- | ---: | ---: | --- |
| No retrieval | 1.3% / 0s | 1.3% / 0s | 题目大体不能靠参数知识回答 |
| Query → raw slice | 42.8% / 0.1s | 38.1% / 0.1s | 原始细节检索很快，但随噪声增加退化 |
| Raw slice + trajectory notes | 51.0% / 0.2s | 45.9% / 0.3s | 高层经验与低层证据互补 |
| AgentRunbook-R | 58.6% / 26.9s | 57.0% / 25.8s | raw state、transition event、procedure/hint 三池明显更稳 |
| Vanilla Codex | 69.9% / 177.2s | 68.7% / 185.8s | agentic 文件探索准确，但太慢 |
| AgentRunbook-C | 74.9% / 108.3s | 70.1% / 139.9s | manifest、workflow 和 helper 将 coding-agent 搜索约束到有效路径 |

[来源：论文 Table 2](https://arxiv.org/html/2605.12493#S5.SS1)、
[官方项目结果表](https://xiaowu0162.github.io/longmemeval-v2/#results)

三个结论可以直接指导 `pi-rs`：

1. 只存紧凑摘要不够，仍要保留可追溯 evidence；只存原文也不够，工作流和 gotcha 需要高层组织。
2. Dynamic 问题需要显式的“前状态—动作—后状态”事件表示；把所有内容都压成同一种 `fact` 会损失
   状态变化语义。
3. Coding agent 适合做慢速、显式的深度记忆搜索，不适合默认 context hook。manifest 和小工具比让
   agent 无约束扫描全部历史更有效。

Pilot study 也支持第一点：即使直接给完整的答案轨迹，固定 reader 仍只有 59.6%；提供精确状态 slice
和 procedure/hint notes 后达到 82.5%，Codex 在 oracle 文件上迭代检查达到 89.7%。这里提升的不只是
“找对文件”，还有证据切片与呈现质量。
[来源：论文 Pilot Studies](https://arxiv.org/html/2605.12493#S3.SS4)

## 5. 2026/08 AgentRunbook-C V2 对自动捕获的参考

这是官方仓库在论文 v1 之后发布的 research update，不应与 LongMemEval-V2 benchmark 名称或论文
Table 2 的 AgentRunbook-C 混为一谈。V2 把 query controller 改成 OpenAI Agents SDK 上的轻量 harness，
核心只保留 shell（检索/执行）和 file editor（持久更新）两个工具。在官方 Small 结果里，这种更小的
orchestration 在低、中 reasoning 档显著降低了 query latency。
[来源：官方 AgentRunbook-C V2 更新](https://xiaowu0162.github.io/longmemeval-v2/agentrunbook-c-v2/)、
[轻量 controller 实现](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/oai_agents_sdk.py)

更值得 `pi-rs` 参考的是它的**无标签 post-query consolidation**：query 完成后，另一个 consolidation
agent 只读取当前问题、memory output、检索 trace 和被引用的本地 span，更新一份持久的
`LEARNED_RETRIEVAL_STRATEGY.md`；它明确不得使用 downstream reader 答案、evaluator score 或 aggregate
metrics。下一次 query 只能把该文件当作搜索线索，并必须用本次的直接 evidence 重新验证。这使“学习”
发生在 query 之后，不污染当前答案，也不借 gold label 作弊。
[来源：online-learning 实现](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/agentrunbook_online_learning.py)、
[consolidation 规则](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/assets/agentrunbook_online_learning/CONSOLIDATE_STRATEGY.md)

Consolidator 为候选经验保留四种 evidence status，而不是把每次结果都提升成“事实”：

| Evidence status | 含义 | 能否成为热路径正向证据 |
| --- | --- | --- |
| `directly_supported` | 同一 entity/view/page/section/pre-post state 的 span 直接证明目标 | 可以，但下次仍需重新验证当前 evidence |
| `contradicts_premise` | 同一 scope 的闭集或直接证据证明目标不存在、前提错误 | 可以作为反证线索，必须保留精确 scope |
| `near_match_only` | 只有相似页面、工作流、实体或状态 | 不可以；最多作为导航或对比 guard |
| `insufficient` | evidence 缺失、不完整、空 span 或不确定 | 不可以；默认不写，最多记录可复现搜索陷阱 |

它同时实施严格 admission policy：默认 no-op，通常一次最多新增一条；优先合并、修正或删除；一次成功
retrieval 不自动获得入库资格；one-off 值、最终答案和 option letter 不入库；约 80 行后只接受特别强的
候选。`Past Queries` 只能放 `directly_supported` 或精确 scope 的 `contradicts_premise`，另外两类只能在
确有复用价值时进入 `Strategies`，并且每条必须带 applicability condition、provenance 和
“do not reuse if” guard。实现还会校验 strategy 结构、快照哈希并在 consolidation 失败时回滚。
[来源：官方 admission/guard 规则](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/assets/agentrunbook_online_learning/CONSOLIDATE_STRATEGY.md)、
[strategy skeleton](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/assets/agentrunbook_online_learning/LEARNED_RETRIEVAL_STRATEGY_SKELETON.md)

这可以直接启发未来 `MemoryCandidate`，但不能直接照搬成 `MemoryRecord`：

1. Candidate 应先保存 `proposed_text + scope + evidence_status + provenance + applicability + guard`，而不是
   立即写 canonical memory。
2. `directly_supported` 和精确 `contradicts_premise` 才有资格进入“建议用户保存”的队列；
   `near_match_only`/`insufficient` 只能保留为内部搜索提示或直接丢弃。
3. 默认动作为 no-op，成功一次不是入库依据；重复出现、可迁移、证据直接且 scope 明确才建议捕获。
4. 用户确认后才把 Candidate 转成 `MemoryRecord`。自动 consolidation 可改进“怎么找”，不能绕开
   `pi-rs` 的显式授权边界。
5. Retrieval strategy 与用户事实应使用不同的 record family/存储层，避免把 Agent 自己的搜索启发
   伪装成用户批准事实。

还有一个评测陷阱：官方 harness 在 `memory.query` 计时结束后才调用 `post_query_hook`，Leaderboard
LAFS 又只使用平均 query latency，因此 online consolidation 的模型、工具和持久化成本不进入主延迟。
`pi-rs` 必须另报 capture/consolidation latency、token、失败率和写放大；对于顺序学习，还要固定并
轮换 question order seed，防止结果只来自一个有利的问题顺序。
[来源：V2 post-query hook](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/memory_modules/agentrunbook_c_v2.py#L605-L675)、
[harness 计时边界](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/harness.py#L555-L604)

## 6. 与当前 `pi-rs` 的映射

当前 [`LocalMemoryProvider`](../../plugins/features/pi-plugin-memory-local/src/storage/mod.rs) 提供本地
`apply` 和 `recall`；[`MemoryRecord`](../../plugins/features/pi-plugin-memory-local/src/types.rs) 是带 scope、kind、origin、evidence 和可选
`supersedes` 的用户批准语义记录。Context hook 最多召回 8 条、默认预算 1,200 token、50ms 超时，
并把结果作为不落盘的隐藏消息注入。这个产品模型与 LME-V2 的完整轨迹 experience memory 有意不同。

### 可以直接复用

| LME-V2 设计 | `pi-rs` 用法 |
| --- | --- |
| context gathering 与固定 reader | 单独测本地 `recall` 和 context rendering，再用固定 reader 测最终答案 |
| 五能力 taxonomy | 作为事实、更新、流程、gotcha、错误前提五类回归集 |
| Small/Medium 的固定 needle + 同域 filler | 每题保持同一答案记录，逐步增加相似项目记录和相似 session 干扰项 |
| 成功和失败轨迹同时存在 | 同时加入成功实践、失败尝试和“看似相关但错误”的记忆 |
| 问题—evidence hop—来源标注 | 为每题标出必须命中的 `record_id` 集和每个 evidence hop |
| query privacy | Provider 只能收到 query、scope 和 limit，不能收到 gold id、kind 或题型 |
| no-recall 与 oracle baseline | 分离“记忆无用”“召回失败”“reader 读错”三类问题 |
| accuracy + latency + context token | 每次改索引或 ranking 都同时报告质量和产品成本 |

### 需要改造

| LME-V2 原形 | 必须如何改 |
| --- | --- |
| `Insert(full trajectory)` | 测当前产品时改为重放 `pi.memory.v1` mutation；测试自动抽取时另建 benchmark Adapter，不伪装成用户批准记录 |
| 100/500 条网页轨迹 | 改为 100/500 个 session，另设 100/1K/10K 条 active record 的存储规模轴 |
| raw state / event / note 三池 | V1 先映射成原始 session evidence、更新事件、curated `MemoryRecord` 三层；不要都塞入 canonical record |
| 网页截图问题 | V1 改为 repo 约定、命令、文件布局、配置、工具错误；未来支持多模态 evidence 后再加入图片 |
| 200K context | 使用生产默认 1,200 token，并增加 300/600/1,200/2,400 token operating points |
| 官方 1–200s LAFS | 用本机实测的交互预算区间；同时保留 p95/p99 与 timeout rate，不只看平均值 |
| premise-awareness judge | 增加 superseded/tombstoned、跨项目 scope、互相冲突和无证据四种错误前提 |
| 英文单域问题 | 加入中英文改写、Rust symbol/path、大小写、连字符和用户原话/语义改写两种 query |

### 当前不适用

1. **不能把官方 451 题的分数当成当前本地记忆分数。** 当前实现不摄入网页状态、图片
   或 transition event，直接转换会同时改变数据和任务定义。
2. **不应为跑 benchmark 把本地 recall 扩成 trajectory store。** 如果以后要支持环境经验，建立
   独立 `ExperienceMemory`/trajectory index，再通过产品层组合；显式用户记忆仍保持小而深。
3. **AgentRunbook-C 不适合作为默认 recall hook。** 108–140 秒平均延迟与当前 50ms timeout 相差三个
   数量级；它更适合作为用户显式触发的“深度历史研究”工具或离线 consolidation。
4. **官方 LAFS 不能原样用于本地 SQLite。** 它的参考 frontier、硬件、模型和延迟区间都绑定官方
   web-agent 任务。

LME-V2 自己也明确指出，传统“聊天 → fact memory”的系统直接适配这类轨迹会表现不佳。
[来源：论文 Appendix C.1](https://arxiv.org/html/2605.12493#A6.SS1)

## 7. 建议建立两条 `pi-rs` 评测轨道

### Track A：Provider-only，可先落地

这条轨道不调用真实 LLM，直接构造 `MemoryMutation` 并调用现有 Interface：

1. 按 session 顺序 `apply` 记录、纠正和 tombstone；
2. 只把自然语言 query、允许的 scopes 和 limit 交给 `recall`；
3. 用 gold `record_id` / evidence-hop 集合计算 retrieval 指标；
4. 重放、重启和 rebuild 后重复查询，要求结果一致；
5. 对 SQLite FTS、未来 hybrid/vector 和 oracle 使用同一 fixture。

建议指标：

| 指标 | 它能发现什么 |
| --- | --- |
| Evidence Recall@K / all-hop success | 需要的记录是否都进入返回集合 |
| MRR 或 nDCG | 关键证据是否排在有限 token budget 前部 |
| Wrong-scope hit rate | user/project/session 是否泄漏 |
| Stale hit rate | 被纠正或遗忘的记录是否仍召回 |
| Distractor hit rate | 相似但错误的项目事实是否挤掉 needle |
| p50/p95/p99、timeout rate | 是否符合 context hook 的交互 SLA |
| Rendered token 与 evidence density | 1,200 token 内有多少真正有用证据 |
| Replay/rebuild equivalence | JSONL 事实源与派生 SQLite 是否一致 |

官方 LME-V2 主要用最终 answer accuracy；上面的 retrieval 指标是 `pi-rs` 应新增的，因为我们掌握自建
fixture 的 gold record ids。官方公开数据刻意移除了 answer-bearing labels，所以只用其公开 release
无法完整复现论文的 retrieval miss / reading error 分解。
[来源：数据集 Release Notes](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/README.md#release-notes)、
[论文错误分析](https://arxiv.org/html/2605.12493#A7.SS1)

### Track B：产品端到端

这条轨道经过 `AgentSessionRuntime`、可见 `memory` tool、session journal 和 context hook：

1. 用 scripted provider 重放“明确记住 / 明确不要记 / 修正 / 忘记”的多 session 对话；
2. 检查 tool call 和 canonical `pi.memory.v1` entry；
3. 新 session 中发出改写后的问题，固定 reader 只从注入 context 作答；
4. 退出重启、rebuild、branch/compact 后再次作答；
5. 再用少量真实模型跑相同数据，统计模型是否正确决定调用 memory tool。

除 LME-V2 的五类能力外，这条轨道必须增加它没有覆盖的产品安全指标：

- explicit-capture precision/recall：用户没有授权时不得自动写，明确授权时应写；
- secret retention rate：密码、token、私钥等必须为 0；
- scope isolation：跨项目、跨 session 不得越权召回；
- correction/forget convergence：重放顺序和并发不应复活旧值；
- prompt-injection robustness：记忆文本不能升级成 system/tool 指令；
- transient recall：召回消息不得写回 JSONL 或被再次自动记忆。

### 可选 Track C：官方 LME-V2 Adapter

只有在产品真正引入 trajectory experience memory 后再做。Adapter 应在 benchmark 边界实现官方
`insert/query`，输出 text/image evidence，并从 Rust backend 通过稳定的进程协议调用；不要让 Python
harness 的题型、gold answer 或 question id 穿过边界。运行报告必须写明官方 data revision、代码
commit、reader、judge、embedding/controller、硬件、并发数、context budget 和随机参数。

## 8. 已知局限和复现注意事项

1. **领域局限。** 数据只有英文、定制网页和 ServiceNow 环境，不覆盖 coding agent、shell、git、
   repo 约定或真实用户偏好。论文也明确将 coding/computer-use/domain-specific agents 列为未覆盖域。
   [来源：论文 Limitations](https://arxiv.org/html/2605.12493#A8.SS1)
2. **离线而非在线。** 它读取预先采集的轨迹，不测 Agent 行为因自己的记忆而改变后产生的分布漂移；
   也不测写入授权、删除、合规或生命周期恢复。
   [来源：论文 Limitations](https://arxiv.org/html/2605.12493#A8.SS1)
3. **测 context gathering，不测任务成功。** 固定 reader 能隔离 memory，但真实产品还受规划、工具调用
   和执行错误影响。
   [来源：论文 §3.3 与 Limitations](https://arxiv.org/html/2605.12493#S3.SS3)
4. **分数依赖 reader/judge。** 官方使用 Qwen3.5-9B reader、GPT-5.2 semantic judge，且 reader 是
   sampling evaluation；换模型、prompt 或硬件后的数字不能与论文表格直接横比。
   [来源：论文 Appendix A.5](https://arxiv.org/html/2605.12493#A5.SS1)
5. **公开数据没有 gold evidence provenance。** 公开文件移除了 answer-bearing labels、原始 task id、
   URL-pattern 标签和 annotation pipeline tags；公开评测能复现最终答案分数，但不能单独验证官方的
   exact trajectory recall。
   [来源：官方数据集卡](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/README.md#release-notes)
6. **Leaderboard 延迟不含 ingestion。** 自动生成 event/note 或 embedding 的插入成本可能很高，但
   LAFS 只使用平均 query latency；磁盘/内存占用和尾延迟也不进入主分数。
   [来源：官方 leaderboard README](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/leaderboard/README.md)
7. **Judge rubric 有取舍。** Gotchas 只要命中至少一个参考 insight 且不矛盾即可得 1，可能掩盖
   多点答案的不完整性；`pi-rs` 的安全 gotcha 应另加“所有必需点”严格分数。
   [来源：官方 gotchas judge](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/evaluation/qa_eval_metrics.py)
8. **论文与公开数据存在版本漂移。** 论文 v1 写的是 599 + 941 = 1,540 条采集轨迹，当前官方数据卡
   写 1,870 条；官方 downloader 支持 `--revision`，因此复现时必须固定 revision 和 checksum，不能
   只写“使用 main”。
   [来源：论文 Appendix A.1](https://arxiv.org/html/2605.12493#A1.SS1)、
   [数据集卡](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2/blob/f152293e235517d504809563c833d7190b8c713b/README.md)、
   [官方 downloader](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/data/download_data.py)
9. **不要复用论文 v1 中暴露 benchmark metadata 的旧 controller prompt。** 论文 Appendix C 的旧
   template 含 question id/type 和 original goals；当前官方代码已添加 privacy tests，要求 query 端
   只见问题与图片。`pi-rs` 应以当前代码的约束为准。
   [来源：论文旧 prompt](https://arxiv.org/html/2605.12493#A6.SS2)、
   [当前 privacy tests](https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/tests/test_query_privacy.py)

## 9. 对后续实施的具体建议

第一阶段不直接下载 7GB 多模态数据，也不先接向量库。先把 LME-V2 的评测思想做成一个小而稳定的
`pi-rs` regression suite：

1. 建立 60–100 道人工 gold cases，五类能力都覆盖，中英文各半；
2. 每题标注允许 scopes、必需 `record_id`/evidence hops、错误前提和严格答案；
3. 生成 100-session Small 与 500-session Medium，两层保持相同 needle；
4. 先跑 no-recall、oracle、当前 SQLite/FTS 三个 baseline；
5. 同时报 Provider retrieval、固定 reader answer、p95 latency、timeout、token、scope/stale/secret
   安全指标；
6. 有基线后再加入 hybrid/vector 或慢速 agentic search，用 adapted latency frontier 判断是否真的改进。

这能回答当前最重要的问题：`pi-rs` 是“没有找到正确记忆”、还是“找到了但呈现/reader 用错了”，
以及一次质量提升是否以不可接受的延迟、scope 泄漏或陈旧记忆为代价。等这套本地评测稳定后，官方
LME-V2 才适合作为未来 experience-memory 的外部泛化测试，而不是当前 V1 的发布门禁。
