# pi-bench

`pi-bench` contains reproducible performance workloads for the public seams owned by the
`pi-rs` runtime. It is an outer, test-only crate and is not a dependency of the product.

P0 covers:

- assistant stream assembly with dense text and mixed thinking/tool-call events;
- Pi v4 JSONL reopen, active-branch reconstruction, and provider projection at three sizes;
- incremental session commits after 64 through 16,384 existing entries, using deferred
  persistence to isolate validation and in-memory apply cost from filesystem sync latency, plus
  independently owned document snapshots, warm shared snapshots, and shared-view refreshes after
  mutation at the same sizes as read-path controls;
- successful factory-backed generation reload and failed-reload retention.

`session_stream` measures the complete Agent-to-Session event bridge with 64, 1,024 and 4,096
history messages and 16 or 256 streamed deltas. Fixtures are prepared before timing; each run
includes the same prompt, persistence and settlement overhead. Compare the extra delta cost at
each history size to detect per-chunk history copying. The isolated `stream_assembly` workload
does not exercise this bridge.

`session_io` covers durable 1/4/16-entry batches with first and retained writers, long and short
forks, newest-50 branch queries, 4/16-item queue clears, fresh session allocation and explicit ID
lookup. It also separates a restored session's first prompt, its next turn, and open plus first
prompt with four native `turn_end` hooks. Configuration is already persisted in those fixtures, so
the first prompt includes acquiring write access. File creation, seeding and verification happen
outside the timer, and only one fixture remains alive at a time. Verification reopens persisted
results; queue checks restore a fresh runtime. Outer record timestamps are fixed while timed writes
use the real clock. Every successful durable batch retains its `sync_data` guarantee.
Run with `cargo bench -p pi-bench --bench session_io`; avoid concurrent builds during measurements.
New allocations are pure path/ID construction, and explicit lookup still reads headers. Their
performance results must be interpreted separately.

`plugin_context` runs 0, 1, 4 and 16 native or JavaScript context hooks over 64 and 4,096
mixed user/assistant messages with 512-byte bodies. Its JS dispatcher measures the real Rust
adapter and outbound JSON encoding, then returns an empty batch result; Node execution, input
cloning, parsing and NAPI crossing are excluded. Reports include encoded bytes and host calls per
operation. The initial owned input clone is included equally in every case. Run with
`cargo bench -p pi-bench --bench plugin_context`; correctness and ordering are separately covered
by the context-hook, boundary and Node/NAPI regressions.

The large session-resume case also reports its replay, settled-session recovery scan, indexed
branch/context reads, document snapshot, document-based branch/context construction, and
provider-projection phases separately. A mixed large case interleaves message entries with
operation lifecycle records instead of measuring an entry-only journal. A generic typed-decode
control shows the cost of the compatibility fallback beside the message-specialized replay path.

Run every suite with Cargo's release benchmark profile:

```sh
cargo bench -p pi-bench
```

Run one suite or shorten a local smoke run:

```sh
PI_BENCH_WARMUP=1 PI_BENCH_SAMPLES=3 cargo bench -p pi-bench --bench session_resume
```

Reports use the `pi.bench.v1` JSON schema and are printed to stdout. Set
`PI_BENCH_OUTPUT` to a directory to retain one JSON file per suite:

```sh
PI_BENCH_OUTPUT=/tmp/pi-bench cargo bench -p pi-bench
```

Each report records its Git revision and dirty state, Rust version, target, profile, workload
parameters, fixture identity, and min/mean/p50/p95/p99/max nanoseconds. Compare results only when
the environment and fixture hashes match. These suites do not use real providers or network I/O,
and their results are not release claims by themselves.

## Session streaming comparison

Local measurement on 2026-10-24 used aarch64 macOS, Rust 1.98.0, the release profile,
3 warmup iterations and 12 samples per case. The baseline was commit `f12552b` with the same
new benchmark copied into its source archive; the comparison used the Agent/Session convergence
changes. Fixture hashes, parameters, target, toolchain and sampling settings matched in all six
cases. Values below are p50 milliseconds for the complete timed Session operation.

| Historical messages | Deltas | Before | After |
| ---: | ---: | ---: | ---: |
| 64 | 16 | 23.99 | 24.67 |
| 64 | 256 | 26.93 | 27.90 |
| 1,024 | 16 | 31.91 | 23.09 |
| 1,024 | 256 | 35.98 | 24.78 |
| 4,096 | 16 | 30.02 | 28.97 |
| 4,096 | 256 | 90.62 | 28.53 |

