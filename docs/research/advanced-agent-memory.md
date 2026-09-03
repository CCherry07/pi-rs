# 面向 `pi-rs` 的先进 Agent Memory 论文与实施路线

> 调研快照：2026-09-02。本文只采用论文原文、ACL Anthology、PMLR、NeurIPS Proceedings、
> OpenReview/ICLR、AAAI Proceedings 和 arXiv 作者稿等一手来源。论文结果来自各自不同的 benchmark、
> reader 和预算，不能横向当成统一排行榜。

## 结论

`pi-rs` 当前的方向并不落后：显式授权的 `MemoryRecord` 是可审计的语义记忆，Pi v4 JSONL 是事实来源，
SQLite/FTS5 与 dense/RRF 是可重建索引，本地 Provider 另有原始会话索引。最近论文真正值得引入的，不是
立刻把 canonical record 换成一张由 LLM 自动维护的图，而是补齐三类能力：

1. **先评测状态演化，而不只评测 Recall@k**：冲突更新、错误前提、选择性遗忘、多跳证据、延迟和
   上下文成本；主要参考 MemoryAgentBench 与 LongMemEval-V2。
2. **保留事实与事件的时间链，同时只向普通查询暴露当前有效状态**：主要参考 THEANINE 与 REMem。
3. **给 Agent 一个有预算的原始会话深搜通道，并把流程经验与用户事实分开**：主要参考 ReFind 与
   AWM。

A-MEM、HippoRAG 2 和 H-MEM 的图/层级结构适合做成 canonical records 之上的**派生索引**；在数据量
和评测尚未证明扁平 hybrid retrieval 是瓶颈前，不应成为默认写入路径。Memory-R1 的学习式
`ADD/UPDATE/DELETE/NOOP` 很有启发，但让模型直接改变用户的持久记忆，当前既不符合显式授权边界，
也缺少生产安全证据。

## 1. 与当前代码的对应关系

当前的核心数据和实现见
[`types.rs`](../../plugins/features/pi-plugin-memory-local/src/types.rs)、
[`storage/mod.rs`](../../plugins/features/pi-plugin-memory-local/src/storage/mod.rs) 和
[`ranking.rs`](../../plugins/features/pi-plugin-memory-local/src/ranking.rs)：

- `MemoryRecord` 已有 `scope`、`kind`、原始 session/entry/tool-call provenance、证据说明、写入时间和
  `supersedes`；`MemoryMutation` 只有显式 `Remember` 与 `Forget`。
- `LocalMemoryProvider` 的 `recall`/`apply` 服务于本地插件；canonical mutation 先进入 session journal，SQLite
  只是幂等、可重建的查询投影。
- FTS5/BM25 与 dense retrieval 已能融合；这解决的是候选召回，不等于解决时间、冲突、多跳或
  流程学习。
- 本地 session 索引保存可重建的逐条会话文本。这与 ReFind 的原始日志检索路线天然相容，并不要求
  自动把所有对话提升为永久语义记忆。

现有设计的明确缺口是：`recorded_at_ms` 只有记录时间，没有事实的有效时间；`supersedes` 能表达一条
替代边，但没有版本组、时间区间和因果关系；没有候选记忆、访问/效用统计、episode、workflow、图
关系或层级摘要。以下论文应被理解为对这些缺口的选择性补充，而不是一个必须整体照搬的新架构。

## 2. 论文地图

