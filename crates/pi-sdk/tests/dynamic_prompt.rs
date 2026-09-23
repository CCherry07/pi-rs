use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_sdk::{
    AgentHost, AgentHostBuilder, ModelSelection, PreparedSystemPrompt, PromptContext, PromptOutput,
    SystemPrompt, WorkspaceSnapshot, WorkspaceSpec,
};
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rendered {
    policy: String,
    tools: Vec<String>,
    workspace: WorkspaceSpec,
}

#[derive(Clone, Default)]
struct Domain {
    prepares: Arc<AtomicUsize>,
    registrations: Arc<AtomicUsize>,
    rendered: Arc<Mutex<Vec<Rendered>>>,
    providers: Arc<Mutex<Vec<Arc<ScriptedProvider>>>>,
}

impl Domain {
    fn builder(&self) -> AgentHostBuilder {
        let source = self.clone();
        let registrations = Arc::clone(&self.registrations);
        let providers = Arc::clone(&self.providers);
        AgentHost::builder(ModelSelection::new("scripted", "test"), "static fallback")
            .system_prompt(SystemPrompt::dynamic(
                move |workspace: &WorkspaceSnapshot| {
                    source.prepares.fetch_add(1, Ordering::SeqCst);
                    // Domain-owned IO: the SDK does not choose a filename or discover this resource.
                    let policy = std::fs::read_to_string(workspace.cwd().join("policy.txt"))
                        .map_err(|error| error.to_string())?;
                    if policy == "invalid-prepare" {
                        return Err("policy preparation failed".into());
                    }
                    let prepared_workspace = workspace.spec().clone();
                    let rendered = Arc::clone(&source.rendered);
                    let mut prepared =
                        PreparedSystemPrompt::new(move |context: PromptContext<'_>| {
                            assert_eq!(context.workspace.spec(), &prepared_workspace);
                            let tools = context
                                .active_tools
                                .iter()
                                .map(|tool| tool.name.clone())
                                .collect::<Vec<_>>();
                            if policy == "invalid-render" {
                                return Err("policy render failed".into());
                            }
                            if policy == "echo-only" && tools != ["echo"] {
                                return Err("policy permits only echo".into());
                            }
                            if policy == "delay-only" && tools != ["delay"] {
                                return Err("policy permits only delay".into());
                            }
                            if policy == "requires-delay"
                                && !tools.iter().any(|tool| tool == "delay")
                            {
                                return Err("policy requires delay".into());
                            }
                            if policy != "allow-empty" && tools.is_empty() {
                                return Err(
                                    "this policy requires selected tools on its first render"
                                        .into(),
                                );
                            }
                            rendered.lock().unwrap().push(Rendered {
                                policy: policy.clone(),
                                tools: tools.clone(),
                                workspace: prepared_workspace.clone(),
                            });
                            Ok(PromptOutput {
                                system_prompt: format!(
                                    "{policy}; tools={}; cwd={}",
                                    tools.join(","),
                                    context.workspace.cwd().display()
                                ),
                                options: Some(json!({"policy": policy, "tools": tools})),
                            })
                        });
                    prepared.resource_options =
                        Some(json!({"policyFile": workspace.cwd().join("policy.txt")}));
                    Ok(prepared)
                },
            ))
            .plugin_factory(move || {
                registrations.fetch_add(1, Ordering::SeqCst);
                TestToolsPlugin::new()
            })
            .provider_plugin_factory(move || {
                let plugin = ScriptedProviderPlugin::scripted(
                    (0..5).map(|_| ScriptedTurn::Text("Domain response".into())),
                );
                providers.lock().unwrap().push(plugin.provider());
                plugin
            })
    }

    fn requests(&self, generation: usize) -> Vec<pi_plugin::ProviderRequest> {
        self.providers.lock().unwrap()[generation].requests()
    }
}

fn policy(root: &Path, content: &str) {
    std::fs::write(root.join("policy.txt"), content).unwrap();
}

