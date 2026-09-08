use super::*;
use crate::curator::{Config, Curator, metadata::Metadata};
use crate::skills::{SkillCreate, SkillScope};
use std::fs;

fn setup(
    root: &Path,
) -> (
    Arc<HermesMemoryPlugin>,
    Curator,
    crate::skills::SkillDocument,
) {
    let plugin = plugin(
        root,
        HermesMemoryConfig {
            review_enabled: false,
            flush_on_compact: false,
            flush_on_shutdown: false,
            curator: Config {
                enabled: false,
                ..Config::default()
            },
            ..HermesMemoryConfig::default()
        },
    );
    let skill = plugin
        .store
        .create_skill(SkillCreate {
            scope: SkillScope::Global,
            name: "cargo-validation".into(),
            description: "Validate Rust changes".into(),
            body: "Run cargo check.".into(),
        })
        .unwrap();
    let curator = Curator::new(
        plugin.store.global_skill_root(),
        plugin.config.curator.clone(),
    );
    curator.adopt(&skill.name).unwrap();
    (plugin, curator, skill)
}

#[tokio::test]
async fn curator_is_private_bounded_and_rolls_back_receipt_backed_changes() {
    let root = tempfile::tempdir().unwrap();
    let (plugin, curator, skill) = setup(root.path());
    let before = fs::read(&skill.path).unwrap();
    let (session, provider) = session(root.path(), plugin.clone(), vec![
        ScriptedTurn::Text("private parent response".into()),
        call("skill_manage", json!({"action":"patch","name":skill.name,"old_string":"cargo check","new_string":"no fresh read"})),
        call("skill_view", json!({"name":skill.name})),
        call("skill_manage", json!({"action":"patch","name":skill.name,"old_string":"cargo check","new_string":"cargo test"})),
        call("skill_manage", json!({"action":"create","name":"rust-validation","description":"Reusable Rust validation","content":"Run cargo test."})),
        call("write", json!({"path":"forbidden.txt","content":"must not write"})),
        text_with_usage("Completed", Usage { input: 15, output: 4, total_tokens: 19, ..Usage::default() }),
    ]).await;
    session
        .prompt("Parent-only sensitive conversation marker")
        .await
        .unwrap();
    let parent = session.runtime().agent().state().messages;
    session.submit("/curator run --consolidate").await.unwrap();
    assert_eq!(session.runtime().agent().state().messages, parent);
    let requests = provider.requests();
    assert_eq!(requests.len(), 7);
    let private = format!(
        "{}{}",
        requests[1].system_prompt,
        serde_json::to_string(&requests[1].messages).unwrap()
    );
    assert!(!private.contains("Parent-only sensitive"));
    assert!(!private.contains("private parent response"));
    assert_eq!(
        requests[1].tools, requests[0].tools,
        "private execution preserves advertised schemas"
    );
    assert!(
        requests
            .last()
            .unwrap()
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.tool_name == "write" && r.is_error))
    );
    assert!(!root.path().join("forbidden.txt").exists());
    assert!(
        requests[2]
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult(r) if r.is_error))
    );
    assert!(
        fs::read_to_string(&skill.path)
            .unwrap()
            .contains("cargo test")
    );
    let meta = Metadata::read(skill.path.parent().unwrap()).unwrap();
    assert_eq!(meta.patch_count, 1);
    let entries = session.log().load().unwrap().records.into_iter().filter(|record| {
        let pi_session::LaneRecordEntry::Usage(usage) = &record.record else { return false; };
        matches!(&usage.attribution, pi_session::UsageAttribution::Adjustment { details: Some(details), .. }
            if details.get("task") == Some(&json!("curator")))
    }).count();
    assert_eq!(entries, 1);
    let created = curator.root.join("rust-validation");
    assert!(Metadata::read(&created).unwrap().curator_managed);
    curator.rollback(None).unwrap();
    assert_eq!(fs::read(&skill.path).unwrap(), before);
    assert!(!created.exists());
    session.shutdown().await;
}

