# Built-in tool test conformance

The behavior oracle is the local `legacy/pi` checkout, not a historical test
count. The reviewed inventory lives in
[`domains/coding/tests/fixtures/tool-test-parity.json`](../domains/coding/tests/fixtures/tool-test-parity.json).
It records the oracle revision, SHA-256 of each source file, every test definition,
and the owning executable Rust tests or an explicit boundary explanation.

## Scope and counting

The baseline includes `packages/coding-agent/test/tools.test.ts` and the focused
read-streaming, path, truncation, edit/diff, file-mutation, search-operations,
shell/PowerShell, strict-declaration, and tool-result-image suites named in
[`scripts/check-tool-test-parity.mjs`](../scripts/check-tool-test-parity.mjs).
This is the built-in tool execution contract and its shared support layers, not
all tests in the Pi monorepo. Agent orchestration, provider adapters, and TUI
rendering retain their independently owned suites. The CLI image-caller cases
are mapped to CLI input tests rather than silently excluded. Shared image
processing suites are inventoried too, with individual terminal-only/Node-worker
boundaries recorded explicitly.

The scope audit distinguishes `diff.test.ts` (interactive ANSI diff rendering)
and `edit-tool-no-full-redraw.test.ts` (TUI redraw) from `edit-diff-window.test.ts`
(the tool's returned diff). Clipboard adapters, terminal-width/rendering suites,
queued-input/request-image policy, dynamic tool selection, and deferred tool
search are separate frontend/session contracts. `tools-manager.test.ts` tests
installing external rg/fd release binaries; Rust's native grep/find engines do
not run that installer. These exclusions do not exempt the corresponding tool
outputs, cancellation, or environment-independence behavior.

At oracle revision `69a6993486acee850d14a4a1547daa4a167228ad`, the inventory has
26 files and 240 definitions: 211 mapped definitions, 29 individually explained
boundaries, and no pending definitions. Those are reviewed mappings, not a claim
that every platform has been executed locally.

An inventory entry is a **test definition**, not a Vitest-expanded case or a Rust
function count. `it.each`, loops over shells, and deterministic generated inputs
can exercise many cases within one definition. A Rust table-driven test may
cover multiple definitions, but its assertions must cover the entire relevant
legacy matrix. Conversely, a cross-layer definition can require several Rust
references. This inventory is not a statement/branch coverage percentage.

## Test seams

- Tool arguments and behavior: `Tool::prepare_arguments`, `validate_arguments`,
  `execute`, and returned content/structured output/error information.
- Filesystem effects: isolated temporary directories, exact bytes after edits,
  symlink/hardlink behavior, and shared edit/write mutation ordering.
- Shell behavior: real subprocesses and the existing shell execution seam,
  cancellation, exit/stream settlement, retained output, and Unicode boundaries.
- Resource bounds: streaming and bounded-diff regressions must detect buffering
  the entire large input; a truncated response alone is not sufficient evidence.
- Product handoff: registered model policy, result-hook ordering, persisted tool
  results, and the subsequent deterministic provider request where needed.

Use the TypeScript source and assertions to establish expected behavior. Do not
regenerate expectations from Rust's current output, weaken assertions to make a
port green, or call a similarly named test proof of equivalent coverage.

## Checking and updating

Normal Cargo tests need neither Node nor a legacy checkout:

```sh
cargo test -p pi-coding --test tool_test_parity
cargo test -p pi-coding --test builtin_tool_contracts
```

The first command checks that every frozen definition has a reviewed disposition
and every Rust reference still names a non-ignored test. It does **not** inspect
the meaning of assertions or substitute for running those tests. The complete
workspace gate runs the owning tests; the tool-platform CI job additionally runs
tool/support/media packages on macOS and Windows. Windows-only regressions are
not considered exercised by a successful Unix run.

With the Node host's normal dependencies installed:

```sh
node --test scripts/check-tool-test-parity.test.mjs
node scripts/check-tool-test-parity.mjs
```

When a local `legacy/pi` checkout is present, also verify that the frozen input
still matches it. This parses TypeScript but never executes legacy tests, tools,
providers, or top-level test setup:

```sh
node scripts/check-tool-test-parity.mjs --oracle
```

To adopt changes from the oracle:

```sh
node scripts/check-tool-test-parity.mjs --refresh
```

Refresh preserves reviewed mappings only for unchanged source files and test
bodies. It marks definitions in changed files `pending`, including changes to
shared fixtures or parameter tables outside test bodies. The command deliberately
fails until those definitions have been reviewed and mapped again. Add or update
the real Rust regressions, run them, update their references, and rerun `--oracle`.
Do not replace `pending` with `not-applicable` to hide missing behavior.

## Representation and platform boundaries

Rust has typed executable tools rather than JavaScript function-object wrappers.
JS callback identity and optional `operations` object shapes therefore do not
have literal Rust equivalents; their observable effects should be tested at the
existing Rust seam instead. Where only the absent adapter API is tested, the
inventory must name that fact explicitly. Rust strings cannot hold isolated
UTF-16 surrogate halves; valid Unicode output/truncation remains applicable.

One mixed `tools.test.ts` case checks both a text-only model's image result and
collapsed/expanded `ToolExecutionComponent` warnings. Its mapping covers the
retained image, clean model-facing text, and warning metadata. The terminal
render assertions are explicitly outside this tool-level inventory, not claimed
as completed TUI parity.

Heap limits expressed through Node's V8 flags cannot be copied as Rust flags.
The equivalent regression must constrain or account for the Rust operation's
working memory while still testing the same large-input behavior. Two public-tool
child-process regressions enforce live Rust-allocation budgets above their
initialized-runtime baseline: reading a 200 MiB sparse single-line file under
32 MiB, and editing a 20,000,011-byte LF/ASCII file under 96 MiB. The edit case
checks the exact total length, middle replacement, and both 100,000-line filler
halves. These are not RSS/V8 measurements or a guarantee for every large Unicode
or CRLF edit. The edit red/green comparison already had windowed diffing: it
demonstrates the additional ownership/allocation fixes, not an isolated comparison
of full-file and windowed diff algorithms.

Different image encoders need not produce identical PNG/JPEG bytes: image dimensions,
format, policy, notes, omission/preservation, and content ordering are the
observable contracts.

The Rust-only opt-in `hashline-edit` tool has additional regressions, including
stale-tag rejection. Those tests complement this inventory; they do not count as
coverage for a missing legacy case.
