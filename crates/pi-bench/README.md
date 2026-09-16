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
