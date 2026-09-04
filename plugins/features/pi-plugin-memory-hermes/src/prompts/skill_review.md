**Skills**: how to do this class of task for this user. Be ACTIVE — most sessions produce at least one skill update, even if small. A pass that does nothing can be a missed learning opportunity; look for real signals before deciding there is nothing to save.

Target shape of the library: CLASS-LEVEL skills, each with a rich SKILL.md and a `references/` directory for session-specific detail. Not a long flat list of narrow one-session-one-skill entries. This shapes HOW you update, not WHETHER you update when a signal is present.

Signals to look for (any one of these warrants action, subject to the protection and evidence rules below):

- User corrected your style, tone, format, legibility, or verbosity. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', 'why are you explaining', 'just give me the answer', 'you always do Y and I hate it', or an explicit 'remember this' are FIRST-CLASS skill signals, not just memory signals. Update the relevant skill to embed the preference so the next session starts already knowing.
- User corrected your workflow, approach, or sequence of steps. Encode the correction as a pitfall or explicit step in the skill that governs that class of task.
- A verified non-trivial technique, fix, workaround, debugging path, or tool-usage pattern emerged that a future session would benefit from. Capture it.
- A skill that got loaded or consulted this session turned out to be wrong, missing a step, or outdated. Patch it, subject to the ownership rules below.

Preference order — prefer the earliest action that fits, but do pick one when a signal above fired:

1. UPDATE A CURRENTLY-LOADED SKILL. Look back through the conversation for skills the user loaded via /skill:<name> or you read via skill_view or Pi's read tool. If one covers the new learning, PATCH that one first. Re-load it with skill_view during this review (see Read-before-write below). It was in play, so it is the right place to extend, provided it is managed by this plugin. Protected and user-owned skills are off-limits however relevant; fall through to the next option.
2. UPDATE AN EXISTING UMBRELLA. Use skills_list and skill_view to find an existing class-level skill. If no loaded skill fits but an existing umbrella does, patch it. Add a subsection, a pitfall, or broaden a trigger.
3. ADD A SUPPORT FILE under an existing umbrella. Use the directory that matches the material:
   - `references/<topic>.md` for session-specific detail (error transcripts, reproduction recipes, provider quirks) and condensed knowledge banks (research, API documentation excerpts, domain notes). Keep it concise and useful for the task, not a full mirror of upstream documentation.
   - `templates/<name>.<ext>` for starter files meant to be copied and modified, such as boilerplate configs, scaffolding, and known-good examples.
   - `scripts/<name>.<ext>` for statically re-runnable actions, such as verification scripts, fixture generators, and deterministic probes that future agents should run rather than hand-type.
   Add support files via skill_manage action=write_file with name, file_path and content. Use a relative file_path starting with references/, templates/, or scripts/. The umbrella's SKILL.md should gain a one-line pointer to each new support file so future agents know it exists; read SKILL.md before adding that pointer.
4. CREATE A NEW CLASS-LEVEL UMBRELLA SKILL only when no existing skill covers the class. The name MUST be at the class level, not a specific PR number, error string, feature codename, library-alone name, or 'fix-X / debug-Y / audit-Z-today' session artifact. If the name only makes sense for today's task, it is wrong — fall back to (1), (2), or (3). Use skill_manage action=create with name, description and content; use scope=project only for repository-specific procedures in a trusted checkout, which writes to its .hermes/skills directory, otherwise the default is global. New skills become /skill:<name> commands after the skill catalog reloads.

Read-before-write (ENFORCED — skill_manage refuses otherwise): before you patch or edit an existing skill's SKILL.md, call skill_view(name) during this review. Before you overwrite or remove an EXISTING supporting file, call skill_view(name, file_path=...) for that exact file. A successful Pi read of the exact file during this review also counts. Content quoted earlier in the conversation transcript does NOT count — the guard requires a fresh load within this review, and your write must be based on what that read returned. Creating a brand-new skill or adding a NEW supporting file needs no prior read. If a write is refused with a read-before-write error, view the named target once and retry the write once; do not loop.

User-preference embedding: when the user expressed a style/format/workflow preference, the update belongs in the SKILL.md body, not just in memory. Memory captures who the user is and their durable preferences; skills capture how to do this class of task for this user. When they complain about how you handled a task, the skill that governs that task needs to carry the lesson.

If you notice two existing skills that overlap, note the overlap in your review reply. Do not perform a speculative broad consolidation during this review.

Protected skills (DO NOT edit these):

- Bundled, installed, and externally owned skills.
- PINNED skills. Pinning blocks all autonomous writes, including content updates. Only foreground user action can change a pinned skill.
- USER-OWNED skills: hand-written skills and anything not managed by this plugin. Loading or consulting a skill does not make it yours to edit.
- Externally changed skills whose content no longer matches the plugin's recorded version.

This review may maintain only agent-created, unpinned skills with an unchanged content hash recorded by this plugin. Skills created through this plugin's skill_manage tool carry that provenance, including creations in a foreground session. Do not change ownership metadata, remove a pin, or copy a protected skill to bypass these restrictions. If a protected skill is wrong or outdated, describe the proposed correction in your review reply and leave the change to the user in a foreground session. If all relevant skills are protected, make no skill writes.

Do NOT capture (these become persistent self-imposed constraints that hurt later when the environment changes):

- Environment-dependent failures: missing binaries, fresh-install errors, post-migration path mismatches, 'command not found', unconfigured credentials, or uninstalled packages. These are fixable setup states, not durable rules.
- Negative claims about tools or features, such as 'browser tools do not work', 'X tool is broken', or 'cannot use Y'. These can harden into future refusals long after the problem was fixed.
- Session-specific transient errors that resolved before the conversation ended. If retrying worked, the lesson is the retry pattern, not the original failure.
- One-off task narratives. A request to summarize today's market or analyze a specific PR is not itself a reusable procedure worth a new skill.
- Unresolved failures: if the session ended WITHOUT finding a working method, do NOT write failed attempts as a 'reliable workflow' or 'recommended approach'. Do not turn an untested sequence into guidance a future session will trust and repeat. Save nothing from those attempts. Only if you independently know a real working alternative (not a guess), capture that alternative — never the dead ends, and never dressed up as best practice.

If a tool failed because of setup state, capture the FIX (a verified install command, configuration step, or environment variable to set) under an existing setup or troubleshooting skill. Never save 'this tool does not work' as a standalone constraint. Do not save task progress or secrets.

A no-op is a real option, but should NOT be the default. If the session ran smoothly with no corrections and produced no new technique, no skill update is needed. Otherwise, act on the supported learning within the ownership rules.
