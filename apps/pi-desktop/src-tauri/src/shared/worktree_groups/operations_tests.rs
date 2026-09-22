use std::collections::HashMap;
use std::path::PathBuf;

use git2::{Repository, Signature};
use pi_core::{WorkspaceRoot, WorkspaceRootId};
use tokio::sync::Mutex;

use super::*;
use crate::shared::git_targets;
use crate::shared::worktree_groups::{build_record, CheckoutRequest, WorktreeRequest};

struct Fixture {
    _directory: tempfile::TempDir,
    state: AppState,
    record: Record,
    repositories: Vec<PathBuf>,
    shared: PathBuf,
}

fn initialize(path: &Path) -> PathBuf {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("tracked.txt"), "base\n").unwrap();
    fs::write(path.join("src/lib.txt"), "source\n").unwrap();
    fs::write(path.join(".gitignore"), "ignored/\n").unwrap();
    let repo = Repository::init(path).unwrap();
    let mut index = repo.index().unwrap();
    for name in ["tracked.txt", "src/lib.txt", ".gitignore"] {
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let signature = Signature::now("Worktree tests", "worktree@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
        .unwrap();
    path.canonicalize().unwrap()
}

fn fixture(count: usize) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let repositories: Vec<_> = (0..count)
        .map(|index| initialize(&base.join(format!("source-{index}"))))
        .collect();
    let shared = base.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("external.txt"), "preserve shared root").unwrap();
    let mut roots: Vec<_> = repositories
        .iter()
        .enumerate()
        .map(|(index, path)| {
            WorkspaceRoot::external(format!("repo-{index}"), format!("Repo {index}"), path)
        })
        .collect();
    roots.push(WorkspaceRoot::external(
        "source-subdir",
        "Source",
        repositories[0].join("src"),
    ));
    roots.push(WorkspaceRoot::external("shared", "Shared", &shared));
    let source =
        WorkspaceSpec::new(roots, WorkspaceRootId::new("repo-0"), &repositories[0]).unwrap();
    let parent = WorkspaceEntry {
        id: "parent".into(),
        name: "Parent".into(),
        path: repositories[0].to_string_lossy().into_owned(),
        kind: WorkspaceKind::Main,
        parent_id: None,
        worktree: None,
        settings: WorkspaceSettings::default(),
    };
    let state = AppState {
        workspaces: Mutex::new(HashMap::from([(parent.id.clone(), parent)])),
        terminal_sessions: Default::default(),
        storage_path: base.join("workspaces.json"),
        settings_path: base.join("settings.json"),
        app_settings: Default::default(),
        dictation: Mutex::new(crate::dictation::DictationState::default()),
    };
    state
        .project_store()
        .upsert(Project::from_workspace("parent", "Parent", &source).unwrap())
        .unwrap();
    let record = build_record(
        WorktreeRequest {
            parent_id: "parent".into(),
            thread_id: None,
            name: "Managed work".into(),
            copy_agents_md: false,
            execution_root_id: None,
            checkouts: repositories
                .iter()
                .enumerate()
                .map(|(index, path)| CheckoutRequest {
                    target: git_targets::target_at(path).unwrap(),
                    branch: format!("managed-{index}"),
                    start_point: "HEAD".into(),
                })
                .collect(),
        },
        source,
        &base.join("managed"),
        Some("setup snapshot".into()),
    )
    .unwrap();
    save(&state.storage_path, &record).unwrap();
    Fixture {
        _directory: directory,
        state,
        record,
        repositories,
        shared,
    }
}

async fn create_fixture(fixture: &Fixture) -> WorkspaceInfo {
    create(
        &fixture.state,
        &fixture.record.plan.id,
        fixture.record.plan.source.clone(),
    )
    .await
    .unwrap()
}

fn assert_branches_retained(fixture: &Fixture, count: usize) {
    for (index, path) in fixture.repositories.iter().take(count).enumerate() {
        Repository::open(path)
            .unwrap()
            .find_reference(&format!("refs/heads/managed-{index}"))
            .unwrap();
    }
}