| 论文 | 状态 | 主要能力 | 对 `pi-rs` 的阶段 |
| --- | --- | --- | --- |
| REMem | ICLR 2026，同行评审 | 时间化 episode、图遍历、多步推理 | P1 实验 |
| MemoryAgentBench | ICLR 2026，同行评审 | 检索、测试时学习、长程理解、选择性遗忘 | P0 评测 |
| Memory-R1 | ACL 2026 Long，同行评审 | 学习式写入、更新、删除与使用 | P2 研究 |
| A-MEM | NeurIPS 2025，同行评审 | 原子笔记、链接、记忆演化 | P1 派生索引 |
| Agent Workflow Memory | ICML 2025，同行评审 | 从轨迹抽取可复用流程 | P0/P1 候选流程 |
| THEANINE | NAACL 2025 Long，同行评审 | 时间与因果链、陈旧事实的历史语境 | P0 时间视图 |
| HippoRAG 2 | ICML 2025，同行评审 | 知识图 + Personalized PageRank + 在线过滤 | P1 多跳实验 |
| H-MEM | EACL 2026 Long，同行评审 | 多层抽象与逐层路由 | P1/P2 扩容 |
| LongMemEval-V2 | arXiv 2026，**预印本** | 轨迹记忆、错误前提、多跳、成本评测 | P0 评测协议 |
| ReFind | arXiv 2026，**预印本** | Agent 控制的原始日志迭代搜索 | P0 深搜实验 |
| MemoryBank | AAAI 2024，同行评审 | 受遗忘曲线启发的衰减 | 仅作排序信号 |

## 3. 最值得跟进的 11 篇论文

### 3.1 REMem: Reasoning with Episodic Memory in Language Agents

