# Legacy Pi Core-Scenario Conformance Design

## Intent

Build a reviewable, repeatable acceptance program that demonstrates whether `pi-rs` covers the
core user-visible scenarios of Legacy Pi. The program compares real product entry points against a
single pinned Legacy Pi oracle, drives deterministic local fixtures, and records exact evidence for
behavior, persistence, side effects, cancellation, and cleanup.

The purpose is observable compatibility, not internal implementation equivalence. `pi-rs` keeps its
plugin-first Rust architecture, immutable generations, `MultiSessionManager`, Ratatui frontend,
typed provider modules, native-plugin ABI, and Rust-only product extensions. Those choices cannot
be counted as evidence for a Legacy Pi scenario unless the corresponding observable behavior also
passes.

## Why

Existing evidence is strong but fragmented by module and oracle revision:

- prompt and built-in-tool fixtures cover many declaration and execution contracts;
- RPC has a bounded dual-product comparison;
- MCP has a detailed open acceptance ledger;
- Node/NAPI bridge tests cover focused extension behavior;
- session lifecycle tests cover important replacement and close races.

The missing layer is one coherent set of vertical user journeys. Module tests can all pass while a
real launch, reload, session transition, extension callback, MCP recovery, or frontend shutdown still
differs. A common core-scenario program makes those cross-layer handoffs the release evidence.

## Goals

1. Define the Legacy Pi core as explicit user journeys rather than package or symbol inventories.
2. Pin one clean oracle for every new vertical scenario:
   `legacy/pi@2126569de40d5595dd3f41f7af126001f04c1de0`.
3. Drive the same scenario inputs through Legacy Pi and each applicable `pi-rs` entry point.
4. Compare ordered semantic outputs, durable history, filesystem effects, subprocess lifetime, and
   externally visible protocol traffic.
5. Convert every unexplained difference into either:
   - a defect with an owning-module red/green regression;
   - a reviewed, documented product divergence; or
   - a blocked observation whose missing evidence is stated precisely.
6. Preserve existing module fixtures and receipts as historical evidence instead of silently
   regenerating them.
7. Produce release-grade receipts from a clean commit, without real credentials or uncontrolled
   network dependencies.

## Non-goals

- Rewriting Rust internals to resemble TypeScript internals.
- Comparing source layout, allocation strategy, object identity, locale implementation, or exact
  image bytes when those are not product contracts.
- Certifying arbitrary third-party extensions, arbitrary MCP servers, or every live provider.
- Treating Rust-only native plugins, multi-root workspace metadata, subagents, memory, scheduling,
  managed worktrees, or Desktop features as substitutes for Legacy Pi behavior.
- Replacing the complete owning-module test suites with a small number of end-to-end scenarios.
- Automatically updating expected output when Legacy Pi changes.
- Using a passing dirty worktree, a stale native binding, or an installed global binary as formal
  acceptance evidence.

## Anti-goals

The program is considered unsuccessful even if tests are green when any of the following is true:

- meaningful fields are normalized away merely to obtain equality;
- a scenario exercises a mock implementation instead of the product entry point it claims to prove;
- tool calls or other external side effects are replayed to make recovery appear successful;
- one frontend is used as a proxy for another without proving the shared boundary;
- skipped, timed-out, or blocked cases are counted as passes;
- historical failures or oracle revisions are overwritten;
- real user credentials, sessions, home directories, or paid network calls enter fixtures or logs.

## Constraints

- The repository is currently a shared, heavily dirty checkout. Implementation tranches must use a
  clean isolated worktree or an explicitly agreed clean baseline. Unrelated staged and unstaged
  changes must not be rewritten.
- New vertical scenarios use the pinned clean oracle above. Existing Prompt, Tool, RPC, and MCP
  evidence keeps its original oracle revision until a separately reviewed oracle-upgrade change.
