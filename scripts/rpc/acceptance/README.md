# Owned-process RPC acceptance

Task 41's **test delivery**, not a second RPC implementation or a declaration that all RPC is conformant.
Only public stdin/stdout commands, real owned-child process signals, the real extension/plugin loaders,
and a loopback OpenAI-completions fixture are exercised. No provider credentials, GUI, installation,
workspace build, or production-code modification is needed by this runner.

## Authority and scope

The oracle is exactly `legacy/pi` commit `05fd3fbda0bbb9a1c7a76e4bf92ab58ed77c8d94`.
Relevant source inspected when writing these scenarios:

- `packages/coding-agent/src/modes/rpc/rpc-mode.ts`: signal registration (377–403),
  receipts/queues/abort (410–457), shutdown/flush/EOF (735–834), dialog correlation (96–164, 777–788).
- `packages/coding-agent/src/core/agent-session.ts`: queue admission (2378–2420),
  clear and abort (2584–2627).
- `packages/coding-agent/src/core/agent-session-runtime.ts:434–442`: abort, shutdown hooks,
  invalidate, and pending credential-write settlement.
- `packages/coding-agent/src/core/output-guard.ts`: ordered stdout and flush.
- `packages/coding-agent/src/modes/rpc/jsonl.ts`: LF-only framing.
- `packages/coding-agent/docs/rpc{,-commands,-extension-ui}.md` document these public seams.

Notable source facts: `abort` is **not** `clear_queue`: it acknowledges after idle with queued
input preserved, without automatically resuming that input. A later explicit prompt starts a new
run, which drains the pending follow-up after its first response. See
`agent-session.ts:1958–1997` and `agent-loop.ts:254–265,311–318`. EOF and signals are separate operations. The first TERM/HUP starts disposal;
a first signal during EOF disposal does not interrupt it; a repeated signal exits immediately.
Legacy intentionally skips stdout flush on SIGTERM (`rpc-mode.ts:743`), unlike EOF/SIGHUP.

The Rust design reference is `docs/architecture.md` (RPC adapter, Session Runtime/activity,
queue persistence) and `apps/pi-cli/README.md` (stdio mode). `--no-approve` controls project
resource trust, **not tool authorization**. Tests never use it as a UI-waiter trigger.

A source statement, an author's in-process regression, and an actual legacy/Rust/Node process
observation are different evidence levels. Harness unit tests below are only **harness self-tests**.
An unavailable exact-build fixture/artifact is `blocked`, never replaced with an old checkout's
binary or counted as a native/Node success.

## Fixed inputs

Obtain a frozen runtime bundle and its source/build/lock manifest from the core owner. Prefer a
minimal copy in this checkout's `.build/rpc-validation/`; otherwise agree an immutable testing
window with the owner. Do not copy their WIP into production source or ask them to commit early.
The owner manifest must identify the actual source snapshot, lock files, CLI, Node package/runtime
dependencies and newly built native binding. Binary hashes alone cannot prove which source a
compiler used; retain the owner's source-to-artifact provenance rather than inventing it.

Make a **local, untracked** input descriptor based on [`inputs.example.json`](inputs.example.json).
All paths are absolute and all artifact hashes must be supplied. The example is intentionally
not runnable until its placeholders are replaced. No target environment is inherited apart from
a small OS/path allowlist; only `PI_RS_NATIVE_BINDING` may be supplied by a target descriptor.

- `legacy`: the pinned prepared source entry and its source resolver, not a synthetic RPC server.
- `rust`: the owner's standalone `bin/pi`.
- `node`: the owner's real `node/dist/bin/pi.js` and explicit fresh `.node` binding.
- `provenance.ownerManifest` names a hash-pinned source/build manifest in `artifacts`.
- `targets.*.artifacts` must cover the executable, entry, binding and optional native fixture.
- Artifacts may be files or whole runtime directories. A directory fingerprint includes all files,
  executable bits, relative names, and in-tree symlinks, excluding only `.git` administration.
  External symlinks are rejected: copy their referents into the frozen runtime, do not silently
  trust mutable dependencies elsewhere. Sorting this filesystem inventory does not sort events.

Compute/check descriptor hashes without running a product:

```sh
node scripts/rpc/acceptance/run.mjs --fingerprint /absolute/frozen/bin/pi
node scripts/rpc/acceptance/run.mjs --fingerprint /absolute/frozen/node
node scripts/rpc/acceptance/run.mjs --fingerprint /absolute/prepared-oracle/legacy
```

The full pinned inventory is checked before and after the suite; executable/entry/binding/plugin
files are also hashed immediately before and after each child. Runner source is hashed too.
Drift invalidates the run and yields `blocked`; old passing observations are not promoted.

## Run

