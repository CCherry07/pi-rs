# Legacy Pi Core Composition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add C01, C05, and C06 acceptance for launch/configuration/trust, model/provider/product authentication, and prompt/context/skill/template composition.

**Architecture:** Scenario sandboxes construct isolated global and project resource trees, then launch both products with explicit artifacts and loopback protocol fixtures. Existing exact prompt/provider/module fixtures remain their historical authorities; vertical scenarios prove resource discovery, selection priority, request projection, persistence, and reload through real products. Protocol fixtures model each distinct built-in wire family, while gateway aliases reuse their owning family rather than duplicating transport tests.

**Tech Stack:** Node conformance harness, Rust coding composition and provider plugins, local HTTP/SSE/OAuth fixtures, existing prompt/provider catalog scripts.

**Spec:** `docs/superpowers/specs/2026-11-28-legacy-pi-core-scenario-conformance-design.md`

## Global Constraints

- Inherit the master plan and completed Foundation/Session Spine interfaces.
- `AGENTS.md`/`CLAUDE.md` context discovery remains independent of project trust; trust gates project `.pi` resources, skills, and native/JS project extensions as documented.
- Initial model priority is explicit request, restorable session model, catalog default, then runtime fallback.
- Credential/environment expansion occurs at request time and raw/semantic evidence must remain redacted.
- Core provider acceptance uses fake local credentials and loopback fixtures only.
- Protocol-family cells are: Anthropic Messages, OpenAI Chat Completions, OpenAI Responses, Google GenerateContent, and AWS Bedrock EventStream. Providers that intentionally route through one of those families do not create a sixth wire family.
- Existing prompt fixtures keep their original oracle revision; adapters cite them but do not silently regenerate them at the new core oracle.

## Review Focus

- Nearest-ancestor trust decisions must not leak between unrelated canonical paths or survive a rejected malformed store write; Task 1 tests symlink/canonical-path and corrupt-store cases.
- Stored credentials and environment references must resolve only at dispatch time and never appear in diagnostics/receipts; Tasks 3–4 use rotated values and marker scans.
- Restored session model must not override a new explicit request, while catalog fallback must not overwrite durable selection; Task 2 asserts all four priority positions.
- Mid-run resource reload must affect the next generation without mutating the in-flight prompt/tool selection; Tasks 5–6 hold an active provider request across reload.
- Provider family fixtures must reject a request accidentally sent in another family's shape even if text output looks correct; Task 3 validates request schemas before responding.

---

## Task 1: Implement C01 launch, precedence, trust, disable, offline, and malformed-config scenarios

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/project-tree.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C01-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C01-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C01-03.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C01-04.mjs`
- Create: `conformance/legacy-pi-core/test/C01-launch-trust.test.mjs`
- Modify only on observed failure: `domains/coding/src/config.rs`, `domains/coding/src/project_trust.rs`, `apps/pi-cli/src/config.rs`, `apps/pi-cli/src/lib.rs`, and focused tests

**Interfaces:**
- Produces `ProjectTreeFixture` for isolated global/project settings, resources, extension paths, trust store, malformed optional files, and request-time environment changes.

- [ ] **Step 1: Write failing C01 tests**

Assert clean isolated launch; global versus trusted-project precedence; accept/reject/remember nearest ancestor; canonical/symlink path identity; rejected project cannot load `.pi` resources; explicit extension survives automatic-discovery disable where specified; `--no-extensions`, `--no-mcp`, `--no-tools`, and `--offline` remain distinct; malformed optional config diagnoses and preserves valid state; unavailable required host/resource returns the expected nonzero exit.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C01-launch-trust.test.mjs`

Expected: FAIL because fixtures/scenarios are missing.

- [ ] **Step 3: Implement fixture and scenario projections**

Drive actual CLI/Node entry points. Capture loaded resource provenance, diagnostics, command/tool inventory, exit, and trust-store bytes. Do not infer loaded state solely from files existing on disk.

- [ ] **Step 4: Run focused owner and formal checks**

```bash
cargo test --locked -p pi-coding project_trust
cargo test --locked -p pi-cli config
node --test conformance/legacy-pi-core/test/C01-launch-trust.test.mjs
node scripts/check-legacy-pi-core.mjs formal --manifest "$CORE_MANIFEST" --group C01 --legacy-dir "$LEGACY_PI_DIR" --rust-cli "$RUST_PI" --node-cli "$NODE_PI" --native-binding "$PI_RS_NATIVE_BINDING_UNDER_TEST" --receipt-dir "$CORE_RECEIPTS/composition-c01-formal"
```

