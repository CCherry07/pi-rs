# pi-sdk

Domain-neutral agent composition using the existing `MultiSessionManager`, `PiSession`,
and transactional session generation lifecycle. The Coding product's defaults live in
[`pi-coding`](../../domains/coding/README.md).

Supply the model, complete base prompt, and plugin factories explicitly:

```rust,ignore
use pi_sdk::{AgentHost, ModelSelection};

let host = AgentHost::builder(
    ModelSelection::new("my-provider", "my-model"),
    "Answer order questions using the order service.",
)
.provider_plugin_factory(|| MyProviderPlugin::new())
.plugin_factory(|| OrdersPlugin::new())
.build();

let session = host.sessions()
    .create_session(workspace_directory, session_file)
    .await?;
session.current().submit("Where is order ORD-42?").await?;
session.reload().await?;
host.sessions().shutdown().await?;
```

`build()` does not access the filesystem or instantiate plugins. Session creation prepares all
registered factories, validates registration, and binds session capabilities before lifecycle
callbacks and tool execution. Preparation failures leave the previous generation intact.
`into_factory()` exposes the same composition to an existing `MultiSessionManager`.

The SDK does not discover `.pi`, `AGENTS.md`, settings, skills, native plugins, packages,
or environment credentials. Providers and tools are installed only by caller-supplied factories.
All registered tools are initially active unless `active_tools(...)` specifies a subset.
An empty list disables them. `session_options(...)` supplies shared session policy explicitly.

For resource-backed prompts, `AgentHostBuilder::system_prompt(SystemPrompt)` accepts the
runtime's existing dynamic factory. The string passed to `AgentHost::builder` remains the
compatible exact-prompt API; this optional method replaces it:

```rust,ignore
use pi_sdk::{PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt, WorkspaceSnapshot};

let builder = builder.system_prompt(SystemPrompt::dynamic(|workspace: &WorkspaceSnapshot| {
    let policy = std::fs::read_to_string(workspace.cwd().join("policy.txt"))
        .map_err(|error| error.to_string())?;
    Ok(PreparedSystemPrompt::new(move |context: PromptContext<'_>| {
        let tools = context.active_tools.iter()
            .map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(", ");
        Ok(PromptOutput::new(format!("{policy}\nAvailable tools: {tools}")))
    }))
}));
```

This domain chooses and reads its own resources. Preparation runs once per candidate generation,
using that session's saved workspace. The first render receives the selected registered tools:
all tools in name order by default, or the explicit selection in caller order. Tool changes
re-render the prepared inputs, and subsequent turns reuse the assembled prompt. Resources are
read again only when a new generation is prepared, including reload and resume. Prepare or render
errors reject the candidate; a failed tool-selection render also preserves the active selection,
prompt, and journal. No additional prompt lifecycle is introduced.
Reload and resume supply a read-only projection of the recovered configuration before the first
render, including accepted deferred tool changes. Legacy journals without a selection use the host
defaults together with registered restoration additions. Saved names missing from the new registry
are dropped; explicit host defaults remain strict. The same recovery plan repairs the journal only
after generation validation succeeds. Fresh isolated sessions render their inherited or requested
selection. Restoring unchanged tool selections reuses the validated prompt and inspection options.

Each new session receives its workspace and JSONL path from the caller. Use
`create_session_with_workspace` for multiple roots and optional application metadata.
Session capabilities available to plugins and tools refer to that exact managed session;
they support the same history, model, and event operations used by the Coding product.
The `projects` module remains available for applications that want durable project definitions.

The configured model starts fresh sessions. Resume restores the saved selection unless
`AgentSessionOptions::initial_model` explicitly overrides it, and reload retains the live model.
Session history, workspace, tool selection, lazy first-assistant persistence, queues, and
generation replacement use the shared session implementation and v4 storage schema.

Run the self-contained, deterministic order workflow with:

```sh
cargo run -p pi-order-agent-example
```

The example uses a real `lookup_order` tool and a scripted local provider. It performs no
network requests and requires no credentials. See [`examples/order-agent`](../../examples/order-agent/README.md).
