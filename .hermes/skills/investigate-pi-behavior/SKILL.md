---
name: "investigate-pi-behavior"
description: "Trace how user-visible Pi/pi-rs behavior is produced across provider events, runtime projection, persistence, and frontend rendering, and identify the correct user configuration seam."
version: 4
created: "2026-09-09"
updated: "2026-09-10"
---
## When to Use
Use when a user asks why a Pi/pi-rs UI node, label, language, transcript item, or other visible behavior appeared; where it is generated; or where to configure it. This is for evidence-backed behavior tracing, not speculative answers from screenshots alone.

## Procedure
1. Read AGENTS.md and the owning product documentation before tracing behavior. For CLI/TUI behavior read apps/pi-cli/README.md; for compatibility claims inspect the corresponding source under legacy/pi.
2. Start from the visible artifact or named capability and search its exact text/tool name. If dynamic, work backward through rendering or tool schema, state reduction, event handling, runtime/session ownership, persistence, and provider execution.
3. Distinguish fixed localized UI text, frontend-derived text, provider/model-generated content, and runtime capability semantics. Do not infer implementation status from wording such as “detached” or “background.”
4. For transformed commands such as `/skill:<name>`, trace original display text and expanded model text as separate values across live events, persisted `AgentMessage` metadata, settlement updates, resume projection, title/preview derivation, and final Markdown rendering. Consult `references/desktop-transformed-input-display.md`.
5. For reasoning items, verify whether the transport supplies a separate summary. If summary is empty, inspect fallback title extraction before claiming another model call generated the title.
6. For subagents and background work, trace launch, ownership, monitor, coordination channel, completion, cancellation, persistence, and usage attribution separately. Distinguish an isolated session from an OS process, and distinguish detachment from a tool wait from survival beyond process exit. Consult references/subagent-execution-model.md.
7. For token-count discrepancies, define the metric before calculating: current/root session product total, per-file provider-call total, or recursive physical work across isolated sessions. Inspect parent usage adjustments before adding child files to avoid double counting. Consult references/session-token-accounting.md.
8. When recommending prompt customization, prefer APPEND_SYSTEM.md for additive instructions. Explain that SYSTEM.md replaces the normal system prompt and is inappropriate for a small preference unless replacement is intentional.
9. State configuration scope and precedence: global agent directory versus trusted project .pi resources. Confirm the effective agent directory, including PI_AGENT_DIR overrides, rather than assuming ~/.pi/agent.
10. State the activation boundary accurately: whether reload, a new session, or application restart is required. If the frontend has no proven reload path for resource generations, recommend a full restart and a new conversation.
11. Separate verified mechanics from model compliance or future product requirements. Do not present an independent worker process as inherently more concurrent than Tokio; tie it to lifecycle, fault-isolation, resource-control, or durability requirements.
12. For the verified reasoning-language and APPEND_SYSTEM.md example, consult references/reasoning-language.md.
## Pitfalls
- Inferring the origin of a title from its appearance without following the data path.
- Confusing reasoning effort selection with reasoning language selection.
- Suggesting SYSTEM.md for a small additive preference and unintentionally replacing Pi's default system instructions.
- Claiming UI locale translates provider-generated reasoning.
- Promising that a system-prompt language instruction will control all providers.
- Giving only a path without explaining global versus project scope, trust gating, PI_AGENT_DIR, and when changes take effect.

## Verification
1. Cite the exact backend projection and frontend parsing/rendering locations supporting the explanation.
2. Verify resource discovery and precedence in crates/pi-resources before giving file paths.
3. Check whether the runtime emits reasoning summary events or only reasoning text for the active adapter.
4. Ensure the proposed instruction is additive, contains no secrets, and is tested in a new conversation after reload/restart.
5. Label any remaining provider-specific uncertainty clearly.