- Fixture execution is offline except for loopback servers and local child processes.
- The standalone Rust CLI cannot host general JavaScript extensions. Extension scenarios therefore
  target the Node/NAPI product, while a corresponding standalone scenario verifies the documented
  actionable rejection or native-only behavior.
- Cargo offline mode and Pi product `--offline` have different meanings and must not be conflated.
- Platform-sensitive behavior is accepted only on the platforms named by the scenario. A macOS pass
  does not prove Windows process-tree, path, terminal, or PowerShell behavior.

## Approaches considered

### 1. Module-by-module parity

Audit and repair Session, MCP, providers, extensions, and TUI independently. This gives clear code
ownership and fast focused feedback, but it can miss cross-layer handoff defects and lets similarly
named unit tests masquerade as product equivalence.

### 2. User-journey-only end-to-end parity

Compare complete products without adding focused regressions. This produces strong top-level
signals, but failures are expensive to diagnose, race cases become flaky, and the suite can grow
into a slow duplicate of all module tests.

### 3. Vertical acceptance with owning-module regressions — selected

Define a bounded set of vertical core journeys. Run each against the pinned oracle and applicable
Rust products. When a difference appears, first reproduce it at the deepest owning seam, add a
focused red/green regression, then rerun the vertical journey. The vertical scenario is the
acceptance authority; the module regression is the durable diagnostic and prevention mechanism.

This approach preserves Rust architecture while preventing isolated green suites from being
mistaken for complete compatibility.

## Core-scenario catalogue

Each group contains a small number of representative happy paths plus failure, cancellation, and
cleanup variants. Existing detailed module inventories remain responsible for exhaustive argument
matrices.

### C01 — Launch, configuration, and project trust

- clean start with isolated `HOME`, agent directory, cwd, and deterministic settings;
- global versus trusted-project settings and resource precedence;
- accept, reject, and remembered nearest-ancestor project trust;
- explicit extension paths versus automatic discovery;
- `--no-extensions`, `--no-mcp`, `--no-tools`, and `--offline` boundaries;
- malformed optional configuration produces diagnostics without corrupting valid state;
- exit codes and user-facing errors for unavailable required hosts or resources.

### C02 — Basic agent turn and streaming

- text-only prompt and streamed assistant response;
- reasoning, usage, response metadata, and terminal message fields;
- one and multiple tool calls, including parallel-safe and serial execution;
- tool result persistence before the next provider request;
- provider error before headers, mid-stream error, cancellation, retry, and context overflow;
- event order from user acceptance through `agent_end`/settled state.

All provider behavior uses scripted local providers with recorded requests. No live model is part of
core acceptance.

### C03 — Built-in coding tools and side effects

- representative `read`, `write`, `edit`, `bash`/PowerShell, `grep`, `find`, and `ls` journey;
- relative, parent-relative, home-relative, and absolute path semantics;
- exact file bytes, shared edit/write mutation ordering, truncation evidence, and error messages;
- shell streaming, exit, timeout, cancellation, descendant cleanup, and retained full output;
- image input and tool-result image normalization at policy boundaries.

The existing reviewed tool-test inventory remains the detailed authority. C03 proves product
registration, dispatch, persistence, and follow-up-provider handoff.

### C04 — Session persistence and navigation

- lazy materialization: no file before the first completed assistant response;
- complete assistant/tool pairs and no dangling tool call after interruption;
- reopen current-format history and transparently resume reviewed Legacy v1/v2/v3 fixtures through
  a non-destructive staged v4 copy as specified below;
- new session, resume, fork before/at an entry, tree navigation, labels, and session naming;
- manual and automatic compaction, branch summaries, context edits, and context usage;
- queues, steer/follow-up ordering, abort with retained queued work, retry, and recovery reduction;
- unknown extension-owned wire data survives read/write replay.

Current Rust v4-only metadata is compared through an explicit compatibility projection or recorded
as a reviewed divergence; it must not be erased from native history to make a legacy comparison
pass.

### C05 — Model, provider, and product authentication

