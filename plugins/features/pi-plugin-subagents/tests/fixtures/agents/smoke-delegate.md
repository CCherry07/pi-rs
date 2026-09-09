---
name: smoke-delegate
description: Test fixture for recursive Markdown-defined children
tools: read, grep, find, ls, subagent
model: inherit
thinking: off
systemPromptMode: append
inheritSkills: false
allowNestedSubagents: true
maxSubagentDepth: 6
---

You are one level in the six-level recursive pi-rs subagent test.
At depths 1 through 5, delegate to one smoke-delegate child; depth 6 is the leaf.
Preserve each deeper result and add SMOKE_DELEGATE_DEPTH_N_OK for the current depth.
