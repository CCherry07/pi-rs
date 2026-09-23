# pi-eval

Domain-neutral evaluation over the shared managed-session lifecycle. `EvalRunner`
creates an isolated fixture workspace, runs prompts, commands, and reloads, grades
observations, and persists native session JSONL plus schema-v1 evaluation artifacts.
`pi-coding-eval` composes the Coding product on this runner; this crate has no Coding
or JavaScript-host dependency.

A target preparer receives `EvalRunContext` once per run and returns
`PreparedEvalTarget::new(manager, provider, model)`. The runner creates its session
through that manager and shuts down the whole manager when the run ends; supply
a manager dedicated to the run. It uses an ordinary `SessionGenerationOverlay` for case
plugins and prompt capture. The preparer owns domain configuration, discovery,
credentials, and any isolated domain resources. Apply `context.active_tools` when
building the target, before its initial dynamic prompt render.

```rust,no_run
use pi_eval::{ArtifactStore, EvalCase, EvalRunner, EvalStep, PreparedEvalTarget};
use pi_sdk::{AgentHost, ModelSelection};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

# async fn example() -> Result<(), pi_eval::EvalError> {
let runner = EvalRunner::new(ArtifactStore::new("eval-artifacts")?);
let case = EvalCase::new("orders/help", "Answer an order question")
    .step(EvalStep::Prompt("How can I track my order?".into()));
let run = runner.run(&case, "order-service", 1, |context| async move {
    let mut builder = AgentHost::builder(
        ModelSelection::new("scripted", "test"),
        "Help customers with their orders.",
    ).provider_plugin_factory(|| ScriptedProviderPlugin::scripted([
        ScriptedTurn::Text("Use the tracking link in your order receipt.".into()),
    ]));
    if let Some(tools) = context.active_tools {
        builder = builder.active_tools(tools);
    }
    Ok(PreparedEvalTarget::new(
        builder.build().session_manager(), "scripted", "test",
    ))
}).await?;
assert!(run.passed);
# Ok(())
# }
```

`PromptTemplate` expands `{{name}}` tokens using only
`PreparedEvalTarget.template_bindings`. There are no implicit workspace, home, or
product-directory bindings; missing bindings fail the step. Values are inserted
literally, without recursive expansion. A target can optionally supply a pure
`prompt_transform`; the overlay reapplies it after reload and captures the prompt
sent to the provider.

Fixture copying and snapshots include all directories by default, including
`.git`, `.pi`, `node_modules`, and `target`. Use
`runner.ignored_directories(["target"])` to exclude directory basenames at every
level from both copying and observation. Fixture symlinks and special files are
rejected. The initial snapshot precedes target preparation, so domain setup changes
inside the workspace remain observable.

Run timing starts before target preparation and ends after manager shutdown; it
excludes fixture copying, the initial snapshot, and artifact persistence. The
runner shuts down every successfully prepared manager, including on session
creation, observation, and artifact errors. A preparer that returns an error owns
cleanup of resources it has not handed to the runner. Timed-out submissions and
reloads receive an abort signal; after a bounded grace period their tasks are
cancelled and joined before workspace cleanup. Cancelling the outer run future triggers the same owned cleanup task: the
active operation is aborted and joined, the manager finishes shutdown, and only
then is the isolated workspace removed. Cancelling during shutdown preserves
that in-flight shutdown future.
