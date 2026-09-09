# Subagent smoke test

Use this playbook to verify the first-party `subagent` feature with a real tool-capable model. The
scenario is read-only: every delegated task inspects the repository and leaves the workspace
unchanged.

The built-in `scout`, `worker`, `reviewer`, `oracle`, and `delegate` role prompts are vendored
verbatim from `nicobailon/pi-subagents` at the commit recorded in
`plugins/features/pi-plugin-subagents/NOTICE`. pi-rs keeps its own native execution protocol and
hidden run marker; it does not add a second pi-rs-specific role/depth wrapper around those prompts.
Like upstream, each child receives an active-agent identity, an ordinary-child or explicit-fanout
boundary, and the project context selected by its profile. The supervisor bridge adds
`contact_supervisor` within the parent's active-tool ceiling; explicit exclusions can disable it.

`subagent` accepts optional `context: "fresh" | "fork"`, overriding the profile's `defaultContext`.
Worker/oracle default to fork; scout/reviewer/delegate and unspecified custom profiles use fresh.
Fork inherits history through the user request before the launching assistant/tool batch, not a
workspace snapshot. To check it manually, put a unique fact in an earlier parent message and ask
two parallel reviewer children with `context: "fork"` to recall it without repeating it in their
tasks. A reviewer with `context: "fresh"` should not have that fact. Child persistence and usage
should contain only its own responses plus a separate inherited-context seed.

The scenario uses two real project agent definitions rather than hardcoded test roles:

- `.pi/agents/smoke-scout.md` is a leaf agent with the `smoke-explorer` alias, a `bash` exclusion,
  inherited skills, a total child-run timeout, and `allowNestedSubagents: false`.
- `.pi/agents/smoke-delegate.md` adds `subagent` to that tool allowlist and recursively launches the
  same profile through child depth 6. Its `maxSubagentDepth: 6` declaration may preserve or tighten
  the inherited limit, but cannot widen a stricter global limit.

## Run the scenario

Start the interactive frontend from the repository root:

```bash
PI_SUBAGENT_MAX_DEPTH=6 cargo run -p pi-cli --
```

Accept the project-trust prompt if this checkout has not been trusted yet. Project `.pi/agents`
definitions are intentionally trust-gated.

Run `/session` and record the primary JSONL path. Then send this prompt:

```text
Read docs/subagents-smoke-test.md and execute the Agent procedure exactly. Use the subagent tool for
every delegated step. Return the required final report after all steps finish.
```

One-shot mode can exercise the same flow, although the interactive frontend makes concurrent tool
activity and primary-session ownership easier to observe:

```bash
PI_SUBAGENT_MAX_DEPTH=6 cargo run -p pi-cli -- --print \
  'Read docs/subagents-smoke-test.md and execute the Agent procedure exactly. Use the subagent tool for every delegated step. Return the required final report after all steps finish.'
```

## Agent procedure

Complete the following steps in order. Treat a step as passed only when its subagent result contains
all expected evidence.

1. Launch exactly one `smoke-explorer` subagent by that alias. Ask it to inspect
   `plugins/features/pi-plugin-subagents/src/runtime.rs` and report the default maximum depth,
   cumulative children per root session, and active children. The expected values are `1`, `64`,
   and `20`; its result must end with `SMOKE_SCOUT_OK`.
2. Launch exactly one `smoke-delegate` subagent. Its task must require each delegate at depths 1
   through 5 to launch exactly one further `smoke-delegate`. The depth-6 delegate must inspect
   `isolated_session_path` in `crates/pi-session/src/multi_session_manager.rs` directly and report
   the path shape for a child of `/work/primary.jsonl`:
   `/work/primary/isolated/<uuid>.jsonl`. The depth-1 result must preserve all six markers from
   `SMOKE_DELEGATE_DEPTH_6_OK` through `SMOKE_DELEGATE_DEPTH_1_OK`.
3. In one assistant turn, launch these two independent subagents so the normal parallel tool
   scheduler can run them together:
   - A `smoke-scout` that reports the supported frontmatter field names from `AgentFrontmatter` in
     `plugins/features/pi-plugin-subagents/src/catalog.rs`.
   - A `smoke-scout` that reports the registered tool name and its required input fields from
     `plugins/features/pi-plugin-subagents/src/tool.rs`.
4. Check the returned evidence. The frontmatter fields must be `name`, `description`, `aliases`,
   `tools`, `excludeTools`, `model`, `thinking`, `systemPromptMode`, `inheritSkills`, `skills`,
   `skillPath`, `timeoutMs`, `inheritProjectContext`, `defaultContext`, `allowNestedSubagents`, and `maxSubagentDepth`. The tool
   must be named `subagent`, with required `agent` and `task` fields. Both parallel results must end
   with `SMOKE_SCOUT_OK`.
5. Return exactly this report shape, replacing each result with `PASS` or `FAIL` and adding one short
   evidence line:

```text
SUBAGENTS_SMOKE_TEST
direct: PASS|FAIL - <evidence>
recursive: PASS|FAIL - <evidence>
parallel: PASS|FAIL - <evidence>
workspace: unchanged
```

## Supervisor roundtrip

Run this separately from the fixed-report procedure above:

```text
Launch one reviewer with this read-only task: call contact_supervisor with reason need_decision
and message "Should this review cover tests too?". After the reply, report its exact decision
and finish with SUPERVISOR_ROUNDTRIP_OK. When the child asks, answer "Yes, include tests" using
subagent_supervisor, then use bg_wait on the same run until it finishes. Keep the original child
session; do not launch a replacement. Leave workspace files unchanged.
```

Pass criteria: the initial tool returns a detached receipt; the request reaches the parent; the
reply appears as the original child's `contact_supervisor` result; `bg_wait` reports that run's
terminal result. A `bg_wait` timeout only ends that wait window. For structured input, repeat with
`reason: "interview_request"`, an `interview` object and a JSON reply. `progress_update` should
reach the parent without blocking the child. These checks work with fresh and fork context.

Communication is currently process-local. Restarting the app does not reattach live work, and
`bg_wait({nonBlocking: true})` is explicitly unsupported.

## Human checks

After the report returns:

1. Run `/session` again. Its path must equal the primary path recorded before the scenario.
2. Confirm the UI showed the canonical `smoke-scout` name for the direct alias launch, one outer
   `smoke-delegate`, and two independent
   `smoke-scout` tool calls in the parallel step. The five descendant delegates belong to isolated
   sessions, so the primary frontend does not need to render them as primary tool calls.
3. Inspect the session directory if persistence needs verification. For a primary file
   `/work/primary.jsonl`, the first child is stored under
   `/work/primary/isolated/<uuid>.jsonl`; each further depth repeats the
   `<parent-stem>/isolated/<uuid>.jsonl` nesting until depth 6.
4. Open `/resume` and confirm isolated child files are absent from the top-level resume listing.

The live scenario verifies model-facing wiring and presentation ownership. Model compliance can
vary, so deterministic runtime and limit behavior remains owned by the automated tests:

```bash
cargo test -p pi-plugin-subagents
cargo test -p pi-cli product_runtime_registers_the_first_party_subagent_tool
cargo test -p pi-cli trusted_project_markdown_agents_extend_the_subagent_catalog
```

For a release gate, also run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```
