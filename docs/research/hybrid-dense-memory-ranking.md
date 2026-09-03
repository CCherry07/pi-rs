# Hybrid lexical + dense 记忆排序的论文依据

> 调研快照：2026-09-02。本文只引用原论文、官方会议页面或作者公开版本，并区分
> **论文直接结论**与**针对 `pi-rs` 的工程推断**。

## 结论

论文支持这条总体路线：保留词法检索的精确匹配能力，用 dense retrieval 补 vocabulary gap，按查询或
检索结果决定何时引入 dense，并在最终列表上做动态截断与多样性选择。它们也明确反对一个过强的
假设：**把 BM25 与 dense 无条件做等权 RRF，并不能保证优于最好的单路结果。**

不过，没有论文直接验证我们落地的完整实现——“先冻结 lexical 主干，再仅用 dense 补位，最后沿用
现有 lexical cutoff/diversity”。其中“dense rescue”有直接的结构性依据；“lexical 命中不被挤出”、
具体 confidence 公式和阈值仍是由当前产品风险与 eval 结果推导出的工程假设。当前实现已通过
`small-dev` 和已查看的 `small-holdout`，完成 500-session medium scale 与 final-ranking stage 诊断，
并对预先冻结的 holdout-v2 做了一次性评测。v2 现已查看，下一轮改动必须在新 dev cases 上开发，
再由另一个冻结 split 验证。

## Raw RRF 为什么需要改

改动前，[`ranking.rs`](../../plugins/features/pi-plugin-memory-local/src/ranking.rs) 中的 lexical `rerank` 会计算词项覆盖、
短语顺序、code atom 和 sparse rank，然后执行 greedy diversity 与相对 cutoff；当时的
`fuse_sparse_dense` 只对两个名次列表计算 RRF。随后
[`storage/mod.rs`](../../plugins/features/pi-plugin-memory-local/src/storage/mod.rs) 的 `SparseDenseRrf` 分支直接返回该 RRF 列表，因此
绕过了 lexical 的 confidence 与 diversity policy。

这不是抽象风险。当前固定 seed eval 显示，raw RRF 把 holdout Recall@5 从 `0.767` 提高到 `0.867`，
并补齐跨语言与 multi-hop，但把 dev Recall@5 从 `0.967` 降到 `0.833`，同时在 dev/holdout 引入
`3/2` 个 distractor；详见 [`evals/pi-memory/README.md`](../../evals/pi-memory/README.md)。这与论文中
“不同 retriever 互补，但无条件 fusion 也可能退化”的观察一致。

现在的 `fuse_sparse_dense` 先运行 lexical `rerank` 得到 protected core，再对 BM25/dense 并集计算
lexical structure、归一化 cosine 与有界 RRF contribution。补召候选必须覆盖 core 尚未覆盖的实质性
query token，或者至少重复三个实质性 token，之后以 core 作为已选集合执行同一套 greedy diversity
和相对 cutoff。这里的“实质性”只看 token 形态（CJK、含数字或至少四个字符），没有重新引入 intent
关键词表或 `MemoryKind` 先验。

固定模型验证中，`small-dev` Recall@5/all-hop 从 raw RRF 的 `0.833/0.867` 提高到 `1.000/1.000`，
distractor 从 `3` 降为 `0`；已查看的 `small-holdout` Recall@5/all-hop 为 `0.933/0.933`，cross-language
Recall@5 为 `1.000`，multi-hop 全命中，所有 forbidden hit 为 `0`。这些结果验证当前工程假设，但不把
它升级为论文结论；该 holdout 已参与诊断，后续不能继续充当无偏调参集。

500-session 对照进一步把“fusion policy”和“规模压力”分开：`medium-dev` 上 raw RRF 与 product
fusion 的 Recall@5 现在是 `0.700/1.000`，`medium-holdout` 是 `0.600/0.800`；两者都保持零 forbidden hit。
这证明 product policy 明显减轻了 raw RRF dilution，但 holdout 的 `0.800` 仍说明候选生成或最终准入
存在规模退化。

Report schema v5 已补齐 candidate-union，以及 protected core、gate-eligible、pre-cutoff 三段
coverage。修复前，`medium-dev` 的三段 Recall 是 `0.900/1.000/0.967`：唯一 miss 已通过 gate，却被
重复 dashboard 挤出 diversity selection。排序改为优先选择“新增实质性 evidence + 相邻 query phrase
或 exact query code atom”的 coherent complement 后，pre-cutoff 达到 `1.000`；让这类明确选中的
complement 穿过 score-only cutoff 后，最终 Recall@5/all-hop 达到 `1.000/1.000`，density 从 `0.494`
升到 `0.506`，forbidden hit 仍为零。

已查看的 `medium-holdout` 保持 `0.800`：两个 miss 完整进入 pre-cutoff 后被普通 cutoff 删除，answer
style 同时掉出两个 64-candidate source list。因为这些 query 已被查看，它们只能说明 ordinary cutoff
和 candidate saturation 仍是两个独立问题，不能用于继续放宽阈值或窗口。