#[tokio::test]
async fn model_preview_cannot_write_provenance_packages_reports_or_scheduler_state() {
    let root = tempfile::tempdir().unwrap();
    let (plugin, curator, skill) = setup(root.path());
    let (session, provider) = session(
        root.path(),
        plugin,
        vec![
            call("skill_view", json!({"name":skill.name})),
            call(
                "skill_manage",
                json!({"action":"delete","name":skill.name,"absorbed_into":"anything"}),
            ),
            ScriptedTurn::Text("Preview only.".into()),
        ],
    )
    .await;
    let before = tree(&curator.root);
    session
        .submit("/curator run --dry-run --consolidate")
        .await
        .unwrap();
    assert_eq!(tree(&curator.root), before);
    let requests = provider.requests();
    assert!(requests[2].messages.iter().any(
        |m| matches!(m, Message::ToolResult(r) if r.tool_name == "skill_manage" && r.is_error)
    ));
    assert!(session.runtime().agent().state().messages.is_empty());
    assert!(!root.path().join("session.jsonl").exists());
    session.shutdown().await;
}

fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|entry| {
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn curator_rejects_project_access_and_preserves_uncopied_supporting_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let (plugin, curator, skill) = setup(root.path());
    fs::write(skill.path.with_file_name("asset.bin"), [0, 255, 128]).unwrap();
    curator.adopt(&skill.name).unwrap();
    let project = plugin
        .store
        .create_skill(SkillCreate {
            scope: SkillScope::Project,
            name: "project-only".into(),
            description: "Private repository procedure".into(),
            body: "Project-only contents".into(),
        })
        .unwrap();
    let target = plugin
        .store
        .create_skill(SkillCreate {
            scope: SkillScope::Global,
            name: "destination".into(),
            description: "Combined procedure".into(),
            body: "Run cargo check.".into(),
        })
        .unwrap();
    curator.adopt(&target.name).unwrap();
    let (session, provider) = session(root.path(), plugin, vec![
        call("skill_view", json!({"skill_id":project.id})),
        call("skill_manage", json!({"action":"create","scope":"project","name":"forbidden","description":"Forbidden project creation","content":"Do not create"})),
        call("skill_view", json!({"name":skill.name})),
        call("skill_manage", json!({"action":"delete","name":skill.name,"absorbed_into":target.name})),
        ScriptedTurn::Text("No unsafe changes.".into()),
    ]).await;
    session.submit("/curator run --consolidate").await.unwrap();
    let requests = provider.requests();
    let results = &requests.last().unwrap().messages;
    assert_eq!(
        results
            .iter()
            .filter(|m| matches!(m, Message::ToolResult(r) if r.is_error))
            .count(),
        3
    );
    let replay = serde_json::to_string(results).unwrap();
    assert!(!replay.contains("Project-only contents"));
    assert!(replay.contains("supportingFiles"));
    assert_eq!(
        fs::read(skill.path.with_file_name("asset.bin")).unwrap(),
        [0, 255, 128]
    );
    assert!(curator.archives().unwrap().is_empty());
    session.shutdown().await;
}