Expected: every mandatory variant has a receipt; ACP-specific disable behavior remains additive where declared.

- [ ] **Step 5: Commit**

Commit harness/captures separately from any owning fix; harness commit message `test(conformance): cover launch and project trust`.

## Task 2: Implement C05-01/02 model priority, models.json routing, and fake credentials

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/model-catalog.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C05-01.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C05-02.mjs`
- Create: `conformance/legacy-pi-core/test/C05-model-routing.test.mjs`
- Modify only on observed failure: `crates/pi-session/src/runtime/selection_intent.rs`, `plugins/providers/pi-plugin-models/src/`, `domains/coding/src/dynamic_providers.rs`, and focused tests

**Interfaces:**
- Produces model-catalog fixture entries with explicit/default/fallback candidates and `${ENV}`/stored fake credential references.

- [ ] **Step 1: Write failing priority/routing tests**

Parameterize explicit request, restorable session selection, catalog default, and runtime fallback. Assert provider/model used in the actual fixture request and durable selection. For models.json, assert custom base URL/routing, request-time environment rotation, stored fake credential use, missing credential diagnostics, and no marker in output.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C05-model-routing.test.mjs`

Expected: FAIL because scenarios are missing.

- [ ] **Step 3: Implement model fixture/scenarios**

Use distinct model IDs and endpoint request markers so priority is observed from the outgoing request, not inferred from UI labels.

- [ ] **Step 4: Verify owner and formal evidence**

Run provider-model package tests, `pi-session` configuration tests, focused Node test, and formal C05-01/02.

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover model selection and routing`.

## Task 3: Implement C05-04 request/header/SSE projection for every wire family

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/provider-families.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C05-04.mjs`
- Create: `conformance/legacy-pi-core/test/C05-provider-families.test.mjs`
- Modify only on observed failure: owning provider plugin under `plugins/providers/`

**Interfaces:**
- Produces strict fixture handlers for `anthropic-messages`, `openai-chat-completions`, `openai-responses`, `google-generate-content`, and `aws-bedrock-eventstream`.

- [ ] **Step 1: Write failing protocol-family tests**

For each family assert URL/path, method, auth/header shape, system/user/tool projection, image policy where supported, streamed text/reasoning/tool deltas, usage, termination marker, transient/permanent status classification, and redacted malformed-response errors. The fixture rejects another family's request schema before sending a response.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C05-provider-families.test.mjs`

Expected: FAIL because protocol fixtures/scenario are missing.

- [ ] **Step 3: Implement strict loopback fixtures and product configuration**

Use fake AWS keys/region for SigV4 and verify signed header structure without checking a real secret. Gateway/OpenRouter/Mistral/xAI aliases map to their actual shared wire family and retain provider-specific module tests outside the vertical pack.

- [ ] **Step 4: Run provider package tests and formal variant cells**

```bash
cargo test --locked -p pi-plugin-anthropic -p pi-plugin-openai -p pi-plugin-google -p pi-plugin-bedrock
node --test conformance/legacy-pi-core/test/C05-provider-families.test.mjs
```

Expected: all families validate request and response semantics; no fixture credential marker appears in receipts.

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover provider protocol families`.

## Task 4: Implement C05-03 and fake product OAuth/device/browser flows

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/provider-oauth.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C05-03.mjs`
- Create: `conformance/legacy-pi-core/test/C05-provider-auth.test.mjs`
- Modify only on observed failure: `apps/pi-cli/src/auth.rs` and OAuth-capable provider plugins (`pi-plugin-anthropic`, `pi-plugin-openai`, `pi-plugin-gateways`, `pi-plugin-openrouter`, `pi-plugin-xai`, `pi-plugin-copilot`)

**Interfaces:**
- Produces fake browser authorization, device-code polling, token refresh, logout, cancellation, and persisted credential probes.

- [ ] **Step 1: Write failing auth/persistence tests**