#[tokio::test]
async fn creates_all_mapped_roots_and_removes_without_parent_or_external_root_deletion() {
    let fixture = fixture(2);
    let info = create_fixture(&fixture).await;
    let spec = info.project.unwrap().resolve().unwrap();
    assert_eq!(spec, fixture.record.plan.workspace);
    assert_eq!(
        spec.roots()
            .iter()
            .find(|root| root.id.as_str() == "source-subdir")
            .unwrap()
            .path,
        fixture.record.plan.checkouts[0].destination.join("src")
    );
    assert_eq!(
        info.settings.worktree_setup_script.as_deref(),
        Some("setup snapshot")
    );
    fixture.state.workspaces.lock().await.remove("parent");
    fixture.state.project_store().remove("parent").unwrap();

    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
    assert!(fixture
        .record
        .plan
        .checkouts
        .iter()
        .all(|checkout| !checkout.destination.exists()));
    assert_eq!(
        fs::read_to_string(fixture.shared.join("external.txt")).unwrap(),
        "preserve shared root"
    );
    assert!(!fixture.record.group_dir.exists());
    assert!(
        !record_path(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .exists()
    );
    assert!(!fixture
        .state
        .workspaces
        .lock()
        .await
        .contains_key(&fixture.record.plan.id));
    assert!(fixture
        .state
        .project_store()
        .get(&fixture.record.plan.id)
        .is_err());
    assert_branches_retained(&fixture, 2);
}

#[tokio::test]
async fn all_members_are_preserved_for_tracked_untracked_and_ignored_changes() {
    let fixture = fixture(2);
    create_fixture(&fixture).await;
    let second = &fixture.record.plan.checkouts[1].destination;
    for name in ["tracked.txt", "untracked.txt", "ignored/cache"] {
        let changed = second.join(name);
        fs::create_dir_all(changed.parent().unwrap()).unwrap();
        fs::write(&changed, "local data").unwrap();
        assert!(remove(&fixture.state, &fixture.record.plan.id, false)
            .await
            .unwrap_err()
            .contains("changes"));
        assert!(fixture
            .record
            .plan
            .checkouts
            .iter()
            .all(|checkout| checkout.destination.exists()));
        assert!(load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .members
            .iter()
            .all(|member| member.progress == MemberProgress::Created));
        if name == "tracked.txt" {
            fs::write(changed, "base\n").unwrap();
        } else if name == "untracked.txt" {
            fs::remove_file(changed).unwrap();
        }
    }
    remove(&fixture.state, &fixture.record.plan.id, true)
        .await
        .unwrap();
    assert_branches_retained(&fixture, 2);
}

#[tokio::test]
async fn explicit_discard_never_bypasses_locks_or_changed_identity() {
    let fixture = fixture(2);
    create_fixture(&fixture).await;
    let locked = fixture.record.members[1].expected_git_dir.join("locked");
    fs::write(&locked, "user lock").unwrap();
    assert!(remove(&fixture.state, &fixture.record.plan.id, true)
        .await
        .unwrap_err()
        .contains("locked"));
    assert!(fixture
        .record
        .plan
        .checkouts
        .iter()
        .all(|checkout| checkout.destination.exists()));
    fs::remove_file(locked).unwrap();

    let gitfile = fixture.record.plan.checkouts[1].destination.join(".git");
    let original = fs::read(&gitfile).unwrap();
    fs::write(
        &gitfile,
        format!("gitdir: {}\n", fixture.record.members[1].git_dir.display()),
    )
    .unwrap();
    assert!(remove(&fixture.state, &fixture.record.plan.id, true)
        .await
        .unwrap_err()
        .contains("identity changed"));
    assert!(fixture
        .record
        .plan
        .checkouts
        .iter()
        .all(|checkout| checkout.destination.exists()));
    fs::write(gitfile, original).unwrap();
    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn later_git_failure_cleans_created_members_and_retains_branches_and_journal() {
    let fixture = fixture(2);
    let branch_lock = fixture.record.members[1]
        .common_dir
        .join("refs/heads/managed-1.lock");
    fs::write(&branch_lock, "another Git process").unwrap();
    let error = create(
        &fixture.state,
        &fixture.record.plan.id,
        fixture.record.plan.source.clone(),
    )
    .await
    .unwrap_err();
    assert!(error.contains("Created branches were retained"));
    assert!(fixture
        .record
        .plan
        .checkouts
        .iter()
        .all(|checkout| !checkout.destination.exists()));
    let saved = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    assert_eq!(saved.status, GroupStatus::CleanupRequired);
    assert!(saved
        .members
        .iter()
        .all(|member| member.progress == MemberProgress::Removed));
    assert!(!fixture
        .state
        .workspaces
        .lock()
        .await
        .contains_key(&fixture.record.plan.id));
    assert_branches_retained(&fixture, 1);
    fs::remove_file(branch_lock).unwrap();
    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn reconciles_git_add_that_succeeded_before_creation_progress_was_saved() {
    let fixture = fixture(1);
    let mut interrupted = fixture.record.clone();
    fs::create_dir_all(&interrupted.group_dir).unwrap();
    interrupted.status = GroupStatus::Creating;
    interrupted.members[0].progress = MemberProgress::Creating;
    save(&fixture.state.storage_path, &interrupted).unwrap();
    let checkout = &interrupted.plan.checkouts[0];
    git_core::run_git_command(
        &checkout.source_workdir,
        &[
            "worktree",
            "add",
            "-b",
            &checkout.branch,
            path_argument(&checkout.destination).unwrap(),
            &checkout.start_oid,
        ],
    )
    .await
    .unwrap();

    remove(&fixture.state, &interrupted.plan.id, false)
        .await
        .unwrap();
    assert!(!checkout.destination.exists());
    assert_branches_retained(&fixture, 1);
}

#[tokio::test]
async fn retries_partial_cleanup_without_touching_reused_removed_member_paths() {
    let fixture = fixture(2);
    create_fixture(&fixture).await;
    let mut interrupted = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    let removed = &interrupted.plan.checkouts[1].destination;
    git_core::run_git_command(
        &interrupted.members[1].common_dir,
        &[
            "--git-dir",
            path_argument(&interrupted.members[1].common_dir).unwrap(),
            "worktree",
            "remove",
            path_argument(removed).unwrap(),
        ],
    )
    .await
    .unwrap();
    interrupted.members[1].progress = MemberProgress::Removed;
    interrupted.status = GroupStatus::Removing;
    save(&fixture.state.storage_path, &interrupted).unwrap();
    fs::create_dir(removed).unwrap();
    fs::write(removed.join("external.txt"), "new owner").unwrap();

    remove(&fixture.state, &interrupted.plan.id, false)
        .await
        .unwrap();
    assert!(!interrupted.plan.checkouts[0].destination.exists());
    assert_eq!(
        fs::read_to_string(removed.join("external.txt")).unwrap(),
        "new owner"
    );
    assert!(
        !record_path(&fixture.state.storage_path, &interrupted.plan.id)
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn workspace_write_failure_cleans_worktrees_before_retrying_metadata_cleanup() {
    let fixture = fixture(1);
    fs::create_dir(&fixture.state.storage_path).unwrap();
    assert!(create(
        &fixture.state,
        &fixture.record.plan.id,
        fixture.record.plan.source.clone()
    )
    .await
    .is_err());
    assert!(!fixture.record.plan.checkouts[0].destination.exists());
    assert_eq!(
        load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .members[0]
            .progress,
        MemberProgress::Removed
    );
    assert!(!fixture
        .state
        .workspaces
        .lock()
        .await
        .contains_key(&fixture.record.plan.id));
    // A failed workspace-list write must not remove the Project that a durable UI
    // entry may still reference. The journal retains it until publication can finish.
    assert!(fixture
        .state
        .project_store()
        .get(&fixture.record.plan.id)
        .is_ok());
    fs::remove_dir(&fixture.state.storage_path).unwrap();
    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
    assert_branches_retained(&fixture, 1);
}

#[tokio::test]
async fn failed_workspace_unpublication_preserves_the_existing_project() {
    let fixture = fixture(1);
    create_fixture(&fixture).await;
    let previous = fs::read(&fixture.state.storage_path).unwrap();
    fs::remove_file(&fixture.state.storage_path).unwrap();
    fs::create_dir(&fixture.state.storage_path).unwrap();
    fs::write(fixture.state.storage_path.join("previous.json"), &previous).unwrap();

    assert!(unpublish(&fixture.state, &fixture.record).await.is_err());
    assert!(fixture
        .state
        .project_store()
        .get(&fixture.record.plan.id)
        .is_ok());
    assert!(fixture
        .state
        .workspaces
        .lock()
        .await
        .contains_key(&fixture.record.plan.id));
    assert_eq!(
        fs::read(fixture.state.storage_path.join("previous.json")).unwrap(),
        previous
    );

    fs::remove_file(fixture.state.storage_path.join("previous.json")).unwrap();
    fs::remove_dir(&fixture.state.storage_path).unwrap();
    fs::write(&fixture.state.storage_path, previous).unwrap();
    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_uses_common_repository_after_the_source_checkout_is_removed() {
    let mut fixture = fixture(1);
    let linked = fixture.repositories[0]
        .parent()
        .unwrap()
        .join("linked-source");
    git_core::run_git_command(
        &fixture.repositories[0],
        &[
            "worktree",
            "add",
            "-b",
            "linked-source",
            path_argument(&linked).unwrap(),
            "HEAD",
        ],
    )
    .await
    .unwrap();
    let source = WorkspaceSpec::from_cwd(&linked);
    fixture.record = build_record(
        WorktreeRequest {
            parent_id: "parent".into(),
            thread_id: None,
            name: "Linked source".into(),
            copy_agents_md: false,
            execution_root_id: None,
            checkouts: vec![CheckoutRequest {
                target: git_targets::target_at(&linked).unwrap(),
                branch: "from-linked-source".into(),
                start_point: "HEAD".into(),
            }],
        },
        source,
        fixture.record.group_dir.parent().unwrap(),
        None,
    )
    .unwrap();
    save(&fixture.state.storage_path, &fixture.record).unwrap();
    create_fixture(&fixture).await;
    git_core::run_git_command(
        &fixture.repositories[0],
        &["worktree", "remove", path_argument(&linked).unwrap()],
    )
    .await
    .unwrap();

    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
    assert!(!fixture.record.plan.checkouts[0].destination.exists());
    Repository::open(&fixture.repositories[0])
        .unwrap()
        .find_reference("refs/heads/from-linked-source")
        .unwrap();
}

#[tokio::test]
async fn copied_context_survives_automatic_rollback_until_explicit_discard() {
    let mut fixture = fixture(1);
    fs::write(
        fixture.repositories[0].join("AGENTS.md"),
        "Local project instructions",
    )
    .unwrap();
    fixture.record.copy_agents_md = true;
    save(&fixture.state.storage_path, &fixture.record).unwrap();
    fs::create_dir(&fixture.state.storage_path).unwrap();

    assert!(create(
        &fixture.state,
        &fixture.record.plan.id,
        fixture.record.plan.source.clone()
    )
    .await
    .is_err());
    let copied = fixture.record.plan.checkouts[0]
        .destination
        .join("AGENTS.md");
    assert_eq!(
        fs::read_to_string(&copied).unwrap(),
        "Local project instructions"
    );
    assert_eq!(
        load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .members[0]
            .progress,
        MemberProgress::Created
    );
    fs::remove_dir(&fixture.state.storage_path).unwrap();
    assert!(remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .is_err());
    assert!(copied.exists());
    remove(&fixture.state, &fixture.record.plan.id, true)
        .await
        .unwrap();
}