#[tokio::test]
async fn project_curator_reads_writes_and_rolls_back_only_its_bound_scope() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let (plugin, global, global_skill) = setup(root.path());
    let project_skill = plugin
        .store
        .create_skill(SkillCreate {
            scope: SkillScope::Project,
            name: global_skill.name.clone(),
            description: "Repository validation".into(),
            body: "Project validation step.".into(),
        })
        .unwrap();
    let project = Curator::new(
        project_skill
            .path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .into(),
        Config::default(),
    );
    project.adopt(&project_skill.name).unwrap();
    let global_before = tree(&global.root);
    let original = fs::read(&project_skill.path).unwrap();
    let (session, provider) = session(root.path(), plugin, vec![
        call("skill_view", json!({"name":project_skill.name})),
        ScriptedTurn::Text("Preview project improvements.".into()),
        call("skills_list", json!({})),
        call("skill_view", json!({"name":global_skill.name})),
        call("skill_view", json!({"skill_id":global_skill.id})),
        call("skill_manage", json!({"action":"create","scope":"global","name":"forbidden","description":"Forbidden global creation","content":"Do not create."})),
        call("skill_manage", json!({"action":"patch","skill_id":project_skill.id,"old_string":"Project validation step.","new_string":"Improved project validation step."})),
        call("skill_manage", json!({"action":"delete","skill_id":project_skill.id,"absorbed_into":global_skill.id})),
        call("skill_manage", json!({"action":"create","name":"project-umbrella","description":"Repository workflow umbrella","content":"Project workflow."})),
        ScriptedTurn::Text("Project updated.".into()),
    ]).await;
    // Session start/foreground bookkeeping may touch the global scheduler, but a
    // project-only model command must leave every byte of the global tree intact.
    let global_before_run = tree(&global.root);
    let project_before_preview = tree(&project.root);
    session
        .submit("/curator run --scope project --dry-run --consolidate")
        .await
        .unwrap();
    assert_eq!(tree(&project.root), project_before_preview);
    assert_eq!(tree(&global.root), global_before_run);
    session
        .submit("/curator run --scope project --consolidate")
        .await
        .unwrap();
    assert_eq!(tree(&global.root), global_before_run);
    assert_eq!(
        fs::read(&global_skill.path).unwrap(),
        global_before[Path::new("cargo-validation/SKILL.md")]
    );
    let messages = &provider.requests().last().unwrap().messages.clone();
    let results = messages
        .iter()
        .filter_map(|m| {
            if let Message::ToolResult(r) = m {
                Some(r)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_error).count(), 3);
    let index = results[0].details.as_ref().unwrap();
    assert_eq!(index["skills"].as_array().unwrap().len(), 1);
    assert_eq!(index["skills"][0]["skillId"], project_skill.id);
    assert!(
        fs::read_to_string(&project_skill.path)
            .unwrap()
            .contains("Improved project")
    );
    let created = project.root.join("project-umbrella");
    assert!(Metadata::read(&created).unwrap().curator_managed);
    assert!(!global.root.join("project-umbrella").exists());
    session
        .submit("/curator rollback --scope project")
        .await
        .unwrap();
    assert_eq!(fs::read(&project_skill.path).unwrap(), original);
    assert!(!created.exists());
    assert_eq!(tree(&global.root), global_before_run);
    session.shutdown().await;
}

#[tokio::test]
async fn project_consolidation_resolves_the_destination_in_its_own_scope() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let (plugin, global, global_destination) = setup(root.path());
    let mut documents = Vec::new();
    for name in ["source", global_destination.name.as_str()] {
        documents.push(
            plugin
                .store
                .create_skill(SkillCreate {
                    scope: SkillScope::Project,
                    name: name.into(),
                    description: "Project procedure".into(),
                    body: "Verified source procedure, already incorporated in the destination."
                        .into(),
                })
                .unwrap(),
        );
    }
    let source = &documents[0];
    let destination = &documents[1];
    let project = Curator::new(
        source.path.parent().unwrap().parent().unwrap().into(),
        Config::default(),
    );
    for document in &documents {
        fs::write(document.path.with_file_name("asset.bin"), [0, 128, 255]).unwrap();
        project.adopt(&document.name).unwrap();
    }
    let (session, _) = session(
        root.path(),
        plugin,
        vec![
            call("skill_view", json!({"skill_id":source.id})),
            call(
                "skill_manage",
                json!({"action":"delete","skill_id":source.id,"absorbed_into":destination.name}),
            ),
            ScriptedTurn::Text("Project source consolidated.".into()),
        ],
    )
    .await;
    let global_before = tree(&global.root);
    session
        .submit("/curator run --scope project --consolidate")
        .await
        .unwrap();
    assert!(!source.path.exists());
    assert!(destination.path.exists());
    assert_eq!(project.archives().unwrap().len(), 1);
    assert_eq!(tree(&global.root), global_before);
    session
        .submit("/curator rollback --scope project")
        .await
        .unwrap();
    assert_eq!(
        fs::read(source.path.with_file_name("asset.bin")).unwrap(),
        [0, 128, 255]
    );
    assert_eq!(tree(&global.root), global_before);
    session.shutdown().await;
}

