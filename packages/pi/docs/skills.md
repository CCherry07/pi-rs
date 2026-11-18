# Skills, resources, and project trust

Global resources load from the agent directory. Project resources are discovered from the current
project and its ancestors using Pi-compatible precedence.

```text
~/.pi/agent/
├── auth.json
├── memory.json
├── models.json
├── settings.json
├── trust.json
├── SYSTEM.md
├── APPEND_SYSTEM.md
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/

<project>/.pi/
├── settings.json
├── skills/
├── extensions/
├── plugins.json
├── plugins.lock
└── plugins/
```

Project `.pi` resources are loaded only after the shared trust service approves the project.
Interactive runs prompt when needed; `--print` and `--json` default to untrusted unless a stored
decision or CLI flag decides otherwise. Use `--approve` / `-a` or `--no-approve` / `-na` for a
run-local override.

`AGENTS.override.md`, `AGENTS.md`, and `CLAUDE.md` context discovery is independent of
project trust. Enabling memory also permits the optional `HERMES.md` context source. Trusted repositories may provide `.hermes/skills` and `.agents/skills`; skills
under `~/.agents/skills` are also supported.


Skills use a `SKILL.md` file with YAML `name` and `description`. The system prompt advertises the catalog; read the referenced file before using a skill. `/skill:<name>` expands it explicitly. Resolve relative references from the skill directory.