Node 22+ and POSIX are required for the real-signal matrix. Set `EVIDENCE_DIR` to your own private
persistent directory **outside** the repository/build/cache. The runner creates a unique child
evidence directory and never writes host-specific paths into checked-in code.

```sh
node scripts/rpc/acceptance/run.mjs --list
node scripts/rpc/acceptance/run.mjs \
  --inputs .build/rpc-validation/pinned-inputs.json \
  --evidence-dir "$EVIDENCE_DIR" \
  --scenarios eof-probes,hup-idle,term-idle,queue-idle-clear \
  --targets rust,node

# Full matrix; default per-product hard deadline is 25 seconds.
node scripts/rpc/acceptance/run.mjs \
  --inputs .build/rpc-validation/pinned-inputs.json \
  --evidence-dir "$EVIDENCE_DIR"
```

The runner creates only disposable per-run scratch directories below its own
`.build/rpc-validation/`. It neither installs nor rebuilds any runtime. It runs legacy and the
selected candidates **sequentially**, using the same sandbox path, catalog, system prompt and
loopback address, resetting only its own sandbox between processes. The script prints the exact
evidence/work paths. Preserve evidence; disposable scratch may be removed afterward.

## Scenarios

| Family | What it proves/observes |
| --- | --- |
| `eof-probes` | Batched synchronous responses survive real stdin EOF. |
| `{eof,hup,term}-active`, `{hup,term}-idle` | Separate EOF / actual PID signals, numeric exit status, held provider disconnect. |
| `queue-idle-clear` | Idle steering/follow-up order, duplicate text, exact returned queue, no provider request. |
| `queue-order` | Steering priority, one-at-a-time order, prompt streamingBehavior, rejected busy prompt, public history. |
| `clear-then-abort` | Explicit queue clear before abort, idle/settled before acknowledgement, no cleared continuation. |
| `abort-before-headers` | Abort while the local provider has accepted a request but sent no headers. |
| `abort-continues-queue` | Abort acknowledges idle with the queue preserved; a new explicit prompt starts the next run, then the retained follow-up drains. The second settlement is scoped to that new run. |
| `queue-lifecycle-only` | Independent queue-only input recipe: the same abort, explicit new prompt and three provider requests, but no history queries. Every emitted stdout frame still participates in strict comparison. |
| `stream-batched` | Unpaced SSE companion; retains known live snapshot/usage timing differences instead of repairing them. |
| `backpressure-{eof,hup,term}` | Reader pauses inside a >1 MiB response; the name ends in non-whitespace so legal name trimming is not mistaken for output loss. Internal Unicode separators remain. Raw bytes resume intact where flush is required; TERM loss remains an observation. |
| `{hup,term}-dispose`, `eof-then-term` | Shutdown hook is held by HTTP gate; first signal/EOF does not bypass it. |
| `repeated-{hup,term}`, `term-then-hup`, `eof-then-repeated-term` | A second real signal exits with the hook still held. |
| `ui-client-cancel` | Cancellation resolves false, stale correlated replies cannot release a new waiter. |
| `ui-abort` | Active tool UI can be aborted; settlement precedes receipt and the fixture never writes. |
| `ui-{eof,hup,term}` | Pending background UI during disposal; records actual resolution separately from flush and process exit. |

Provider frames are released only after actual stdout delta/HTTP request barriers, not sleeps.
Shutdown gates support a `probe` round trip that leaves the hook blocked. This demonstrates the
explicitly controlled schedule only; a barrier pass is not arbitrary scheduling equivalence.
The `stream-batched` companion intentionally removes delta pacing.

`queue-lifecycle-only` changes **test input**, not comparison: it sends no `get_messages` or
`get_entries`, before or after the new prompt. It shares `abort-continues-queue`'s provider request,
text-delta observation and terminal-frame release barriers identically for every target. Abort must
settle before acknowledgement while preserving one follow-up; the explicit new prompt must lead to
three provider requests overall, then an empty queue and a new non-aborted settlement. All emitted
events/responses, fields and trailing bytes remain subject to the unchanged full-stream comparator.
A green result covers only this queue lifecycle schedule (Task 48), not general RPC/history conformance
or `toolsAdded` coverage. The original `abort-continues-queue` input, including every history query,
remains unchanged; its separate strict/history failure evidence is neither replaced nor relabeled.
Use a new owner-pinned candidate containing the ordering fix for product validation, not repeated
runs of an unchanged failed candidate. Controller self-tests are not three-process acceptance.

## Real native/JS fixtures

[`fixtures/extension.mjs`](fixtures/extension.mjs) uses the real extension API.
[`fixtures/native.rs`](fixtures/native.rs) uses the existing `pi-plugin` API and native loader.
Both use the same HTTP gate/observation contract and install **no signal handlers**. Only the
product under test may handle TERM/HUP. Neither fixture manually cancels background UI.