#[tokio::test]
async fn automatic_worker_maintains_project_when_global_is_paused() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    let plugin = plugin(
        root.path(),
        HermesMemoryConfig {
            review_enabled: false,
            ..HermesMemoryConfig::default()
        },
    );
    let skill = plugin
        .store
        .create_skill(SkillCreate {
            scope: SkillScope::Project,
            name: "old-project-workflow".into(),
            description: "Repository workflow".into(),
            body: "A verified repository procedure.".into(),
        })
        .unwrap();
    let project = Curator::new(
        skill.path.parent().unwrap().parent().unwrap().into(),
        Config::default(),
    );
    Metadata::new(skill.path.parent().unwrap(), true, "agent", 0)
        .unwrap()
        .save(skill.path.parent().unwrap())
        .unwrap();
    project
        .update_state(|state| {
            state.last_run_at = Some(0);
            state.last_activity_at = Some(0);
        })
        .unwrap();
    let global = Curator::new(plugin.store.global_skill_root(), Config::default());
    global.update_state(|state| state.paused = true).unwrap();
    let (session, provider) = session(root.path(), plugin, vec![]).await;
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while project.state().unwrap().run_count == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("project worker should run at its first tick");
    assert!(!skill.path.exists());
    assert_eq!(project.archives().unwrap().len(), 1);
    assert_eq!(global.state().unwrap().run_count, 0);
    assert!(provider.requests().is_empty());
    session.shutdown().await;
}

#[tokio::test]
async fn normal_read_activity_is_observed_without_transferring_ownership() {
    let root = tempfile::tempdir().unwrap();
    let (plugin, _, skill) = setup(root.path());
    let directory = skill.path.parent().unwrap();
    let mut meta = Metadata::read(directory).unwrap();
    meta.curator_managed = false;
    meta.save(directory).unwrap();
    let (session, _) = session(
        root.path(),
        plugin,
        vec![
            call("read", json!({"path":skill.path})),
            ScriptedTurn::Text("Read".into()),
        ],
    )
    .await;
    session.prompt("Read this procedure").await.unwrap();
    let meta = Metadata::read(directory).unwrap();
    assert_eq!(meta.view_count, 1);
    assert!(!meta.curator_managed);
    session.shutdown().await;
}

#[tokio::test]
async fn curator_stops_at_the_iteration_budget() {
    let root = tempfile::tempdir().unwrap();
    let (plugin, curator, skill) = setup(root.path());
    let turns = (0..12)
        .map(|_| call("skill_view", json!({"name":skill.name})))
        .collect();
    let (session, provider) = session(root.path(), plugin, turns).await;
    session.submit("/curator run --consolidate").await.unwrap();
    assert_eq!(provider.requests().len(), 8);
    assert_eq!(curator.state().unwrap().run_count, 1);
    assert!(session.runtime().agent().state().messages.is_empty());
    session.shutdown().await;
}

#[tokio::test]
async fn empty_library_never_invokes_the_provider() {
    let root = tempfile::tempdir().unwrap();
    let (plugin, curator, skill) = setup(root.path());
    curator.archive(&skill.name).unwrap();
    let (session, provider) = session(root.path(), plugin, vec![]).await;
    session.submit("/curator run --consolidate").await.unwrap();
    assert!(provider.requests().is_empty());
    assert_eq!(curator.state().unwrap().run_count, 1);
    session.shutdown().await;
}