- initial model priority: explicit request, restorable session model, catalog default, fallback;
- `models.json` registration, custom routing, environment and stored fake credentials;
- model/thinking/tool-selection changes and persistence;
- local fake OAuth/device/browser flows, refresh, logout, cancellation, and redacted errors;
- provider request projection, headers, usage, SSE termination, and retry classification for the
  supported built-in protocol families.

The core suite validates protocol families with loopback fixtures. Optional live-provider probes are
separate, credential-gated evidence and cannot block fixture-only acceptance unless explicitly made
a release requirement later.

### C06 — Prompts, project context, skills, and templates

- default, restricted-tool, custom, appended, and forced system prompts;
- AGENTS/HERMES/CLAUDE context precedence and multi-directory discovery rules;
- project trust interaction with `.pi` resources;
- skill and prompt-template discovery, collision diagnostics, invocation, and reload;
- tool-selection rerendering without mutating an in-flight run prompt.

Existing exact prompt fixtures remain the declaration/text authority; C06 verifies the product
resource-to-generation path.

### C07 — JavaScript extension lifecycle

- loading explicit and discovered TypeScript/JavaScript extensions through the Node/NAPI product;
- registration and execution of tool, command, provider, agent hook, and session hook;
- input and tool-result patches, provider request hooks, custom entries/messages, and UI dialogs;
- reload success, reload rollback, retirement, stale callback rejection, and callback cancellation;
- new/resume/fork context replacement and session lifecycle ordering;
- extension diagnostics and secret-safe errors;
- process exit and RPC EOF abort active callbacks without leaving the Rust runtime alive.

Direct JavaScript object identity is not a compatibility requirement. The behavior visible through
extension APIs and the product transcript is.

### C08 — MCP static, dynamic, auth, and recovery

- configured global/project stdio and Streamable HTTP servers;
- exposure policy, tool/resource/prompt discovery, qualified naming, and output-schema projection;
- partial startup with one healthy and one failing server;
- HTTP JSON/SSE framing, protocol negotiation, session ID authority, resume cursor, and cancellation;
- safe resource retry, no ambiguous `tools/call` replay, expired-session replacement, lazy reconnect,
  explicit reconnect, and shutdown cleanup;
- OAuth login/logout/refresh with fake browser and local authorization servers;
- Legacy `mcp-auth.json` identity, URL-only migration, locking/CAS, atomic replacement, and crash
  fixtures in both directions;
- extension register/replace/unregister, configured-file precedence, whole-generation publication,
  scope isolation, and observer retirement.

Approved timing differences, such as next-idle whole-generation tool publication, require an
explicit divergence record and a regression for the promised Rust behavior.

### C09 — Noninteractive frontends

- print final text and error exits;
- NDJSON event projection and terminal fields;
- bidirectional RPC commands, correlation, queueing, dialogs, EOF, signals, and backpressure;
- equivalent core session state across frontends where the contract is shared.

Legacy Pi at the pinned oracle has no corresponding ACP product mode. Rust ACP stable-v1 setup,
transient MCP servers, permission prompts, cancellation, and cleanup remain a required real-product
gate with an additive `P` record, but they are not counted as Legacy dual-product (`D`) evidence.

A frontend is exercised through its real executable or adapter. Calling `AgentSession` directly is
supporting module evidence, not C09 acceptance.

### C10 — Interactive TUI and PTY behavior

- fullscreen default and main-screen opt-out;
- streamed Markdown, tool/error/working semantic distinction, and stable spacing relationships;
- Unicode and IME input, history, completion, queued-input preview, scrolling, selection, and copy;
- `/model`, `/resume`, `/mcp`, and `/skill:` selectors;
- extension dialogs/widgets through the Node-hosted Rust interactive product and native-plugin UI
  through the standalone Rust product; standalone Rust is not expected to load general JavaScript;
