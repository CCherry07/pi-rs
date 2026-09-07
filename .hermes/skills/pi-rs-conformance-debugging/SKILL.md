---
name: "pi-rs-conformance-debugging"
description: "Diagnose and fix pi-rs behavior mismatches against the applicable upstream oracle, with focused red-green tests and full repository quality gates."
version: 1
created: "2026-09-04"
updated: "2026-09-04"
---
## When to Use
Use when pi-rs produces a user-visible behavior, tool schema, dispatch result, wire shape, or notification that differs from Pi or another explicitly pinned upstream oracle such as Hermes.

## Procedure
1. Read AGENTS.md and the relevant architecture/module documentation before changing behavior.
2. Locate the owning Rust adapter and identify the exact behavior oracle. Use legacy/pi for Pi behavior; when architecture pins another upstream implementation, inspect that exact pinned commit rather than assuming legacy/pi owns the feature. If a single raw file is insufficient, download/search the pinned source archive and quote the initialization, counter increment/reset, and final trigger conditions together.
3. When comparing thresholds or scheduling behavior, separate the configured number from its counting unit and trigger boundary: user requests, provider/model iterations, tool batches, final responses, and settled runs are not interchangeable. Verify defaults, increment/reset sites, enablement gates, and when deferred work actually launches.
4. Build a fast regression at the real adapter seam using the exact observed input shape. Run it before the fix and confirm it fails with the reported symptom.
5. Compare dispatch predicates as well as schemas. Dynamic-language truthiness can distinguish a non-empty collection from an empty collection, while a Rust Option check may distinguish only present from absent.
6. List affected projections of the same semantic decision, including execution, notifications, summaries, and persistence. Keep their predicates aligned.
7. Implement policy in the owning adapter. Preserve deeper invariants in storage/runtime APIs unless the oracle requires changing those invariants.
8. Run the focused regression, the owning crate tests, and then the repository gates: cargo fmt --all -- --check; cargo test --workspace; cargo clippy --workspace --all-targets -- -D warnings; git diff --check.
9. Request a focused review for conformance and edge cases before reporting completion.

## Pitfalls
- Do not infer current Pi behavior from memory or an older implementation; inspect the relevant source.
- Do not treat field presence as equivalent to truthiness when porting dispatch logic. Explicitly test omitted, null, empty, and non-empty collection forms where the schema permits them.
- Do not fix only execution if notifications or summaries independently interpret the same arguments; this can make successful mutations invisible.
- Do not weaken a deep API's defensive validation merely to compensate for an adapter dispatch bug.
- A filtered cargo test with --exact can silently run zero tests when the fully qualified test name is unknown; verify the reported test count or use a unique substring first.
- Do not edit architecture documentation for a narrow conformance correction that does not change an architectural invariant.

## Verification
1. The regression test fails before the change with the user's exact symptom and passes afterward.
2. Focused execution and presentation/notification tests both pass when both paths interpret the affected input.
3. The owning crate test suite passes.
4. All repository quality gates pass with no warnings or diff whitespace errors.
5. Review confirms non-empty malformed batches still fail atomically and do not fall back to conflicting top-level fields.