The core owner builds `native.rs` against the **same frozen SDK fingerprint** as the CLI. Read
`crates/pi-plugin/docs/native.md` first. No workspace/lock/production manifest change is required:
use a private temporary crate under the owner's build area with `[lib]` path pointing to this
file, `crate-type = ["cdylib"]`, and dependencies on that SDK's `pi-plugin` (`native` feature),
`serde_json`, and matching `tokio` (`rt-multi-thread`, `net`, `io-util`, `time`). The fixture owns
a one-worker runtime: a separately linked cdylib cannot assume host Tokio TLS is visible. HTTP
control and background UI work run on that owned handle; nonblocking runtime shutdown cancels
remaining fixture tasks without installing signal handlers. Match the owner's toolchain,
lock, features and Rust flags. The exact-build loader is the compatibility gate; do not bypass it.
Supply the resulting library and its SHA as `targets.rust.nativePlugin`. If absent, Rust's
fixture-dependent rows remain blocked while plain process/queue cases still run.

Environment contract (all generated per child by the runner):

- `RPC_ACCEPTANCE_CONTROL_PORT`: loopback-only fixture port.
- `RPC_ACCEPTANCE_FIXTURE`: `shutdown`, `background-ui`, `input-ui`, or `tool-ui`.
- `RPC_ACCEPTANCE_HOLD_SHUTDOWN=1`: withhold hook completion until HTTP gate release.
- `POST /control/event {name,data}` records `fixture-ready`, `ui-released`,
  `shutdown-enter`, `shutdown-pulse`, `shutdown-complete`.
- `POST /control/gate {name:"shutdown"}` waits for `{action:"probe"|"release"}`.

A `ui-released` marker proves the awaiting callback resumed; **absence is unobserved**, not proof
of a leak. Rust may drop a cancelled callback future instead of running its post-await code.
Process exit or stdout EOF alone does not prove UI-promise resolution. These three observations
are kept separately in the result. The tool fixture always blocks the write even after approval.

## Evidence and verdicts

Each real child has `launch.json` (PID/PGID, executable, arguments, isolated environment), byte-exact
`stdin.raw`, `stdout.raw`, `stderr.raw`, ordered `controller.jsonl`, `fixture.jsonl` (requests and
exact released SSE, no credential headers), `provider-requests.json`, `exit.json` and `result.json`.
`exit.json` distinguishes process `exit`, stdout `end`, pipe `close`, deadline and final partial
bytes. Both inner and separate outer-worker hard deadlines kill only recorded owned process groups.
The final `manifest.json` lists SHA-256 for every evidence file; `manifest.sha256` covers that manifest.

`normalized.jsonl` is the **complete mixed** event/response stream. `normalization.json` enumerates
every mapped value and path. Only generated session/UI/journal identities and known clock values
are mapped, preserving presence and JSON type. No event sorting, field removal, future-usage
insertion, custom-data rewriting or checkpoint suppression is allowed. A valid native v4
`active_tools_change` checkpoint stays visible even when it makes history differ from legacy.
Unterminated final bytes are retained and compared too.

- Per-target `passed/failed/blocked` records behavioral checks and the actual process result.
- Per-candidate comparison independently records full ordered stdout equality and the first
  difference. Provider requests remain complete diagnostic evidence, not secretly normalized.
- Case `passed` requires an oracle behavioral pass, all requested candidate passes, and equal
  mixed stdout. A strict difference is `failed`, even if a narrower shutdown/queue behavior passes.
  UI rows also compare observed resume markers. Missing markers leave UI resolution `unobserved`
  and comparative acceptance `blocked` (unless another check already failed), never falsely passed.
- Missing artifacts/fixtures, an oracle behavioral failure, or an unfinished worker is `blocked`
  for comparative acceptance. Its red trace remains evidence, not a fabricated oracle success.
- A suite-wide hash mismatch makes the whole run invalid/blocked.

Exit status: `0` passed, `1` observed failure/difference, `2` blocked/invalidated.

## Harness validation and handoff

```sh
node --test scripts/rpc/acceptance/test/*.test.mjs
node --check scripts/rpc/acceptance/run.mjs
rustfmt --edition 2024 --check scripts/rpc/acceptance/fixtures/native.rs
git diff --check
```

Self-test probes are explicitly synthetic and must never be entered as product/oracle artifacts.
They verify byte capture, LF-only framing, real owned HUP delivery, HTTP barriers, and normalization
anti-masking rules. This test-only delivery does not require another full workspace/native build.
Report the core owner's compilation evidence separately.

Deliver only this new directory's patch and per-file SHA-256, the pinned-input manifest, exact run
command and persistent evidence manifest. Do not commit/land/push it independently of its core
owner's uncommitted implementation. Failed scenarios are valid runner deliveries; they are not
product approvals.