`1.3.0-holdout-v2` 在运行前冻结 15 个新问题，并让 small/medium 共用问题、curated needles 与固定
seed，只改变 100/500-session filler 规模。一次性 product run 在两档都得到 Recall@5 `0.833`、all-hop
`0.800`、两个 distractor hit，且无 wrong-scope/stale hit。small 的 sparse/dense/union Recall 为
`0.700/1.000/1.000`，medium 为 `0.700/0.967/0.967`；两档 protected/gate/pre-cutoff 都是
`0.633/0.833/0.833`。这把三个失败进一步分解为：ephemeral-port 与 false-deploy-premise 是候选已到但
gate 未接纳；release multi-hop 在 small 是 gate 漏掉第二跳，在 medium 则是 64-candidate source window
已经漏掉第二跳。另一个 full-test case 虽命中正确证据，却把明确排除的 fast-test shortcut 排在前面。
因此后续不能靠单一全局降阈值解决；准入、否定前提与规模自适应候选窗口要作为独立假设验证。

## 最有力的八篇论文

| 论文、机构与时间 | 论文直接结论 | 对 `pi-rs` 的意义与边界 |
| --- | --- | --- |
| [CLEAR: Complement Lexical Retrieval Model with Semantic Residual Embeddings](https://link.springer.com/chapter/10.1007/978-3-030-72113-8_10)，Luyu Gao 等，Carnegie Mellon University / Johns Hopkins University，ECIR 2021 | CLEAR 明确把 BM25 当作已有 exact-match 信号，并训练 dense residual 去学习 lexical 没捕获的结构与语义；实验优于 lexical、dense-only 及普通融合。 | 这是“lexical backbone + semantic rescue”最直接的依据。但 CLEAR 的 dense 模型经过 residual supervision；当前通用 `multilingual-e5-small` 没有，因此只能借鉴角色分工，不能照搬其效果结论。 |
| [Predicting Efficiency/Effectiveness Trade-offs for Dense vs. Sparse Retrieval Strategy Selection](https://arxiv.org/abs/2109.10739)，Negar Arabzadeh、Xinyi Yan、Charles L. A. Clarke，University of Waterloo，CIKM 2021 | 论文先运行 sparse，再依据 query 与 top sparse document 选择 sparse-only 或 sparse+dense hybrid；在 MS MARCO 上，按查询选择比随机分配 dense 预算取得更好的 recall/latency frontier。 | 这是 confidence-gated dense rescue 最接近的已发表结构：dense 不必对每个 query 都同等介入。不过论文使用有 relevance label 的 BERT classifier；我们初版的 coverage/margin gate 只是低成本替代方案。 |
| [Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)，Gordon Cormack、Charles Clarke、Stefan Büttcher，University of Waterloo / Google，SIGIR 2009 | RRF 用 `1 / (k + rank)` 聚合多个排名；作者在 TREC/LETOR 实验中观察到平均约 4–5% 改善。论文中的 `k=60` 来自 pilot。 | 支持用 rank fusion 避免直接混合 BM25 与 cosine 的异量纲 raw score。它只定义**排序分数**，没有把 RRF score 校准为 relevance probability，也没有证明 `k=60` 或等权 fusion 对本地记忆最优。 |
| [An Analysis of Fusion Functions for Hybrid Retrieval](https://doi.org/10.1145/3596512)，Sebastian Bruch、Siyu Gai、Amir Ingber，Pinecone / University of California, Berkeley，ACM TOIS 2024 | 论文发现 RRF 对参数并非不敏感；它丢弃原始 score distribution 的信息；经过少量样本调参的 normalized convex combination 在其 in-domain 与 OOD 实验中都优于 RRF。 | 直接反驳“RRF 天然稳健且其归一化值可当 confidence”。在 `pi-rs` 中，RRF 可作为候选共识/排序信号，但 cutoff 应看独立 lexical/dense evidence，而不是当前归一化 RRF 值。 |
| [BEIR: A Heterogeneous Benchmark for Zero-shot Evaluation of Information Retrieval Models](https://datasets-benchmarks-proceedings.neurips.cc/paper/2021/hash/65b9eea6e1cc6bb9f0cd2a47751a186f-Abstract-round2.html)，Nandan Thakur 等，TU Darmstadt UKP Lab，NeurIPS Datasets and Benchmarks 2021 | 在 18 个异构数据集、多个架构的 zero-shot 对比中，BM25 是稳健基线；dense 模型在与训练域重合较低的数据上会落后于 BM25，且没有单一架构统治所有任务。 | 支持保护 exact-match 主干以及保留 BM25/lexical 回退，尤其当前 embedding 并未针对用户记忆训练。但 BEIR 是英文文档检索，不直接决定 multilingual memory 的权重。 |
| [List-aware Reranking-Truncation Joint Model for Search and Retrieval-augmented Generation](https://doi.org/10.1145/3589334.3645336)，Shicheng Xu 等，中科院计算所 / 中国科学院大学 / 中国人民大学，WWW 2024 | GenRT 将 reranking 与 query-specific truncation 联合建模；论文指出固定 top-k 会带入 irrelevant information，并在搜索与 RAG 任务上取得更好的 ranking/truncation 结果。 | 支持“limit 是上限、低置信度时返回少于 k 条”，也支持在融合后重新做 list-aware cutoff。GenRT 是有监督 encoder-decoder；现有相对 cutoff 只是轻量启发式，不能声称复现 GenRT。 |
| [The Use of MMR, Diversity-Based Reranking for Reordering Documents and Producing Summaries](https://doi.org/10.1145/290941.291025)，Jaime Carbonell、Jade Goldstein，Carnegie Mellon University，SIGIR 1998 | MMR 以 greedy selection 平衡 query relevance 与相对已选集合的 novelty，从而降低冗余。 | 直接支持“每选一条后再评价剩余候选”的 diversity 形态。当前基于 query-term coverage/Jaccard 的实现是 MMR-like policy，并不是原论文的相似度函数。 |
| [DF-RAG: Query-Aware Diversity for Retrieval-Augmented Generation](https://aclanthology.org/2026.findings-eacl.150/)，Saadat Hasan Khan 等，George Mason University / Capital One，Findings of EACL 2026 | DF-RAG 在 MMR 上按 query 动态选择 diversity 强度；在五个 reasoning-intensive QA 数据集上，比 cosine vanilla RAG 提升约 4–10% F1，并特别针对 multi-hop 的互补证据。 | 支持 dense rescue 进入最终列表前必须与已选证据做多样性比较，也解释了当前 holdout multi-hop 为什么可能受益。其 Planner/Evaluator 使用 70B LLM 和 GPU，不适合我们的 50 ms recall 热路径。 |

## 论文支持到什么程度

### 有直接支持

1. **Lexical 与 dense 是互补信号。** CLEAR、BEIR 及 strategy-selection 论文都给出相应机制或跨任务
   证据。
2. **Dense 应按需介入。** CIKM 2021 的 sparse-vs-hybrid selector 直接证明 per-query routing 可以改善
   效果—成本 frontier。
3. **RRF 适合做 rank aggregation，不适合冒充置信概率。** 原始 RRF 只用 rank；TOIS 2024 进一步
   说明它会丢失 score distribution。
4. **最终结果不必填满 top-k。** GenRT 直接研究 query-specific truncation。
5. **多跳证据需要 relevance 与 novelty 的联合选择。** MMR 与 DF-RAG 支持 greedy diversity。

### 仍是我们的工程推断

- **Lexical 主干完全不可被 dense 替换。** 论文支持 lexical robustness 与 dense complement，但没有证明
  “冻结前 N 条”总是最优。这是为了先守住当前零 distractor baseline 的保守 guardrail。
- **Dense 只填剩余槽位。** 这是把 CLEAR residual 思路映射到无需训练的 E5 Adapter，不是 CLEAR
  算法本身。
- **Confidence 的具体定义。** lexical coverage、code-atom match、top/second margin、dense cosine、
  dense margin、双路 agreement 都有合理动机，但组合式和阈值需要由本项目 eval 校准。
- **复用现有 `0.35` 相对 cutoff 和 diversity 系数。** 没有论文支持这些常数；融合后的 score space
  也与 lexical score 不同，不能机械复用。
- **以 lexical 已选集合初始化 diversity。** 这是 MMR 原理下合理的产品化实现，但仍须做 ablation。

## 已落地的可验证实现

当前版本称为 **protected lexical + gated dense rescue**，而不是“论文算法复现”：

1. 对同一 scope-filtered candidate window 分别取得 BM25 与 dense 列表。
2. 先运行现有 lexical `rerank + diversify + cutoff`，得到 lexical core。
3. 对其余候选保留可解释特征：两路 rank、是否双路命中、lexical coverage/margin、dense cosine/margin；
   RRF 只作为候选顺序或 agreement signal。
4. 仅当候选增加实质性 query evidence，或至少有三个实质性 query-term match 时，进入 rescue pool。
5. 以 lexical core 为已选集合，对 rescue pool 做 MMR-like greedy diversity；允许最终少于 requested limit。
6. rescue 若新增实质性 evidence，且以相邻 query phrase 或 exact query code atom 形成 coherent
   complement，则先于 score-only 重复项占位，并保留到 final；其余 rescue 仍执行 `0.35` 相对 cutoff。
7. embedding 缺失、失败或 gate 全拒绝时，输出必须与当前 lexical hybrid 完全一致。

当前已可重复运行 `lexical`、`raw RRF` 和完整 `+ gate + diversity/cutoff` 三个端点，并记录
protected core、gate-eligible pool 与 pre-cutoff selection 的 gold coverage。阈值只在 dev 上选择；
原 holdout 和 holdout-v2 都已查看，不应继续作为无偏调参集。后续要先建立不复用 v2 wording 的开发
case，再冻结 v3 做一次验收。验收仍应同时看 Recall@5、all-hop、cross-language、distractor、evidence
density、返回条数分布和 p95 latency，而不能只看 aggregate recall。

最终判断是：**有充分论文依据启动这个方向，但没有论文替我们验证具体启发式。** 最稳妥的做法是
把论文作为结构约束，把权重、阈值和“是否冻结 lexical core”作为明确的 eval 假设。