#[tokio::test]
async fn first_render_uses_registered_tools_or_the_exact_explicit_selection() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "version-1");
    for selection in [
        None,
        Some(vec!["echo".into(), "delay".into(), "echo".into()]),
    ] {
        let domain = Domain::default();
        let builder = domain.builder();
        let host = match selection {
            Some(tools) => builder.active_tools(tools).build(),
            None => builder.build(),
        };
        let session = host
            .sessions()
            .create_session(directory.path(), directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let expected = session.current().runtime().active_tools();
        assert!(!expected.is_empty());
        assert_eq!(domain.rendered.lock().unwrap()[0].tools, expected);
        assert_eq!(domain.rendered.lock().unwrap().len(), 1);
        assert_eq!(
            session.current().runtime().prompt_options().unwrap()["tools"],
            json!(expected)
        );
        assert_eq!(domain.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(domain.registrations.load(Ordering::SeqCst), 1);
        if expected.len() == 2 {
            assert_eq!(expected, ["echo", "delay"]);
        }
        session
            .current()
            .submit("Use the configured policy.")
            .await
            .unwrap();
        assert_eq!(
            domain.requests(0)[0]
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            expected
        );
        host.sessions().shutdown().await.unwrap();
        std::fs::remove_file(directory.path().join("session.jsonl")).unwrap();
    }

    policy(directory.path(), "allow-empty");
    let domain = Domain::default();
    let host = domain.builder().active_tools(Vec::new()).build();
    let session = host
        .sessions()
        .create_session(directory.path(), directory.path().join("empty.jsonl"))
        .await
        .unwrap();
    assert!(session.current().runtime().active_tools().is_empty());
    assert!(domain.rendered.lock().unwrap()[0].tools.is_empty());
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn frozen_prompt_inputs_survive_tool_changes_and_turns_until_reload() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "version-1");
    let domain = Domain::default();
    let host = domain.builder().build();
    let session = host
        .sessions()
        .create_session(directory.path(), directory.path().join("session.jsonl"))
        .await
        .unwrap();
    session.current().submit("First question.").await.unwrap();
    policy(directory.path(), "echo-only");
    session.current().set_active_tools(["echo"]).unwrap();
    session.current().submit("Second question.").await.unwrap();
    session.current().submit("Third question.").await.unwrap();
    assert_eq!(domain.prepares.load(Ordering::SeqCst), 1);
    for request in domain.requests(0) {
        assert!(request.system_prompt.starts_with("version-1;"));
    }
    assert_eq!(
        session.current().runtime().prompt_options().unwrap()["tools"],
        json!(["echo"])
    );

    session.reload().await.unwrap();
    assert_eq!(domain.prepares.load(Ordering::SeqCst), 2);
    assert_eq!(domain.registrations.load(Ordering::SeqCst), 2);
    assert_eq!(session.current().runtime().active_tools(), ["echo"]);
    session
        .current()
        .submit("Question after reload.")
        .await
        .unwrap();
    assert!(
        domain.requests(1)[0]
            .system_prompt
            .starts_with("echo-only; tools=echo;")
    );
    let child = session
        .launch_isolated_session(pi_plugin::IsolatedSessionRequest::new(
            pi_core::CustomMessageContent::Text("Inherit the selected domain capability.".into()),
        ))
        .await
        .unwrap();
    session.wait_for_isolated_session(&child).await.unwrap();
    assert!(
        domain.requests(2)[0]
            .system_prompt
            .starts_with("echo-only; tools=echo;")
    );
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_preparation_or_render_preserves_the_live_session_and_journal() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "version-1");
    let domain = Domain::default();
    let host = domain.builder().build();
    let path = directory.path().join("session.jsonl");
    let session = host
        .sessions()
        .create_session(directory.path(), &path)
        .await
        .unwrap();
    session.current().submit("Start.").await.unwrap();
    session.current().set_active_tools(["echo"]).unwrap();
    for invalid in ["invalid-prepare", "invalid-render"] {
        let previous = session.current();
        let saved = std::fs::read(&path).unwrap();
        let prompt = previous.runtime().agent().state().system_prompt.clone();
        policy(directory.path(), invalid);
        assert!(session.reload().await.is_err());
        assert!(Arc::ptr_eq(&previous, &session.current()));
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        assert_eq!(
            session.current().runtime().agent().state().system_prompt,
            prompt
        );
        assert_eq!(session.current().runtime().active_tools(), ["echo"]);
        session
            .current()
            .submit("Continue with the valid generation.")
            .await
            .unwrap();
    }
    assert!(
        domain
            .requests(0)
            .iter()
            .all(|request| request.system_prompt.starts_with("version-1;"))
    );
    host.sessions().shutdown().await.unwrap();

    policy(directory.path(), "echo-only");
    let domain = Domain::default();
    let host = domain.builder().build();
    let session = host.sessions().open_session(&path).await.unwrap();
    let saved = std::fs::read(&path).unwrap();
    let prompt = session
        .current()
        .runtime()
        .agent()
        .state()
        .system_prompt
        .clone();
    assert!(session.current().set_active_tools(["delay"]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert_eq!(
        session.current().runtime().agent().state().system_prompt,
        prompt
    );
    assert_eq!(session.current().runtime().active_tools(), ["echo"]);
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn resume_prepares_current_domain_resources_in_the_saved_workspace_and_restores_tools() {
    let directory = tempfile::tempdir().unwrap();
    let orders = directory.path().join("orders");
    std::fs::create_dir(&orders).unwrap();
    policy(&orders, "version-1");
    let workspace = WorkspaceSpec::new(
        vec![
            pi_core::WorkspaceRoot::external("orders", "Orders", &orders),
            pi_core::WorkspaceRoot::external(
                "catalog",
                "Catalog",
                directory.path().join("catalog"),
            ),
        ],
        pi_core::WorkspaceRootId::new("orders"),
        &orders,
    )
    .unwrap();
    let path = directory.path().join("session.jsonl");
    let domain = Domain::default();
    let host = domain.builder().build();
    let session = host
        .sessions()
        .create_session_with_workspace(workspace.clone(), &path, None, None)
        .await
        .unwrap();
    let id = session.current().log().id().to_owned();
    session.current().set_active_tools(["echo"]).unwrap();
    session
        .current()
        .submit("Persist this conversation.")
        .await
        .unwrap();
    host.sessions().shutdown().await.unwrap();

    policy(&orders, "echo-only");
    let current_domain = Domain::default();
    let resumed_host = current_domain.builder().build();
    let resumed = resumed_host.sessions().open_session(&path).await.unwrap();
    assert_eq!(resumed.current().log().id(), id);
    assert_eq!(resumed.current().runtime().workspace().spec(), &workspace);
    assert_eq!(resumed.current().runtime().active_tools(), ["echo"]);
    assert!(
        current_domain
            .rendered
            .lock()
            .unwrap()
            .iter()
            .all(|rendered| rendered.workspace == workspace && rendered.policy == "echo-only")
    );
    assert_eq!(current_domain.prepares.load(Ordering::SeqCst), 1);
    resumed
        .current()
        .submit("Use the current domain policy.")
        .await
        .unwrap();
    assert!(
        current_domain.requests(0)[0]
            .system_prompt
            .starts_with("echo-only; tools=echo;")
    );
    resumed_host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_explicit_tools_fail_before_first_render_and_persistence() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "version-1");
    let domain = Domain::default();
    let host = domain
        .builder()
        .active_tools(vec!["unknown".into()])
        .build();
    let path = directory.path().join("session.jsonl");
    let result = host
        .sessions()
        .create_session(directory.path(), &path)
        .await;
    assert!(result.err().unwrap().to_string().contains("unknown"));
    assert!(domain.rendered.lock().unwrap().is_empty());
    assert!(!path.exists());
}

#[tokio::test]
async fn legacy_journal_without_tool_selection_renders_defaults_and_additions_together() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "requires-delay");
    let path = directory.path().join("legacy.jsonl");
    let log = pi_session::SessionLog::create(
        &path,
        pi_session::SessionHeader::new("legacy-tools", directory.path()),
    )
    .unwrap();
    assert!(log.context().unwrap().active_tool_names.is_none());
    drop(log);
    let domain = Domain::default();
    let host = domain
        .builder()
        .active_tools(vec!["echo".into()])
        .session_options(
            pi_session::AgentSessionOptions::default()
                .additional_active_tools(vec!["delay".into(), "removed-addition".into()]),
        )
        .build();
    let session = host.sessions().open_session(&path).await.unwrap();
    assert_eq!(
        session.current().runtime().active_tools(),
        ["echo", "delay"]
    );
    assert!(
        domain
            .rendered
            .lock()
            .unwrap()
            .iter()
            .all(|rendered| rendered.tools == ["echo", "delay"])
    );
    session
        .current()
        .submit("Use both configured capabilities.")
        .await
        .unwrap();
    assert_eq!(
        domain.requests(0)[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", "delay"]
    );
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn legacy_journal_keeps_explicit_caller_tools_strict_before_first_render() {
    let directory = tempfile::tempdir().unwrap();
    policy(directory.path(), "requires-delay");
    let path = directory.path().join("legacy.jsonl");
    let log = pi_session::SessionLog::create(
        &path,
        pi_session::SessionHeader::new("legacy-unknown", directory.path()),
    )
    .unwrap();
    drop(log);
    let saved = std::fs::read(&path).unwrap();
    let domain = Domain::default();
    let host = domain
        .builder()
        .active_tools(vec!["unknown-explicit".into()])
        .session_options(
            pi_session::AgentSessionOptions::default()
                .additional_active_tools(vec!["delay".into()]),
        )
        .build();
    let result = host.sessions().open_session(&path).await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("unknown-explicit")
    );
    assert!(domain.rendered.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_recovery_writes_determine_first_render_without_mutating_a_rejected_journal() {
    use pi_session::{
        ActiveToolsEntry, LaneRecordEntry, MAIN_LANE, NewLaneRecord, OperationIntent,
        ProvisionedEntry, SessionEntry,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interrupted.jsonl");
    let log = pi_session::SessionLog::create(
        &path,
        pi_session::SessionHeader::new("interrupted-tools", directory.path()),
    )
    .unwrap();
    let anchor = log
        .append_session_record(SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: vec!["echo".into()],
        }))
        .unwrap();
    log.append_record(NewLaneRecord {
        id: "interrupted-run".into(),
        lane: MAIN_LANE.into(),
        record: LaneRecordEntry::OperationStarted {
            source_leaf_id: Some(anchor.id),
            intent: OperationIntent::Run {
                original_prompt: Vec::new(),
                initial_messages: Vec::new(),
                system_prompt_override: None,
                resume_data: None,
            },
        },
    })
    .unwrap();
    log.append_record(NewLaneRecord {
        id: "accepted-selection".into(),
        lane: MAIN_LANE.into(),
        record: LaneRecordEntry::WriteDeferred {
            run_id: "interrupted-run".into(),
            target: ProvisionedEntry {
                id: "recovered-selection".into(),
                entry: SessionEntry::ActiveToolsChange(ActiveToolsEntry {
                    active_tool_names: vec!["delay".into()],
                }),
            },
        },
    })
    .unwrap();
    let saved = std::fs::read(&path).unwrap();
    drop(log);

    policy(directory.path(), "invalid-render");
    let domain = Domain::default();
    let host = domain.builder().build();
    assert!(host.sessions().open_session(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert!(host.sessions().sessions().is_empty());
    let unchanged = pi_session::SessionLog::open_handle(&path).unwrap();
    assert!(unchanged.get_entry("recovered-selection").is_none());
    assert_eq!(
        unchanged
            .find_open_operations(MAIN_LANE, None)
            .unwrap()
            .len(),
        1
    );
    drop(unchanged);

    policy(directory.path(), "delay-only");
    let session = host.sessions().open_session(&path).await.unwrap();
    assert_eq!(session.current().runtime().active_tools(), ["delay"]);
    assert_eq!(domain.rendered.lock().unwrap().len(), 1);
    assert!(
        domain
            .rendered
            .lock()
            .unwrap()
            .iter()
            .all(|rendered| rendered.tools == ["delay"])
    );
    assert!(
        session
            .current()
            .log()
            .get_entry("recovered-selection")
            .is_some()
    );
    assert!(
        session
            .current()
            .log()
            .find_open_operations(MAIN_LANE, None)
            .unwrap()
            .is_empty()
    );
    // Reopening performs no provider work and commits the already-accepted write only once.
    assert!(domain.requests(1).is_empty());
    host.sessions().shutdown().await.unwrap();
}
