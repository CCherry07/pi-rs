# Reasoning language and title derivation

## Verified pi-rs Desktop path

- Provider reasoning becomes `StreamEvent::ThinkingStart` and `StreamEvent::ThinkingDelta`.
- `apps/pi-desktop/src-tauri/src/pi_runtime/projection.rs` projects this as a Desktop item with `type: "reasoning"`, an empty `summary`, and the streamed text in `content`.
- `apps/pi-desktop/src/features/messages/utils/messageRenderUtils.ts::parseReasoning` uses the first non-empty summary line as the title; when summary is empty, it falls back to the first non-empty content line. It removes that line from the displayed body and truncates the title to 80 characters.
- Therefore a title such as “Clarifying my approach” can be model-generated reasoning text rather than a fixed UI string or a separately generated summary.

## Prompt configuration

Use additive prompt configuration for a language preference:

- Global: `<agent-dir>/APPEND_SYSTEM.md`; the default agent directory is `~/.pi/agent`, unless `PI_AGENT_DIR` overrides it.
- Project: `<project>/.pi/APPEND_SYSTEM.md`; project `.pi` resources require project trust.
- Avoid `SYSTEM.md` for this purpose because it replaces the normal system prompt rather than appending to it.

Suggested content:

```md
## Language

除非用户明确要求使用其他语言，否则所有面向用户可见的内容都使用简体中文，包括最终回答、推理摘要、reasoning 节点标题、可见的思考过程、计划、进度说明和错误解释。

代码、命令、文件路径、API 名称和无法准确翻译的专有名词保持原样。
```

Reload/restart the product and use a new conversation. This instruction improves compliance but does not guarantee reasoning language for every provider/model.