The workload includes journal synchronization, so small differences include filesystem noise.
The large-history/high-delta case exposes the removed per-chunk copying; the low-delta case
does not show an improvement at every size. The Agent and Session shared-snapshot regressions
independently verify history pointer sharing across chunks and immutable retained history after
message-end hooks and context changes. No fixed speedup ratio is an acceptance requirement.

## Incremental document refresh comparison

Local measurement on 2026-10-26 compared commit `6f2b170` with incremental shared-document
refresh on an Apple M3 Pro, aarch64 macOS, Rust 1.98.0, release. Both binaries ran twice in
baseline/current/current/baseline order. Each `session_commit` case used 5 warmups and 50 samples;
fixture hashes, parameters and sampling settings matched. The table shows the range of the two
p50 measurements in microseconds for one deferred append followed by a shared-document read.

| Existing entries | Before (µs) | After (µs) |
| ---: | ---: | ---: |
| 64 | 13.959 | 2.625–2.708 |
| 1,024 | 119.250–120.625 | 2.500–2.625 |
| 4,096 | 457.291–459.833 | 2.625 |
| 16,384 | 3,313.458–3,396.375 | 2.750 |

The previous view is released before mutation in this workload. The store can therefore retain
its historical payloads and copy only the new suffix when the next reader requests a document.
Retained strong or weak snapshots require a fresh document; independently owned snapshots and
initial materialization still scale with history. The cache can retain one older document until
its next read or the store is dropped. Deferred appends do not measure filesystem sync latency.

The same alternating comparison ran `session_stream` with 3 warmups and 24 samples. At 4,096
messages and 256 deltas, whole-turn p50s were 31.02/31.42 ms before and 31.84/27.92 ms after.
This does not establish a whole-turn speedup: that workload includes journal synchronization and
does not repeatedly request full documents. The improvement above is specific to document refresh.

## Session I/O comparison

An expanded Session audit on 2026-10-26 compared the implementation-start Session snapshot with
the I/O changes on an Apple M3 Pro, macOS, Rust 1.98.0, release, with warm files and scripted
providers. Both versions used the same surrounding Agent/Plugin code and current Runtime,
including its prepared-input timestamp fix. Two uninstrumented runs per version were bracketed
around a separate instrumented run; measurements ran without concurrent compilation. The values
below are the range of the two p50s in milliseconds, with 3 warmups and 20 samples per case.

| Operation | Before (ms) | After (ms) |
| --- | ---: | ---: |
| Restored 4096-message session, first prompt, 4 native hooks | 26.983–27.222 | 14.902–15.088 |
| Same session, next prompt | 31.567–32.484 | 14.119–14.785 |
| Open session and run first prompt | 30.040–33.119 | 17.231–18.149 |
| Fork a 16384-message branch | 184.712–201.448 | 25.344–27.238 |
| Clear 4 queued inputs | 22.948–25.674 | 1.455–2.888 |
| Resolve an existing ID among 1000 session files | 356.525–583.139 | 80.049–90.906 |
| Read the newest 50 branch entries from 16384 messages | 1.492–1.529 | 0.0062–0.0063 |

The first-prompt fixture already contains configuration, so acquiring the first append handle is
included. The next prompt starts with 4098 messages. Ordinary prompts still perform four writes
and four `sync_data` calls; the instrumented first prompt spent 10.919 ms of its 14.852 ms mean in
those syncs. The 64 KiB fork buffer reduced underlying File writes from 16386 to 101 and reused
validated state without rereading the output. Queue cancellation keeps four ordinary v4 records
while committing them with one write and one sync. Writer reuse applies to Unix.

An additional `Pi::create_project_session` comparison, with optional features disabled and the same
generation assembly, measured 257.917–263.973 ms for the old scan-before-create path versus
0.499–0.501 ms for direct allocation with 1000 historical files. Its fixture verifies the canonical
project directory, seeded ID lookup and resulting session parent; project path normalization must
not silently turn a populated-directory benchmark into an empty-directory benchmark.

The expanded audit covered 274 cases plus focused checks, retaining 29740 accepted primary raw
samples. A single rename regression candidate did not recur in a separate 50-sample alternating
comparison. Filesystem timings varied between runs; the operation counts and durability regressions
provide independent evidence. The permanent `session_io` suite retains 23 representative workloads
with setup and verification outside timing for future comparisons.
