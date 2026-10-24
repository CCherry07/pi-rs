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