- resize, suspend/resume, Ctrl-C, EOF/HUP/TERM, terminal restoration, and no protocol bytes on the
  presentation descriptor.

Acceptance uses semantic screen snapshots, accessibility/state probes where available, and bounded
PTY transcripts. It does not require byte-for-byte ANSI output or pixel identity.

### C11 — Replacement, concurrency, and cleanup

- failed candidate preparation leaves the previous generation usable;
- concurrent reload/restart/new/resume/fork ordering;
- manager shutdown versus replacement;
- duplicate close, owner close, isolated descendant close, and acquisition registering during close;
- activated-but-unpublished candidate cancellation;
- dynamic provider/MCP mutations arriving during preparation;
- no orphan child processes, callbacks, transports, locks, session registrations, or partial files.

Deterministic barriers, not sleeps, control every race. C11 is a cross-cutting acceptance group even
when the user-visible symptom appears in another scenario.

### C12 — Packaging and host selection

- standalone native product versus Node/NAPI product selection;
- local binding selection rejects stale packaged bindings in development validation;
- package version, platform artifact, libc, and release-manifest consistency;
- explicit extension behavior and actionable standalone rejection;
- Node process-global offline environment restoration and supported overlap constraints;
- installation-independent fixture paths and no accidental global-package dependency.

## Required scenario set v1

The first acceptance manifest contains exactly the 48 scenario definitions below. Every semicolon-
separated variant named in a row is mandatory. The implementation plan may split one definition
into multiple fixture files or parameterized cases, but it may not omit a named variant or add a
new required scenario without a reviewed design amendment.

| IDs | Required definitions |
| --- | --- |
| C01-01…04 | clean isolated launch; trusted-project precedence and remembered trust; rejected/untrusted project isolation; disable/offline/malformed-optional-config variants |
| C02-01…04 | text stream and terminal metadata; serial/parallel tool continuation; transient/permanent provider failure and bounded retry; pre-header/mid-stream cancellation and context overflow |
| C03-01…04 | filesystem mutation plus all supported path forms; search ordering/context/truncation; shell stream/exit/timeout/cancel/descendant cleanup; image input and tool-result normalization |
| C04-01…06 | lazy materialization; interrupted assistant/tool-pair repair; native v4 reopen; reviewed Legacy v1/v2/v3 migration fixtures; new/resume/fork/tree/label/name transitions; compaction/branch-summary/queue/abort/retry recovery |
| C05-01…04 | initial model priority; models.json custom routing and fake credentials; model/thinking/tool selection persistence; fake OAuth plus representative request/header/SSE projection for each supported protocol family |
| C06-01…03 | default/restricted/custom/appended/forced prompt; context precedence and trust; skill/template discovery/collision/invocation/reload rerendering |
| C07-01…05 | extension load plus tool/command/agent/session hooks; patches/custom entries/provider registration; Node-hosted UI dialog/widget behavior; reload success/rollback/stale-context retirement; RPC EOF/process-exit callback cleanup and secret-safe diagnostics |
| C08-01…07 | configured stdio and HTTP JSON/SSE; partial startup/exposure/schema projection; protocol/session/resume/cancellation authority; safe resource retry with no tool replay; expiry/lazy reconnect/explicit reconnect/shutdown; OAuth storage/login/logout/refresh/crash interoperability; dynamic registration/precedence/scope/observer retirement |
| C09-01…03 | print final/error output; NDJSON ordering and terminal fields; RPC command/correlation/queue/dialog/cancel/EOF/signal/backpressure variants |
| C10-01…03 | fullscreen/main-screen and semantic transcript layout; composer Unicode/IME/history/completion/selectors/queue/scroll/selection/copy; Node-hosted extension UI plus resize/suspend/signal/terminal restoration |
| C11-01…03 | failed preparation and concurrent transition rollback; manager/slot/tree close including in-flight and activated-unpublished candidates; dynamic mutation interleaving and complete resource cleanup |
| C12-01…02 | standalone/Node host selection including stale-binding and explicit-extension behavior; release version/artifact/libc/manifest consistency plus offline environment restoration |

