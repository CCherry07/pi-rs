# Prompt templates

Put Markdown templates in the agent directory's `prompts/` or a trusted project's `.pi/prompts/`.
The file name becomes a slash command. YAML frontmatter may supply `description`.
The template body supports `$ARGUMENTS` and numbered positional arguments.
Use `prompts` in settings.json for additional files or directories, then `/reload`.

```markdown
---
description: Review a change
---
Review $ARGUMENTS for correctness, compatibility, and missing tests.
```

System instructions are separate: `SYSTEM.md` replaces the default preamble and
`APPEND_SYSTEM.md` adds instructions before project context, skills, and working directory.
See [resource discovery and trust](skills.md) and [extension hooks](extensions.md).
