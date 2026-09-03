# pi-memory evaluation

`pi-memory-eval` is a deterministic, provider-only retrieval benchmark inspired by
LongMemEval-V2. It evaluates the current curated `MemoryRecord` product model; it does not claim
compatibility with the official web-trajectory benchmark and does not widen the production memory
plugin Interface.

The critical isolation rule is structural: an `EvalBackend` receives only query text, scopes, and
the result limit. Question ids, ability labels, evidence ids, forbidden records, and expected
answers remain in the runner. `OracleBackend` is the one deliberately privileged upper-bound
adapter and is never a product backend.

## Run

```sh
cargo run -p pi-memory-eval -- \
  --backend sqlite \
  --suite small-dev \
  --report target/memory-eval.json
```

The model-backed hybrid lexical/dense Adapter is opt-in and never downloads during an evaluation:

```sh
cargo run -p pi-memory-eval -- \
  --backend sqlite-dense \
  --embedding-cache ~/.pi/agent/models/embeddings \
  --suite small-holdout \
  --report target/memory-eval/sqlite-dense-small-holdout.json
```

Run the original frozen query split separately with `--suite small-holdout`. The later one-shot
generalization readout uses `small-holdout-v2` and `medium-holdout-v2`; both v2 suites select the
same questions over their respective fixed haystacks. All of these suites have now been viewed and
must not be used for another ranking-threshold choice.

Available backends are `no-recall`, `oracle`, `sqlite-bm25`, `sqlite`, `sqlite-dense`, and
`sqlite-dense-raw-rrf`. The default `sqlite` backend uses the lightweight hybrid lexical ranker;
`sqlite-bm25` preserves the raw FTS5 control. `sqlite-dense` loads the pinned FastEmbed model from
an explicit `--embedding-cache`, embeds the haystack before timing queries, and exercises
production protected-lexical/dense-rescue ranking through `LocalMemoryProvider::recall`.
`sqlite-dense-raw-rrf` is an evaluation-only historical control: it uses the same sparse and dense
candidates but ranks them by equal-weight RRF without the product confidence, cutoff, or diversity
policy. SQLite uses an isolated temporary database unless `--database <path>` is supplied. Use
`--help` for all options.

The bundled corpus contains 45 bilingual questions across static state, dynamic state, procedure,
gotcha, and premise-awareness abilities. `small-dev` contains the original 15 development
questions; `small-holdout` contains the first 15 separately worded questions; and
`small-holdout-v2` adds 15 questions frozen before the one-shot model run. The legacy `smoke` and
`small` suite names retain the development questions. The seven curated sessions include active
evidence, same-scope distractors, cross-project/session records, superseded records, and a
tombstoned procedure.

The `small` haystack keeps those needles and deterministically expands the corpus to 100 sessions and
306 records from seed `24052026`. The `medium` haystack reuses the exact curated needles and seed but
expands to 500 sessions, 1,506 records, and 1,507 replay mutations. Every generated id, text, scope,
timestamp, and replay position is stable. Generated sessions contribute one Atlas-project hard
negative, one contextual user-memory hard negative, and one cross-project record. The medium dev,
first holdout, and v2 holdout suites reuse their corresponding small-tier question sets, so paired
small/medium runs isolate scale pressure. Only the first execution of v2 was an unseen-query
readout.

The current lightweight hybrid ranker asks FTS5 for a bounded 32–100 candidate window, then combines
candidate-local inverse-document-frequency coverage, phrase order, contiguous spans, code atoms,
and reciprocal sparse rank. It deliberately does not infer intent from language-specific keyword
lists or use `MemoryKind` as a lexical prior. A diversity pass favors complementary evidence over
near-duplicate summaries, and a relative confidence cutoff treats the requested limit as a maximum
instead of filling the prompt with weak hits. The ordinary checked baselines do not use embeddings.
The separate `sqlite-dense` backend uses the pinned `intfloat/multilingual-e5-small` Adapter and
sqlite-vec index. It protects the ordinary lexical result, then admits dense candidates using
normalized cosine, a bounded RRF contribution, substantive query-evidence coverage, seeded diversity,
and the relative confidence cutoff. A rescue that adds a previously uncovered substantive term
through an adjacent query phrase or exact query code atom is a coherent complement: it wins the
corresponding diversity slot and is not discarded by the score-only cutoff. Its test is ignored by
default and requires `PI_MEMORY_EMBEDDING_CACHE`, so model downloads and a 470 MB ONNX load never
become an implicit workspace-test side effect.

## Metrics

The JSON report contains per-case diagnostics and aggregate:

- evidence-hop Recall@1/5/8, all-hop success, and MRR;
- wrong-scope, stale, and same-scope distractor case rates and hit counts;
- evidence density;
- completion, backend-error, timeout rate, and query latency mean/p50/p95/p99/max;
- ordered hit scores and the main retrieval metrics split by ability.
- Recall@5, all-hop success, MRR, density, and forbidden-case rate split by same-language,
  cross-language, and mixed-language evidence relation;
- the same metrics split into single-hop and multi-hop cases;
- for SQLite runs, rank-ordered sparse, dense, and deduplicated union candidate ids plus their
  evidence-hop recall and all-hop rate before final ranking;
- for product sparse/dense runs, the same coverage at protected-core, gate-eligible, and pre-cutoff
  boundaries, so a miss can be assigned to candidate generation, gate, diversity, or cutoff.

Wrong-scope and stale hits are correctness failures. Latency is reported but is not hard-gated in
the smoke regression because shared CI timing varies; product operating points should be compared
from reports produced on the same host.

## Corpus layout

```text
fixtures/v1/
  manifest.json
  sessions.jsonl
  questions.jsonl
  baselines/*.json
  haystacks/medium.json
  haystacks/small.json
  haystacks/smoke.json
  suites/medium-dev.json
  suites/medium-holdout.json
  suites/medium-holdout-v2.json
  suites/small-dev.json
  suites/small-holdout.json
  suites/small-holdout-v2.json
  suites/small.json
  suites/smoke.json
```

Sessions contain production `MemoryMutation` values in replay order. Each question declares one or
more evidence hops; any record id within a hop satisfies that hop. Forbidden records carry a
reason: `wrongScope`, `stale`, or `distractor`. A suite references one haystack plus an explicit,
ordered list of question ids, so development and holdout queries cannot accidentally receive
different generated filler. The curated language relation compares the query with the dominant
natural language of its required evidence; identifiers and code literals do not determine the
label.

Loading validates identities, origins, mutation targets, active evidence, scope visibility,
forbidden-record semantics, suite membership, haystack references, and uniqueness of the
backend-visible query input. This keeps Oracle lookup unambiguous and prevents a malformed fixture
from producing a misleading score. Language relation and split membership remain runner-owned gold
metadata and never cross the `EvalBackend` seam.

On `small-dev`, the pinned raw-BM25 control has Recall@5 `0.900`, all-hop `0.867`, and evidence
density `0.175`; the lightweight hybrid has Recall@5 `0.967`, all-hop `0.933`, density `0.656`, and
zero designated wrong-scope, stale, or distractor hits.

The frozen `small-holdout` is deliberately harder. Raw BM25 has Recall@5 `0.733`; hybrid reaches
`0.767` while raising evidence density from `0.168` to `0.633` and removing the designated
distractor. Hybrid holdout Recall@5 is `1.000` for same-language cases, `0.750` for cross-language,
and `0.250` for mixed-language cases; its only multi-hop case retrieves one of two hops. These gaps
are the acceptance target for selecting and pinning the production dense model, not an invitation
to add query-specific vocabulary. Hybrid scores are adapter-local ranking scores, not calibrated
probabilities. Latency is intentionally excluded from pinned baseline files and must be measured on
the same host.

The first pinned-model raw-RRF run remains a historical diagnostic: `small-dev` Recall@5 was
`0.833` with 3 designated distractor hits, while `small-holdout` Recall@5 was `0.867` with 2
distractor hits. The protected lexical + gated dense-rescue implementation removes that tradeoff.
With the pinned model and a 500 ms per-query test budget, `small-dev` reaches Recall@5 and all-hop
`1.000`, evidence density `0.489`, and zero wrong-scope, stale, or distractor hits. The viewed
`small-holdout` reaches Recall@5/all-hop `0.933`, cross-language Recall@5 `1.000`, full success on
its multi-hop case, evidence density `0.422`, and zero forbidden hits. Because this holdout was
already inspected while diagnosing raw RRF, future threshold selection requires a new frozen split
rather than further tuning against these 15 questions.

The fixed-seed 500-session tier adds a separate scale axis. On `medium-dev`, lexical hybrid reaches
Recall@5/all-hop `0.900/0.867`; raw RRF falls to `0.700/0.733`; protected dense rescue now reaches
`1.000/1.000`. On `medium-holdout`, the same three configurations reach Recall@5
`0.767/0.600/0.800`, with protected dense all-hop also `0.800`. All six medium runs have zero
designated wrong-scope, stale, or distractor hits. Product dense evidence density was `0.494` on dev
before the ranking-stage fix and is `0.506` after it; holdout remains `0.417`, versus raw RRF
`0.158/0.133`. These results show that the product policy prevents raw-fusion dilution.