A scenario row is accepted only when all of its mandatory variants have individual receipts. A
single passing variant cannot turn the row green.

## Endpoint and platform applicability

| Groups | Required Legacy endpoint | Required Rust endpoints | Required platforms |
| --- | --- | --- | --- |
| C01–C06 | Legacy Node product | standalone Rust and Node/NAPI Rust; shared outputs must agree | Linux canonical dual run; macOS filesystem/process subset; Windows C03 path/PowerShell and C05 credential/config subset |
| C07 | Legacy Node extension product | Node/NAPI Rust; standalone Rust only for actionable JS-host rejection and native-plugin equivalents | Linux and macOS; Windows extension load/exit smoke |
| C08 | Legacy Node MCP product | standalone Rust for configured native MCP and Node/NAPI Rust for configured plus extension MCP | Linux canonical; macOS stdio/process cleanup; Windows stdio/path/header subset |
| C09 | Legacy print/JSON/RPC modes | standalone and Node/NAPI print/JSON/RPC | Linux canonical; macOS Unix signal/backpressure subset; Windows EOF/cancel subset |
| C10 | Legacy interactive TUI | Node-hosted Rust TUI for JS UI and standalone Rust TUI for native behavior | macOS and Linux PTY; Windows non-PTY input/exit/restore smoke until a reviewed ConPTY driver is added |
| C11 | Legacy lifecycle where the same operation exists | standalone and Node/NAPI Rust; Rust-only isolated-tree cases require a real-product regression plus `P`, not false `D` | Linux canonical and macOS process-cleanup subset |
| C12 | Legacy only where launcher/package behavior has a counterpart | Rust release products and host packages; ACP is a separate Rust-only product gate | every supported release target for package selection; native execution on available macOS/Linux/Windows CI hosts |

`Linux canonical` means the complete fixture-only semantic comparison runs there. Platform subsets
are additional required evidence for platform-sensitive behavior, not permission to infer all
Windows or macOS behavior from Linux. Manifest rows declare the exact endpoint/platform cells and
refuse an undeclared skip.

## Legacy session migration decision

C04-04 covers reviewed v1, v2, and v3 Legacy fixtures through transparent, non-destructive resume.
When a resume target is a supported Legacy file, the product uses the existing import codec to
validate and stage a new v4 destination in the managed session directory. The source is never opened
for writing and remains byte-identical. The destination retains import provenance in its v4 header.
Only after destination validation and complete generation preparation succeed may the managed handle
publish the replacement; cancellation or failure removes the staged destination and preserves the
current session. The product reports that it resumed a compatible copy and identifies the unchanged
source. Direct v4 resume keeps its existing path and does not pass through migration.

The explicit `/import` command remains available. Transparent resume and explicit import share the
same codec, validation, staging, rollback, and provenance implementation rather than maintaining two
migration algorithms. No v1/v2/v3 file is upgraded in place.

## Harness architecture

The new self-contained acceptance area should be:

```text
conformance/legacy-pi-core/
  README.md
  manifest.json
  scenarios/
  drivers/
  normalize/
  fixtures/
  baselines/
  schemas/
```

Generated run receipts belong under an ignored `.eval/project-checks/legacy-pi-core/<run-id>/`
directory. Only reviewed, deterministic baseline data is checked in.

### Checked-in baselines and live oracle execution

Routine Rust CI compares current Rust products against reviewed, checked-in semantic captures so it
does not need to install or execute Legacy Pi. Baseline-check CI, oracle-upgrade work, and every
formal core-acceptance run must also execute the pinned Legacy product, prove its raw and semantic
output still matches the checked-in capture, and then compare the Rust products from the same
scenario run. Therefore:

