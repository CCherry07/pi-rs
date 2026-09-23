# Coding evaluation adapter

`pi-coding-eval` prepares the Coding product for `pi-eval::EvalRunner`. It owns
isolated home and agent directories, bootstrap `auth.json` / `models.json`,
settings, shell environment, extension/native-plugin sources, and Coding prompt
treatments. It builds `pi_coding::Pi` and supplies its existing session manager;
the shared runner owns steps, reload, cancellation, observation, grading, and
artifact persistence.

Build an `EvalCase` with the generic case builders, then wrap it with
`CodingEvalCase::new(case)`. The wrapper adds only `discover_extensions` and
`requires_js_host`. `CodingEvalVariant` selects the prompt treatment and explicit
plugin sources. `CodingEvalHarness::run` accepts the case, variant, repetition,
and the normal Coding `Config`. The CLI in `apps/pi-eval` uses this adapter, and
the Node launcher supplies the ordinary `JsPluginHost` for JavaScript cases.

Each run copies fixture files into an isolated workspace, excluding `.git`,
`node_modules`, and `target`. Tool selections are written to isolated settings
before the first generation is rendered. Coding templates retain `{{workspace}}`,
`{{agent_dir}}`, and `{{home}}`; source plugin paths resolve against the original
working directory. Memory and local MCP discovery are disabled for evaluations.

`WithoutPiDocumentation` removes only the built-in documentation paragraph,
preserving appended instructions, project context, and the working directory.
The evaluation artifact schema and session JSONL remain owned by `pi-eval` and
`pi-session` respectively. The Coding product itself does not depend on this
crate.