Assert model/thinking/tool selection changes persist and restore; browser/device flow displays only fake authorization URLs/codes; refresh occurs once under concurrent demand; cancellation leaves no pending listener/state; logout removes only the target identity; invalid/expired responses are redacted and classified.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C05-provider-auth.test.mjs`

Expected: FAIL because fixture/scenario are missing.

- [ ] **Step 3: Implement local auth fixture and real CLI interactions**

Never launch a real browser: provide an injected test opener that records URL and simulates callback/device approval. Rotate fake tokens to prove refresh, and scan all outputs for token markers.

- [ ] **Step 4: Verify and commit**

Run focused provider/auth tests plus formal C05-03/04; commit as `test(conformance): cover provider auth lifecycle`.

## Task 5: Implement C06-01 exact prompt composition through products

**Files:**
- Create: `conformance/legacy-pi-core/scenarios/C06-01.mjs`
- Create: `conformance/legacy-pi-core/test/C06-prompts.test.mjs`
- Modify: `scripts/sync-prompt-conformance.mjs` only if an adapter/export is needed; never overwrite historical fixtures
- Modify only on observed product failure: `domains/coding/src/prompt.rs` and focused prompt tests

**Interfaces:**
- Produces provider-request prompt records for default, restricted-tool, custom, appended, and forced system prompt variants.

- [ ] **Step 1: Write failing product prompt tests**

Assert exact product request prompt and tool-dependent sections for all five variants. Record the historical prompt fixture revision separately from the new live C06 receipt.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C06-prompts.test.mjs`

Expected: FAIL because C06 product scenario is absent.

- [ ] **Step 3: Implement adapters without changing historical captures**

The scenario may import/read reviewed prompt fixtures, but new baseline generation comes only from pinned Legacy live capture and is reviewed separately.

- [ ] **Step 4: Verify**

Run existing prompt sync/check tests, `pi-coding` prompt tests, focused C06 test, then formal C06-01.

- [ ] **Step 5: Commit**

Commit as `test(conformance): verify product prompt composition`.

## Task 6: Implement C06-02/03 context, trust, skills, templates, collision, invocation, and reload

**Files:**
- Create: `conformance/legacy-pi-core/fixtures/resources.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C06-02.mjs`
- Create: `conformance/legacy-pi-core/scenarios/C06-03.mjs`
- Create: `conformance/legacy-pi-core/test/C06-resources.test.mjs`
- Modify only on observed failure: coding resource loader and `plugins/features/pi-plugin-skills/`, `plugins/features/pi-plugin-prompts/`

**Interfaces:**
- Produces nested directory/context trees and generation snapshots before/during/after resource reload.

- [ ] **Step 1: Write failing resource tests**

Assert AGENTS/HERMES/CLAUDE precedence and multi-directory discovery; context remains discoverable independent of trust; project `.pi` skills/templates obey trust; duplicate names produce deterministic collision diagnostics; invocation uses the winning source; reload updates the next generation; an in-flight provider request retains its original prompt/tool-selection rendering.

- [ ] **Step 2: Verify desired-red**

Run: `node --test conformance/legacy-pi-core/test/C06-resources.test.mjs`

Expected: FAIL because fixture/scenarios are missing.

- [ ] **Step 3: Implement with generation barriers**

Hold one provider request at a barrier, mutate/reload resources, assert the held request is unchanged, then start a second run and assert new inventory/prompt.

- [ ] **Step 4: Verify owner and formal evidence**

Run skill/prompt plugin tests, `pi-coding` resource tests, C06 Node tests, and formal C06-02/03.

- [ ] **Step 5: Commit**

Commit as `test(conformance): cover context skills and templates`.

## Task 7: Complete composition CI and documentation

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `conformance/legacy-pi-core/README.md`
- Modify: `docs/architecture.md` only for an actual composition invariant change

- [ ] **Step 1: Add routine C01/C05/C06 baseline shards**

Linux runs complete groups; macOS runs filesystem/auth subset; Windows runs C01 config/path and C05 credential/config subset. Every cell is manifest-driven and rejects undeclared skips.

- [ ] **Step 2: Run tranche verification**

```bash
node --test conformance/legacy-pi-core/test/{C01-launch-trust,C05-model-routing,C05-provider-families,C05-provider-auth,C06-prompts,C06-resources}.test.mjs
cargo test --locked -p pi-coding -p pi-plugin-models -p pi-plugin-anthropic -p pi-plugin-openai -p pi-plugin-google -p pi-plugin-bedrock -p pi-plugin-skills -p pi-plugin-prompts
cargo clippy --locked -p pi-coding -p pi-plugin-models -p pi-plugin-anthropic -p pi-plugin-openai -p pi-plugin-google -p pi-plugin-bedrock -p pi-plugin-skills -p pi-plugin-prompts --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

- [ ] **Step 3: Run GitNexus change detection and commit**

Commit CI/docs as `ci: gate legacy composition scenarios`; product fixes stay in separate owning commits.