- a checked-in capture with no current oracle execution is reusable `O` evidence, not fresh `D`;
- a Rust match against that capture is regression evidence, not by itself a new dual-product claim;
- formal `D` requires pinned Legacy and applicable Rust endpoints executed under the same manifest,
  fixture hashes, platform cell, and run receipt;
- CI may shard executions, but the formal receipt indexes all shards and rejects mixed oracle or
  product commits;
- baseline updates are dedicated reviewed changes containing old/new raw evidence and semantic diff.

This keeps routine CI bounded while preventing stale captures from becoming permanent unquestioned
truth.

### Manifest

The manifest records:

- schema version;
- pinned Legacy Pi commit;
- scenario ID and human-readable contract;
- applicable endpoints: Legacy, standalone Rust, Node/NAPI Rust, ACP, or PTY;
- fixture and driver hashes;
- normalizers applied;
- required platform set;
- expected acceptance state;
- owning Rust module tests;
- approved divergence reference, if any.

A run refuses to compare when the Legacy checkout, driver, or fixture does not match the manifest.
It never refreshes baselines implicitly.

### Drivers

A Node-based orchestrator launches product processes because it can execute the Legacy TypeScript
oracle and parse the existing JSON/RPC streams without adding a production dependency. Python or a
small Rust helper may own PTY/platform mechanics where Node cannot provide deterministic terminal
control. Drivers use:

- isolated temporary `HOME`, cwd, agent directory, and session directory;
- local scripted provider, OAuth, MCP HTTP/SSE, and stdio fixture processes;
- explicit timeouts and condition/barrier synchronization;
- exact subprocess and open-port cleanup;
- capture of stdout, stderr, protocol traffic, files, process exits, and fixture-side requests.

The harness invokes already-built products selected by explicit paths. Building is a separate step
whose source revision and artifact hashes enter the receipt.

### Comparison model

Comparison occurs in layers:

1. **Raw evidence** — immutable bytes, files, requests, exits, and timing markers.
2. **Parsed records** — validated against scenario-specific schemas.
3. **Reviewed normalization** — only unstable identities such as temporary absolute paths,
   generated UUIDs, timestamps, and port numbers.
4. **Semantic comparison** — ordered records, state transitions, side effects, and cleanup.

Normalizers are named, unit-tested, and scenario-declared. They may replace a volatile value but
may not drop a record, reorder events, fill future data into an earlier snapshot, convert an error
to success, or remove native history extensions. If representations differ legitimately, a
scenario-specific projector exposes the common contract while raw evidence remains available.

## Evidence states

Reuse the established conformance vocabulary:

- **S — Source reviewed:** implementation and oracle paths are mapped.
- **O — Oracle executed:** the pinned Legacy behavior was captured with the declared fixture.
- **R — Rust regression:** the owning seam has a focused executable regression.
- **D — Dual product:** applicable real Legacy and Rust entry points pass semantic comparison.
- **P — Product divergence:** behavior differs intentionally and has an approved divergence record.
- **Blocked:** required observation cannot currently be made; reason and unblock action are explicit.
- **Failed:** comparison ran and found an unexplained difference.

A core scenario is accepted only with `D`, or with `D` for its common contract plus `P` for every
remaining difference. `S`, `O`, `R`, timeout, skip, and Blocked are never counted as acceptance.
Reports show counts by state and scenario group; they do not collapse progress into a percentage.

## Divergence register

Every approved divergence records:

- stable ID and scenario IDs;
- exact Legacy and Rust behavior;
- user-visible consequence;
- why preserving the Rust behavior is preferable;
- compatibility/migration guidance;
- owner and approval date;
- documentation and regression paths;
- conditions that would reopen the decision.

Internal implementation differences need no divergence record when semantic comparison passes.
Existing approved MCP timing/connection-reuse differences and Rust v4 storage extensions must be
migrated into this register rather than copied as informal prose.

## Failure-to-fix workflow

For each failed vertical scenario:

1. Preserve raw Legacy and Rust evidence and identify the first semantic divergence.
2. Trace the difference to the deepest owning module.
3. Add the smallest deterministic regression that fails on the old Rust behavior.
4. Fix only that owning seam; do not special-case the scenario name in generic runtime code.
5. Run the focused regression and affected package checks.
6. Rerun the unchanged vertical scenario.
7. Update the manifest only for new test ownership or an explicitly approved normalizer/divergence.
8. Preserve both red and green receipts.

Three unsuccessful fixes to one symptom trigger an architectural review rather than a fourth patch.

## Execution ordering

The detailed implementation plan should preserve this dependency order:

1. **Foundation:** manifest/schema, product artifact selection, isolated fixtures, receipt format,
   normalizer tests, and a minimal C02 golden-path scenario.
2. **Session spine:** C02–C04 plus C11 lifecycle barriers. Every later scenario depends on reliable
   persistence, cancellation, and replacement evidence.
3. **Configuration and product composition:** C01, C05, and C06.
4. **Extension boundary:** C07 through both Node/NAPI and standalone rejection paths.
5. **MCP:** C08, reusing the stable extension/session lifecycle and completing auth-store
   interoperability.
6. **Frontends:** C09 and C10, including the existing RPC evidence and new PTY acceptance.
7. **Packaging and release gate:** C12, cross-platform jobs, complete clean-commit receipts, and
   documentation of accepted divergences.

Within a stage, independent scenario fixtures may be implemented in parallel. Changes to shared
session, generation, plugin, RPC, or persistence contracts remain sequential and require the
full-workspace gate.

## Formal acceptance

Coverage of Legacy Pi core scenarios is complete only when:

1. every required manifest scenario is `D` or has explicit `P` dispositions for all differences;
2. no required scenario is Failed, Blocked, silently skipped, or represented only by module tests;
3. standalone and Node/NAPI Rust outputs agree wherever they share a contract;
4. fixture-only official MCP conformance and the declared RPC/PTy packs pass at their reviewed
   baseline;
5. macOS, Linux, and Windows platform-sensitive subsets pass in CI;
6. package/Cargo/release versions are coherent and the complete Node package suite is green;
7. workspace tests, all-target Clippy with warnings denied, formatting, and diff checks pass;
8. the products and receipts were rebuilt from one clean commit with exact source/artifact hashes;
9. documentation lists every approved divergence and does not claim untested parity;
10. no real credentials, user files, paid requests, or uncontrolled network access were used.

A clean core acceptance does not certify all optional Rust-only features or arbitrary ecosystem
plugins. Those retain their own product and compatibility matrices.

## Risks and mitigations

- **Suite size and runtime:** keep vertical scenarios representative; detailed matrices stay in
  owning packages. Split deterministic groups across CI without changing scenario identity.
- **Flaky concurrency:** use explicit fixture barriers and state conditions, never timing sleeps as
  correctness assertions.
- **Over-normalization:** require named normalizer tests and raw-evidence review for every new rule.
- **Oracle drift:** refuse mismatched commits and review upgrades as dedicated changes.
- **Stale Rust artifacts:** record source inputs and native binding hashes; build and execution are
  separate evidenced steps.
- **Platform drift:** declare platform applicability per scenario and avoid inferring Windows from
  Unix behavior.
- **Secret leakage:** fake credentials only, redacted diagnostics, bounded logs, and repository-wide
  marker scans in the acceptance gate.
- **Shared dirty checkout:** perform implementation in isolated clean worktrees and merge only
  reviewed changes; do not manufacture acceptance from the current mixed snapshot.

## Decisions left to the plan author

The implementation plan may choose exact scenario file encoding, schema library, process-control
helper boundaries, and CI shard sizes, provided it preserves the manifest, evidence, normalization,
and acceptance contracts above. It may reuse existing RPC, prompt, tool, MCP, PTY, and bridge
fixtures instead of moving them immediately; adapters must reference their immutable inputs and
must not hide their historical oracle revisions.
