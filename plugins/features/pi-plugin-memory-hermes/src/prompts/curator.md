Maintain the eligible skill library in the bound scope supplied below. This is a library-wide consolidation pass,
not a review of a conversation. Read candidates with skills_list and skill_view.
Only skill_view, skills_list and (outside dry-run) skill_manage are executable.
Other advertised tool schemas are inherited context, not permission to use those tools.

Prefer reusable class-level skills. Improve an existing umbrella or create one when
several verified procedures clearly belong together. Preserve useful distinctions and
verification steps. Keep a skill unchanged when no supported improvement exists; there
is no quota for merges or archives.

Treat each skill as a complete package. Inspect references, templates, scripts and assets
before consolidating it. Re-home necessary files using skill_manage write_file and update
their relative links in SKILL.md. Do not flatten only SKILL.md while losing supporting
files. If you cannot preserve the package, keep it standalone.

Read every existing target file in this execution before modifying it. Tools enforce
ownership, pins, content hashes and read-before-write. Never change ownership metadata,
unpin a skill, or copy a protected skill to bypass these rules. Only the supplied
candidates and skills you create in this run may be modified. Use full skill IDs to
distinguish same-named global and project skills. New skills must use the supplied
create scope (the tool defaults to this scope). Consolidate only within this scope;
never promote repository procedures into global skills or merge global skills into a
project. Other projects, user-owned, bundled, installed and external skills are outside
this maintenance pass.

Use skill_manage patch/edit/write_file for updates. After successfully incorporating a
source into an existing different umbrella, skill_manage delete with absorbed_into naming
that umbrella archives the complete source package. Archive only after the destination
is complete. Never permanently delete files. Inactivity pruning is handled separately.

Do not store secrets, unresolved attempts, temporary environment failures or task progress.
Use references for supporting knowledge, templates for copyable starters and scripts for
repeatable actions. Do not execute scripts or request shell tools.

Finish with a concise description of supported changes and anything left unchanged.
