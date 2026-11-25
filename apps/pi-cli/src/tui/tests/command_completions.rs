use super::*;
use pi_plugin::{CommandCompletions, UiCompletion};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn items(version: usize) -> Vec<UiCompletion> {
    vec![
        UiCompletion {
            value: "view ".into(),
            label: "view".into(),
            description: None,
        },
        UiCompletion {
            value: "visit ".into(),
            label: "visit".into(),
            description: None,
        },
        UiCompletion {
            value: format!("view item-{version}"),
            label: format!("item-{version}"),
            description: Some("generation-owned item".into()),
        },
    ]
}

#[test]
fn registered_arguments_use_the_generic_selector_without_executing_a_command() {
    let mut app = demo_app();
    app.command_completions = vec![CommandCompletions {
        name: "inventory".into(),
        items: items(1),
    }];
    app.input.set_text("/inventory v");
    assert!(completion_panel_visible(&app));
    assert_eq!(
        suggestions_for_app(&app)
            .iter()
            .map(|item| item.invocation.as_str())
            .collect::<Vec<_>>(),
        ["/inventory view ", "/inventory visit "]
    );
    move_command_selection(&mut app, 1);
    assert!(complete_selected_command(&mut app));
    assert_eq!(
        app.input.text(),
        "/inventory visit ",
        "Tab preserves the continuation space"
    );
    app.input.set_text("/inventory view i");
    assert!(
        complete_selected_command_for_enter(&mut app),
        "first Enter only selects metadata"
    );
    assert_eq!(app.input.text(), "/inventory view item-1");
    assert!(
        !complete_selected_command_for_enter(&mut app),
        "the completed invocation is submitted only on the next Enter"
    );
    let screen = render_app(&app, 90, 20);
    assert!(screen.contains("generation-owned item"));
    assert!(screen.contains("item-1"));
    app.dismissed_completion = Some(app.input.text());
    assert!(!completion_panel_visible(&app));
    assert!(suggestions_for_app(&app).is_empty());
    app.input.set_text("/inventory view ");
    assert!(
        completion_panel_visible(&app),
        "editing after Escape restores suggestions"
    );
    app.input.set_text("/inventory view item-1 extra");
    assert!(suggestions_for_app(&app).is_empty());
    assert!(!completion_panel_visible(&app));
    app.input.set_text("/inventory-other v");
    assert!(
        suggestions_for_app(&app).is_empty(),
        "command names match exactly"
    );
}

struct Commands(usize);
#[pi_plugin::plugin]
impl pi_plugin::Plugin for Commands {
    fn id(&self) -> pi_core::PluginId {
        pi_core::PluginId::new("fixture-command-completions")
    }

    fn register(&self, context: &mut pi_plugin::RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_command_with_completions(Arc::new(Inspect), items(self.0))
    }
}
struct Inspect;
#[async_trait::async_trait]
impl pi_plugin::Command for Inspect {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "inventory".into(),
            description: "Fixture".into(),
            argument_hint: Some("[action]".into()),
        }
    }
    async fn execute(
        &self,
        _: pi_plugin::CommandContext,
        _: String,
    ) -> Result<pi_plugin::CommandOutcome, pi_plugin::CommandError> {
        panic!("completion must not execute the command")
    }
}

#[tokio::test]
async fn command_completion_snapshots_follow_only_successful_generation_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let version = Arc::new(AtomicUsize::new(1));
    let fail = Arc::new(AtomicBool::new(false));
    let plugin = Arc::new(pi_test_support::ScriptedProviderPlugin::scripted([]));
    let provider = plugin.provider();
    let sessions = MultiSessionManager::new({
        let version = version.clone();
        let fail = fail.clone();
        move |request: SessionGenerationRequest| {
            let plugin = plugin.clone();
            let version = version.load(Ordering::SeqCst);
            let fail = fail.load(Ordering::SeqCst);
            async move {
                let mut builder = pi_runtime::PiRuntime::builder()
                    .provider_plugin_arc(plugin)
                    .plugin(Commands(version))
                    .agent_options(pi_agent::AgentOptions {
                        provider_id: ProviderId::new("scripted"),
                        model_id: ModelId::new("test"),
                        cwd: request.cwd,
                        ..Default::default()
                    })
                    .system_prompt(pi_runtime::SystemPrompt::Final("test".into()));
                if fail {
                    builder = builder.plugin(Commands(999));
                }
                Ok(PreparedSessionGeneration::new(
                    builder.build()?,
                    AgentSessionOptions::default(),
                ))
            }
        }
    });
    let path = directory.path().join("session.jsonl");
    let handle = sessions
        .create_session(pi_session::CreateSessionOptions::new(
            directory.path(),
            &path,
        ))
        .await
        .unwrap();
    let first = handle.current();
    let mut old = App::new(&first, &first.snapshot());
    old.input.set_text("/inventory view ");
    assert_eq!(
        suggestions_for_app(&old)[0].invocation,
        "/inventory view item-1"
    );
    version.store(2, Ordering::SeqCst);
    fail.store(true, Ordering::SeqCst);
    assert!(handle.reload().await.is_err());
    assert!(Arc::ptr_eq(&first, &handle.current()));
    let retained = App::new(&handle.current(), &handle.current().snapshot());
    assert_eq!(retained.command_completions, old.command_completions);
    fail.store(false, Ordering::SeqCst);
    handle.reload().await.unwrap();
    let next = handle.current();
    assert!(!Arc::ptr_eq(&first, &next));
    let mut refreshed = App::new(&next, &next.snapshot());
    refreshed.input.set_text("/inventory view ");
    assert_eq!(
        suggestions_for_app(&refreshed)[0].invocation,
        "/inventory view item-2"
    );
    assert_eq!(
        suggestions_for_app(&old)[0].invocation,
        "/inventory view item-1",
        "a captured UI snapshot never mixes generations"
    );
    assert!(provider.requests().is_empty());
    assert!(!path.exists(), "completion never materializes a session");
}