- **出处与机构**：Yiheng Shu、Saisri Padmaja Jonnalagadda、Xiang Gao、Bernal Jiménez Gutiérrez、
  Weijian Qi、Kamalika Das、Huan Sun、Yu Su；The Ohio State University 与 Intuit AI Research；
  ICLR 2026 conference paper，已同行评审。
  [OpenReview 论文](https://openreview.net/pdf?id=fugnQxbvMm)
- **机制**：离线阶段把经历组织成 hybrid memory graph，节点同时包含 time-aware gist 和带时间范围的
  facts；在线阶段由 agentic retriever 使用检索、图探索、时间筛选/排序等工具迭代收集证据。
- **证据**：论文在四个 episodic-memory benchmark 上报告，相对当时强系统，episodic recollection
  和 reasoning 分别有 3.4 与 13.4 个绝对点提升，并改善不可回答问题的拒答。
- **局限**：建图和查询都依赖 LLM；端到端结果混合了索引器、工具策略和 reader 的能力，不能证明图
  本身优于 hybrid retrieval。其延迟也不适合每轮默认 recall。
- **代码映射**：保留 `MemoryRecord` 为原子事实；增加可重建的 `episode`、gist、fact-time 和 relation
  投影，所有派生节点指回 session entry。先实现有明确预算的 deep-search 工具，再考虑默认注入。

### 3.2 MemoryAgentBench: Evaluating Memory in LLM Agents via Incremental Multi-Turn Interactions

- **出处与机构**：Yuanzhe Hu、Yu Wang、Julian McAuley；University of California, San Diego；
  ICLR 2026 conference paper，已同行评审。
  [OpenReview 论文](https://openreview.net/pdf/73075b52c441b9966980a5928b47d073c9671992.pdf)、
  [arXiv 作者稿](https://arxiv.org/abs/2507.05257)
- **机制**：将长上下文转换为逐轮增量输入，并把记忆能力拆为 accurate retrieval、test-time
  learning、long-range understanding 和 selective forgetting；新增 EventQA 与
  FactConsolidation，其中后者同时测单跳和多跳冲突更新。
- **证据**：论文评测了上下文、RAG、外部 memory module 和工具型 agent；结论不是某一种架构全胜，
  而是现有方法没有同时掌握四种能力。
- **局限**：任务由多套已有数据转换而来，指标与上下文长度差异很大；“test-time learning”也不等于
  生产中可安全执行的流程学习。
- **代码映射**：最适合直接变成 `pi-memory-eval` 的 sequential fixture：先 `Remember(A)`，再纠正为 B，
  再 `Forget`，分别询问当前事实、历史事实和由两条当前事实组成的多跳问题；记录 Recall@k、最终答案、
  错误旧值泄漏和 mutation 一致性。

### 3.3 Memory-R1: Enhancing Large Language Model Agents to Manage and Utilize Memories via Reinforcement Learning

- **出处与机构**：Sikuan Yan 等 13 位作者；ACL 2026 Long，已同行评审。ACL 元数据完整列出作者，
  本笔记不在未逐项核对论文首页的情况下猜测多机构对应关系。
  [ACL Anthology](https://aclanthology.org/2026.acl-long.583/)
- **机制**：拆成 Memory Manager 与 Answer Agent；前者学习 `ADD/UPDATE/DELETE/NOOP`，后者学习选择并
  使用记忆；分别以 PPO/GRPO 和最终任务结果训练。
- **证据**：论文称只用 152 个训练 QA pair，即在 LoCoMo、MSC、LongMemEval 与 3B–14B 模型间优于
  强基线，说明“管理动作”本身可以被学习，而不只靠固定规则。
- **局限**：小数据成功不等于安全、校准或跨领域稳定；结果奖励可能鼓励为答题而改写记忆，论文没有
  证明隐私、授权、审计与误删保障。
- **代码映射**：现在即可借用四动作作为内部候选分类和测试 taxonomy；远期 learned manager 只能产出
  `ProposedMutation`，仍须经过用户授权与 canonical JSONL journal，绝不能直接调用
  本地 `apply`。

### 3.4 A-MEM: Agentic Memory for LLM Agents

- **出处与机构**：Wujiang Xu、Zujie Liang、Kai Mei、Hang Gao、Juntao Tan、Yongfeng Zhang；作者
  affiliation 包括 Rutgers University、AIOS Foundation 与独立研究者；NeurIPS 2025 Main
  Conference，已同行评审。
  [NeurIPS Proceedings](https://proceedings.neurips.cc/paper_files/paper/2025/hash/19909c36f51abc4856b4560aff3d36d6-Abstract-Conference.html)
- **机制**：借鉴 Zettelkasten，把每条 memory 变成带 context、keywords、tags 和 embedding 的原子
  note；新 note 会查找关联、建 link，并触发旧 note 属性的 memory evolution。
- **证据**：论文在六个 foundation model 上报告优于所比较的 SOTA memory baselines；其主要贡献是
  动态组织方式，而非一种新的向量距离。
- **局限**：context/tag/link 和“演化”都可能产生模型幻觉；若直接覆写旧记录，会破坏来源、授权和
  可重放性。对话 QA 的收益也不能直接外推到 coding-agent 的动作安全。
- **代码映射**：把 annotation 与 link 做成按 record ID 关联、带 extractor/model version 的可重建
  side table；原始 `MemoryRecord.text`、`origin`、`evidence` 永不被 evolution 就地修改。

### 3.5 Agent Workflow Memory

- **出处与机构**：Zora Zhiruo Wang、Jiayuan Mao、Daniel Fried、Graham Neubig；Carnegie Mellon
  University 与 MIT；ICML 2025，已同行评审。
  [PMLR](https://proceedings.mlr.press/v267/wang25bx.html)
- **机制**：从历史动作轨迹中归纳重复出现的 workflow，可离线抽取，也可在测试时在线抽取；后续只
  给 agent 提供与当前任务匹配的流程。
- **证据**：论文报告 Mind2Web 和 WebArena 成功率相对提升 24.6% 与 51.1%，并减少成功任务的步骤；
  online AWM 在跨任务/网站/领域设置中比基线高 8.9–14.0 个绝对点。
- **局限**：实验是网页导航，不是代码修改；轨迹中的偶然动作、危险命令和旧版 UI 都可能被固化成
  “流程”，成功一次也不等于可推广。
- **代码映射**：不要把 workflow 塞进普通 `Fact`。短期建立候选对象：步骤、前置条件、scope、来源
  trajectory、成功/失败结果、工具/版本约束和风险标记；只有通过重复验证或用户确认才提升为持久
  `Instruction`，执行时仍走现有 trust/tool policy。

### 3.6 THEANINE: Towards Lifelong Dialogue Agents via Timeline-based Memory Management

- **出处与机构**：Kai Tzu-iunn Ong、Namyoung Kim、Minju Gwak、Hyungjoo Chae、Taeyoon Kwon、
  Yohan Jo、Seung-won Hwang、Dongha Lee、Jinyoung Yeo；Yonsei University 与 Seoul National
  University；NAACL 2025 Long，已同行评审。
  [ACL Anthology](https://aclanthology.org/2025.naacl-long.435/)
- **机制**：不把过时记忆简单丢掉，而是用时间和因果关系连成 memory timeline；回答时取出相关状态
  的演化链。TeaFarm 用反事实干预检验模型是否真正利用时间线。
- **证据**：论文在 automatic、LLM-based 和 human evaluation 中均报告超过所比较的代表性基线；
  counterfactual 评测比只看生成流畅度更能暴露错误记忆使用。
- **局限**：论文的“不删除旧记忆”不能照搬到产品。用户明确 `Forget`、隐私删除或保留期到期时，
  删除语义必须优先；对话连贯性也不等于 coding 决策正确性。
- **代码映射**：`supersedes` 已是时间链的最小起点。下一步可先用派生表表达 `version_group`、
  `valid_from/to`、`precedes`、`caused_by`；默认 recall 只返回 active head，只有“之前/为什么改变”类
  时间问题才展开历史，并始终保留 evidence pointer。

### 3.7 HippoRAG 2: From RAG to Memory: Non-Parametric Continual Learning for LLMs

- **出处与机构**：Bernal Jiménez Gutiérrez、Yiheng Shu、Weijian Qi、Sizhe Zhou、Yu Su；The Ohio
  State University 与 University of Illinois Urbana-Champaign；ICML 2025，已同行评审。
  [ICML 官方页](https://icml.cc/virtual/2025/poster/45585)、
  [arXiv 作者稿](https://arxiv.org/abs/2502.14802)
- **机制**：把 passage 与 OpenIE entity/relation 组织成图，用 Personalized PageRank 做关联传播；
  同时更深地保留 passage 节点，并用在线 LLM 过滤无关 triple。
- **证据**：论文在 associative/multi-hop QA 上报告比当时 embedding RAG 平均高约 7 个点，并称没有
  牺牲 factual 与 sense-making 任务。它也提醒：早期图/摘要 RAG 可能因噪声在简单题上退化。
- **局限**：OpenIE 与在线过滤增加成本和模型依赖；错误 relation 会通过图传播。通用 QA 的图收益
  不能替代 `pi-rs` 自己的 stale/scope/distractor 测试。
- **代码映射**：先保留 BM25+dense RRF 为第一阶段，图只接收这些候选的 evidence-backed relation，
  作为可关闭的二阶段扩展。passage/session-entry 节点不能只剩摘要，否则无法审计答案来源。

### 3.8 H-MEM: Hierarchical Memory for High-Efficiency Long-Term Reasoning in LLM Agents

- **出处与机构**：Haoran Sun、Shaoning Zeng、Bob Zhang；Yangtze Delta Region Institute (Huzhou)、
  University of Electronic Science and Technology of China 与 University of Macau；EACL 2026
  Long，已同行评审。
  [ACL Anthology](https://aclanthology.org/2026.eacl-long.15/)
- **机制**：按语义抽象度组织 Domain → Category → Memory Trace → Episode 四层；上层向量携带指向
  下层子记忆的索引，查询逐层路由，避免在所有叶子上做相似度计算。
- **证据**：论文在 LoCoMo 五类任务上报告整体优于五个基线；最大记忆规模下查询低于 100 ms，而其
  对照图中的基线超过 400 ms。
- **局限**：结果集中在一个长对话 benchmark，层级摘要可能丢失少见但关键证据；论文也明确列出
  text-only、容量、生命周期和隐私/安全限制。
- **代码映射**：只有当真实 corpus 和基准证明全量 hybrid recall 是延迟瓶颈时再做。可用 SQLite
  parent/child 派生表，叶子保持原始 record/session entry，上层摘要必须带覆盖范围、版本和可回溯
  child IDs。

### 3.9 LongMemEval-V2

- **出处与机构**：UCLA 团队；2026 年 arXiv preprint，**截至本快照未同行评审**。
  [arXiv](https://arxiv.org/abs/2605.12493)、
  [官方项目页](https://xiaowu0162.github.io/longmemeval-v2/)、
  [本仓库专项分析](./longmemeval-v2.md)
- **机制**：把网页/企业环境轨迹顺序摄入，要求 memory system 仅凭问题收集有界上下文，再由固定
  reader 回答；451 题覆盖 static state、dynamic state、procedure、errors/gotchas 和 premise
  awareness，并提供 small/medium 噪声层级与多跳证据。
- **证据**：它同时报告答案质量、context gathering latency、token/context cost 和
  latency-adjusted 分数，比只算 Recall@k 更接近真实系统权衡。
- **局限**：官方输入包含网页状态、截图和动作轨迹，而当前 `pi-rs` 只持久化用户明确批准的紧凑
  semantic records；直接跑官方集会把抽取、视觉、网页经验和检索能力混在一起。
- **代码映射**：复用协议而非冒充官方分数：固定 seed 的 100-session small tier；标注 evidence hop；
  设 no-memory、oracle、lexical、dense、hybrid；同时测错误旧值、错 scope、distractor、拒答、P50/P95
  延迟和注入 token。

### 3.10 ReFind: When Your Agent Opens the Chat App: Agent-Controlled Search over Raw Chat Logs Rivals Structured Memory

- **出处与机构**：Ruizhe Li、Licheng Zhang、Benfeng Xu、Mingxuan Du、Zheren Fu、Weidong Chen；
  University of Science and Technology of China 与 MetaStone Technology；arXiv v1，2026-08-16，
  **非常新的预印本，未同行评审**。
  [arXiv](https://arxiv.org/abs/2608.12888)
- **机制**：不先抽取结构化长期记忆，而让 ReAct agent 反复改写查询并搜索逐 turn 原始日志；底层用
  BM25、session-aware rank fusion、时间过滤、相邻窗口扩展和已看 session 跳过，把 evidence
  gathering 与最终回答分开。
- **证据**：论文在约 2,800 个 MemoryAgentBench 问题上报告平均 58.2，比较表中的 HippoRAG 2 为
  53.2、BM25 为 48.8；在其 LongMemEval-S/M 子集上报告 93.2±3.3/89.3±6.0。
- **局限**：发表距本快照只有约两周；部分 baseline 数字沿用先前论文而非全部重跑，LongMemEval
  子集只有 50/15 题且方差明显。迭代 tool calling 的成本不应混入每轮默认 recall，英文 BM25 设置也
  不能证明中文效果。
- **代码映射**：这是与现有 session search 最贴合的 P0 实验。增加显式、慢速、带最大查询轮数/
  session 数/token 数的 `session_search` 或 `/memory-local-deep-search`；返回 entry IDs 与窗口，避免自动
  提升为永久 `MemoryRecord`。先在本地复现后再宣称超过结构化 memory。

### 3.11 MemoryBank: Enhancing Large Language Models with Long-Term Memory

- **出处与机构**：Wanjun Zhong、Lianghong Guo、Qiqi Gao、He Ye、Yanlin Wang；Sun Yat-sen
  University、Harbin Institute of Technology、KTH Royal Institute of Technology 等；AAAI 2024，
  已同行评审。
  [AAAI Proceedings](https://ojs.aaai.org/index.php/AAAI/article/view/29946)
- **机制**：在长期对话 memory bank 上借鉴艾宾浩斯遗忘曲线，以时间和“被回忆次数”调整保留强度，
  并维护用户 persona。
- **证据**：论文把衰减机制作为长期对话实验的一部分；作者也明确承认该遗忘模型是高度简化的探索，
  不能当成人类记忆的准确模型。
- **局限**：按访问次数增强会形成 popularity feedback loop：已经容易召回的内容更不易衰减，长尾但
  关键的安全约束反而可能消失。时间久也不等于用户不再需要，更不构成删除授权。
- **代码映射**：若增加 `last_recalled_at`、`recall_count`、任务成功反馈，应只作为可解释的排序/归档
  signal；绝不据此生成隐式 `Forget`。用户明确 pin、scope、kind 与当前有效状态优先于 decay。

## 4. 建议的数据边界

短期不扩张生产插件 Interface，先在 SQLite 投影内部加入可重建 side tables：

| 派生对象 | 最小字段 | 论文来源 | 约束 |
| --- | --- | --- | --- |
| temporal state | `record_id`, `version_group`, `valid_from/to`, `relation` | THEANINE、REMem | active view 与 history view 分开 |
| annotation | `record_id`, `context`, `tags`, `extractor_version` | A-MEM | 不覆写 canonical text |
| relation | `src`, `dst`, `kind`, `confidence`, `evidence_ids` | A-MEM、HippoRAG 2 | 可删除、可重建、有 provenance |
| usage stats | `record_id`, `last_recalled`, `count`, `outcome` | MemoryBank | 只调排序，不触发遗忘 |
| workflow candidate | steps、preconditions、scope、trajectory、outcome、risk | AWM | 验证/授权后才提升 |
| episode/hierarchy | gist、time range、parent/children、source entry IDs | REMem、H-MEM | 叶子和证据不可丢 |

如果未来需要把 valid time 或 workflow 变成跨 provider 的稳定语义，再通过带默认值的 wire evolution
修改 `MemoryRecord`/新增强类型；不要让某个 SQLite 索引技巧反向污染 canonical protocol。

## 5. 优先实施顺序

1. **P0：评测先行。** 以 MemoryAgentBench + LongMemEval-V2 设计固定 seed 的 sequential suite，覆盖
   correction、forget、premise awareness、multi-hop、distractor、wrong scope、latency 和 token。
2. **P0：时间/冲突 active view。** 在不改变 journal 的前提下，为 `supersedes` 补 version grouping
   和时间关系；普通 recall 只给当前 head，时间问题可展开历史。
3. **P0：原始 session 深搜。** 在本地 session search 上做 ReFind 风格的有预算迭代检索；默认 hook 仍用
   低延迟 semantic memory，深搜只在需要时显式触发。
4. **P1：流程候选与晋升。** 从成功/失败轨迹生成 AWM 风格候选，执行验证和用户确认后才成为
   `Instruction`；先支持手工晋升，再研究自动阈值。
5. **P1：派生 annotation/link 小实验。** 在同一评测上比较 flat hybrid、hybrid+link expansion；只有
   multi-hop 收益覆盖写入成本、延迟和误关联后，才考虑 HippoRAG 2/A-MEM 式图。
6. **P1/P2：规模触发层级。** corpus 足够大且 profiling 证明扁平检索成为瓶颈时，再尝试 H-MEM；所有
   摘要保留 child/evidence 指针。
7. **P2：学习式管理。** 最后才评估 Memory-R1；模型只能建议 mutation，用户授权、journal 顺序、
   trust policy 与 `Forget` 语义保持硬边界。

## 6. 明确 caveats

- **同行评审不是生产证明。** 上述正式会议论文多数只验证 QA、对话或网页任务，没有覆盖 coding-agent
  的命令安全、项目 trust、重放一致性和用户数据删除。
- **预印本需本地复现。** LongMemEval-V2 与尤其新的 ReFind 只能作为设计线索；不得把其论文数字当成
  `pi-rs` 的预期结果。
- **结果不可直接横比。** 不同论文使用不同模型、judge、上下文、top-k、预算和 baseline revision；
  本文数字只描述各论文自己的实验。
- **自动抽取永远是派生信息。** LLM 生成的 summary、tag、relation、workflow 和 importance 都可能
  出错，必须带版本、来源和重建路径，不能静默改写用户授权的 canonical record。
- **历史保留服从删除语义。** THEANINE 式时间链对理解状态演化有用，但用户明确遗忘、隐私删除和
  保留政策高于“为了未来推理保留旧事实”。
- **先证明问题再加结构。** graph/hierarchy 会引入一致性、迁移、模型调用和调试成本；若 flat
  BM25+dense RRF 加 agentic raw-log search 已满足质量与延迟，就没有必要为了论文新颖性复杂化系统。