Report schema v5 adds candidate and ranking-stage coverage without exposing gold metadata to the
backend. Before the policy change, `medium-dev` sparse/dense/union Recall was
`0.933/0.900/1.000`; protected/gate/pre-cutoff Recall was `0.900/1.000/0.967`, and final Recall@5
was `0.967`. The missing command therefore passed the evidence gate but lost a diversity slot.
Prioritizing coherent new evidence moved pre-cutoff Recall to `1.000`; preserving that deliberately
selected complement through cutoff moved final Recall@5/all-hop to `1.000/1.000`, raised density,
and kept every forbidden-hit count at zero.

The already viewed `medium-holdout` remains unchanged: sparse/dense/union and
gate/pre-cutoff Recall are `0.933`, while final Recall@5 is `0.800`. Two misses reach pre-cutoff and
are removed by the stricter ordinary cutoff; answer-style evidence is absent from both 64-candidate
source lists. These are diagnostics, not tuning targets. They do not justify lowering the global
cutoff or widening the candidate window without a newly frozen unseen split.

Corpus version `1.3.0-holdout-v2` froze the next 15 questions before either product run. They have
three cases for each ability, a `7/5/3` same/cross/mixed-language distribution, one multi-hop case,
and identical ordered ids in the small and medium suites. Oracle achieves Recall@5/all-hop
`1.000/1.000` with no forbidden hits. At freeze time the aggregate `questions.jsonl` SHA-256 was
`0fba3279f4e476f1d5ecb2ce6575c32fcc6c791f53d98e9fd5febce2f0d7d32f`; the small and medium v2
suite manifests were `08ad7e0da65a9eff0a05d97eb78a51dda63ae4c4a857cc5f8b08f3e8e630b581` and
`3537c93a924f4bb6cfbc6ad7c6d52563c58e912fc10fb09637b0101f61b08459`.

The one-shot pinned-model result is the same at both scales: Recall@5 `0.833`, all-hop `0.800`, no
wrong-scope or stale hit, and two designated distractor hits. Evidence density is `0.363/0.380` and
p95 query latency on the same development host is `21.51/56.45 ms` for small/medium. Small
sparse/dense/union Recall is `0.700/1.000/1.000`; medium is `0.700/0.967/0.967`. Both have
protected/gate/pre-cutoff Recall `0.633/0.833/0.833`. The three final gaps are therefore actionable
diagnostics rather than a threshold target:

- the release query loses its workspace-test second hop at the gate on small and at candidate
  generation on medium;
- the ephemeral-port answer reaches the dense/union pool at both scales but does not pass the gate;
- the false `deploy`-tool premise reaches both source lists, but ranking retains the release
  workflow instead of the registered-tool inventory.

The full-test query also ranks the explicitly forbidden fast-test shortcut ahead of the correct
workspace command. Because v2 is now viewed, none of these observations authorizes query-specific
vocabulary, a lower global cutoff, or a wider global candidate window. Fixes must be developed on
new dev cases and validated once against a separately frozen v3 split.

Reproduce a medium product/control comparison with:

```sh
cargo run -p pi-memory-eval -- \
  --backend sqlite-dense \
  --embedding-cache ~/.pi/agent/models/embeddings \
  --suite medium-holdout \
  --timeout-ms 1000 \
  --report target/memory-eval/sqlite-dense-medium-holdout.json

cargo run -p pi-memory-eval -- \
  --backend sqlite-dense-raw-rrf \
  --embedding-cache ~/.pi/agent/models/embeddings \
  --suite medium-holdout \
  --timeout-ms 1000 \
  --report target/memory-eval/sqlite-dense-raw-rrf-medium-holdout.json
```

Reproduce the frozen v2 readout (the output is now a viewed diagnostic, not a tuning set) with:

```sh
cargo run -p pi-memory-eval -- \
  --backend sqlite-dense \
  --embedding-cache ~/.pi/agent/models/embeddings \
  --suite small-holdout-v2 \
  --timeout-ms 1000 \
  --report target/memory-eval/sqlite-dense-small-holdout-v2-frozen.json

cargo run -p pi-memory-eval -- \
  --backend sqlite-dense \
  --embedding-cache ~/.pi/agent/models/embeddings \
  --suite medium-holdout-v2 \
  --timeout-ms 1000 \
  --report target/memory-eval/sqlite-dense-medium-holdout-v2-frozen.json
```

Reproduce the pinned-model regression test explicitly after installing the model:

```sh
PI_MEMORY_EMBEDDING_CACHE=~/.pi/agent/models/embeddings \
  cargo test -p pi-memory-eval --test dense_model_baseline -- --ignored --nocapture
```

The next retrieval slice should turn the v2 failure classes into development cases without tuning
against the v2 wording, then freeze a v3 query split before accepting any ranking or threshold
change. It should test negated premises, complementary multi-hop evidence, and candidate saturation
independently before an adaptive window is considered. Full reader accuracy and the official
LongMemEval-V2 trajectory Adapter belong in later, separate tracks.
