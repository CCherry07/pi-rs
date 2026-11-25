//! A product may request whole-generation refresh without changing session wire records.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pi_agent::AgentOptions;
use pi_core::{ModelId, ProviderId};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSessionOptions, CreateSessionOptions, MultiSessionManager, PreparedSessionGeneration,
    SessionError, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

fn manager(revision: Arc<AtomicUsize>, fail: Arc<AtomicBool>) -> MultiSessionManager {
    MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let revision = revision.clone();
        let fail = fail.clone();
        async move {
            if fail.load(Ordering::SeqCst) {
                return Err(SessionError::Runtime("fixture candidate rejected".into()));
            }
            let baseline = revision.load(Ordering::SeqCst);
            let runtime = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
                    format!("fresh revision {baseline}"),
                )]))
                .agent_options(AgentOptions {
                    provider_id: ProviderId::new("scripted"),
                    model_id: ModelId::new("test"),
                    cwd: request.cwd,
                    ..AgentOptions::default()
                })
                .build()?;
            Ok(PreparedSessionGeneration::new(
                runtime,
                AgentSessionOptions::default().generation_freshness(Arc::new(move || {
                    Ok(revision.load(Ordering::SeqCst) != baseline)
                })),
            ))
        }
    })
}

#[tokio::test]
async fn external_change_publishes_before_provider_submission_without_materializing_a_draft() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.jsonl");
    let revision = Arc::new(AtomicUsize::new(0));
    let manager = manager(revision.clone(), Arc::new(AtomicBool::new(false)));
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    let before = session.current();
    let identity = session.id();
    assert!(!path.exists());
    revision.store(1, Ordering::SeqCst);
    before.prompt("use fresh generation").await.unwrap();
    assert!(!Arc::ptr_eq(&before, &session.current()));
    assert_eq!(session.id(), identity);
    let journal = std::fs::read_to_string(&path).unwrap();
    assert!(journal.contains("fresh revision 1"), "{journal}");
    assert!(!journal.contains("fresh revision 0"));
    assert!(!journal.contains("generation_freshness"));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_freshness_preparation_preserves_old_generation_and_retries_without_a_journal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.jsonl");
    let revision = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let manager = manager(revision.clone(), fail.clone());
    let session = manager
        .create_session(CreateSessionOptions::new(directory.path(), &path))
        .await
        .unwrap();
    let before = session.current();
    revision.store(1, Ordering::SeqCst);
    fail.store(true, Ordering::SeqCst);
    let error = before.prompt("not admitted").await.unwrap_err();
    assert!(error.to_string().contains("fixture candidate rejected"));
    assert!(Arc::ptr_eq(&before, &session.current()));
    assert!(before.is_idle());
    assert!(!path.exists());
    fail.store(false, Ordering::SeqCst);
    before.prompt("retry").await.unwrap();
    assert!(!Arc::ptr_eq(&before, &session.current()));
    let journal = std::fs::read_to_string(&path).unwrap();
    assert!(!journal.contains("not admitted"));
    assert!(journal.contains("fresh revision 1"));
    manager.shutdown().await.unwrap();
}
