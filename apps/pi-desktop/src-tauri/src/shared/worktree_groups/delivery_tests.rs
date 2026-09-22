use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature};
use pi_core::{WorkspaceRoot, WorkspaceRootId, WorkspaceSpec};
use pi_sdk::projects::Project;
use tokio::sync::Mutex;

use super::super::delivery_execution::{
    self, DeliveryAttempt, DeliveryAttemptStatus, DeliveryExecutionRequest,
};
use super::super::{build_record, create, load, remove, save, CheckoutRequest, WorktreeRequest};
use super::*;
use crate::shared::git_targets;
use crate::state::AppState;
use crate::types::{WorkspaceEntry, WorkspaceKind};

struct Fixture {
    _directory: tempfile::TempDir,
    base: PathBuf,
    state: AppState,
    record: Record,
    repositories: Vec<PathBuf>,
    shared: PathBuf,
}

impl Fixture {
    fn workspace(&self) -> &WorkspaceSpec {
        &self.record.plan.workspace
    }

    fn key(&self, member: usize) -> String {
        self.record.members[member]
            .expected_git_dir
            .to_string_lossy()
            .into_owned()
    }

    fn worktree(&self, member: usize) -> &Path {
        &self.record.plan.checkouts[member].destination
    }

    fn overview(&self) -> DeliveryOverview {
        overview(
            &self.state.storage_path,
            &self.record.plan.id,
            self.workspace(),
        )
        .unwrap()
    }

    fn preview(&self, member: usize, branch: &str) -> DeliveryPreview {
        preview(
            &self.state.storage_path,
            &self.record.plan.id,
            self.workspace(),
            &self.key(member),
            branch,
        )
        .unwrap()
    }

    fn execute(&self, request: DeliveryExecutionRequest) -> Result<DeliveryAttempt, String> {
        delivery_execution::execute(
            &self.state.storage_path,
            &self.record.plan.id,
            self.workspace(),
            request,
        )
    }

    fn inspect(&self, attempt_id: &str) -> Result<DeliveryAttempt, String> {
        delivery_execution::inspect(
            &self.state.storage_path,
            &self.record.plan.id,
            self.workspace(),
            attempt_id,
        )
    }

    fn finish(&self, attempt_id: &str) -> Result<DeliveryAttempt, String> {
        delivery_execution::finish(
            &self.state.storage_path,
            &self.record.plan.id,
            self.workspace(),
            attempt_id,
        )
    }
}

fn execution_request(preview: &DeliveryPreview, attempt_id: &str) -> DeliveryExecutionRequest {
    DeliveryExecutionRequest {
        attempt_id: attempt_id.into(),
        checkout_key: preview.checkout_key.clone(),
        target_branch: preview.target.branch.clone(),
        source_oid: preview.source.oid.clone(),
        target_oid: preview.target.oid.clone(),
    }
}

fn preparing_attempt(preview: &DeliveryPreview) -> DeliveryAttempt {
    let now = chrono::Utc::now().to_rfc3339();
    DeliveryAttempt {
        id: uuid::Uuid::new_v4().to_string(),
        checkout_key: preview.checkout_key.clone(),
        source_oid: preview.source.oid.clone(),
        target_oid: preview.target.oid.clone(),
        target_branch: preview.target.branch.clone(),
        result_oid: None,
        status: DeliveryAttemptStatus::Preparing,
        created_at: now.clone(),
        updated_at: now,
        error: None,
    }
}

fn assert_attempt_status(attempt: &DeliveryAttempt, expected: &str) {
    assert_eq!(serde_json::to_value(attempt).unwrap()["status"], expected);
}

fn head_oid(path: &Path) -> Oid {
    Repository::open(path)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
}

fn replace_saved_attempt_status(fixture: &Fixture, status: &str) {
    let mut record = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    let mut attempt = serde_json::to_value(&record.deliveries[0]).unwrap();
    attempt["status"] = status.into();
    record.deliveries[0] = serde_json::from_value(attempt).unwrap();
    save(&fixture.state.storage_path, &record).unwrap();
}

fn simulate_checkout_before_ref_update(fixture: &Fixture) {
    let record = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    let attempt = &record.deliveries[0];
    let target = Oid::from_str(&attempt.target_oid).unwrap();
    Repository::open(&fixture.repositories[0])
        .unwrap()
        .find_reference(&format!("refs/heads/{}", attempt.target_branch))
        .unwrap()
        .set_target(target, "Simulate checkout completed before ref update")
        .unwrap();
    replace_saved_attempt_status(fixture, "applying");
}

fn commit_files(path: &Path, files: &[(&str, &str)], message: &str) -> Oid {
    let repo = Repository::open(path).unwrap();
    let mut index = repo.index().unwrap();
    for (name, content) in files {
        let path = path.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Delivery tests", "delivery@example.invalid").unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

async fn fixture(count: usize) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let repositories: Vec<_> = (0..count)
        .map(|index| {
            let path = base.join(format!("origin-{index}"));
            let repo = Repository::init(&path).unwrap();
            let hooks = repo.path().join("test-hooks");
            fs::create_dir(&hooks).unwrap();
            let attributes = repo.path().join("empty-attributes");
            fs::write(&attributes, "").unwrap();
            let mut config = repo.config().unwrap();
            config
                .set_str("core.hooksPath", hooks.to_str().unwrap())
                .unwrap();
            config
                .set_str("core.attributesFile", attributes.to_str().unwrap())
                .unwrap();
            config.set_str("user.name", "Delivery tests").unwrap();
            config
                .set_str("user.email", "delivery@example.invalid")
                .unwrap();
            config.set_bool("commit.gpgsign", false).unwrap();
            repo.set_head("refs/heads/main").unwrap();
            commit_files(
                &path,
                &[
                    ("tracked.txt", "base\n"),
                    ("src/lib.txt", "source\n"),
                    (".gitignore", "ignored/\n"),
                ],
                &format!("Initial {index}"),
            );
            path
        })
        .collect();
    let shared = base.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("external.txt"), "shared resource").unwrap();
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
        settings: Default::default(),
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
            thread_id: Some("saved-session".into()),
            name: "Delivery group".into(),
            copy_agents_md: false,
            execution_root_id: None,
            checkouts: repositories
                .iter()
                .enumerate()
                .map(|(index, path)| CheckoutRequest {
                    target: git_targets::target_at(path).unwrap(),
                    branch: format!("deliver-{index}"),
                    start_point: "HEAD".into(),
                })
                .collect(),
        },
        source,
        &base.join("managed"),
        None,
    )
    .unwrap();
    save(&state.storage_path, &record).unwrap();
    create(&state, &record.plan.id, record.plan.source.clone())
        .await
        .unwrap();
    let record = load(&state.storage_path, &record.plan.id).unwrap();
    Fixture {
        _directory: directory,
        base,
        state,
        record,
        repositories,
        shared,
    }
}

#[tokio::test]
async fn delivery_selects_one_checkout_without_cross_repository_results() {
    let fixture = fixture(2).await;
    let first = commit_files(
        fixture.worktree(0),
        &[("feature-a.txt", "first")],
        "First feature",
    );
    let second = commit_files(
        fixture.worktree(1),
        &[("feature-b.txt", "second")],
        "Second feature",
    );
    let overview = fixture.overview();
    assert_eq!(overview.checkouts.len(), 2);
    assert!(overview
        .checkouts
        .iter()
        .all(|checkout| checkout.error.is_none()));
    assert_eq!(
        overview.checkouts[0].root_ids,
        vec![
            WorkspaceRootId::new("repo-0"),
            WorkspaceRootId::new("source-subdir")
        ]
    );
    assert_eq!(overview.shared_roots.len(), 1);
    assert_eq!(overview.shared_roots[0].path, fixture.shared);
    assert!(overview
        .checkouts
        .iter()
        .all(|checkout| checkout.default_target_branch.as_deref() == Some("main")));

    for (member, oid, file) in [(0, first, "feature-a.txt"), (1, second, "feature-b.txt")] {
        let preview = fixture.preview(member, "main");
        assert_eq!(preview.checkout_key, fixture.key(member));
        assert_eq!(preview.source_workdir, fixture.worktree(member));
        assert_eq!(preview.target_workdir, fixture.repositories[member]);
        assert_eq!(preview.comparison.ahead, 1);
        assert_eq!(preview.comparison.behind, 0);
        assert_eq!(preview.comparison.commits.len(), 1);
        assert_eq!(preview.comparison.commits[0].oid, oid.to_string());
        assert_eq!(preview.comparison.files.len(), 1);
        assert_eq!(preview.comparison.files[0].path, file);
        assert!(preview.blockers.is_empty());
    }
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        "/foreign/git/key",
        "main"
    )
    .is_err());
}

#[tokio::test]
async fn delivery_computes_commits_relative_to_the_explicit_target_branch() {
    let fixture = fixture(1).await;
    let first = commit_files(fixture.worktree(0), &[("first.txt", "first")], "First");
    let second = commit_files(fixture.worktree(0), &[("second.txt", "second")], "Second");
    let repo = Repository::open(&fixture.repositories[0]).unwrap();
    repo.branch("accepted-first", &repo.find_commit(first).unwrap(), false)
        .unwrap();

    let main = fixture.preview(0, "main");
    assert_eq!(main.comparison.ahead, 2);
    let accepted = fixture.preview(0, "accepted-first");
    assert_eq!(accepted.target.oid, first.to_string());
    assert_eq!(accepted.comparison.ahead, 1);
    assert_eq!(accepted.comparison.commits.len(), 1);
    assert_eq!(accepted.comparison.commits[0].oid, second.to_string());
    assert!(accepted
        .blockers
        .iter()
        .any(|message| message.contains("must be on target branch accepted-first")));
    assert!(fixture.overview().checkouts[0]
        .target_branches
        .iter()
        .any(|branch| branch.name == "accepted-first" && branch.oid == first.to_string()));
}

#[tokio::test]
async fn delivery_warns_for_source_dirt_and_blocks_nonignored_target_changes() {
    let fixture = fixture(1).await;
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "committed")],
        "Feature",
    );
    fs::write(
        fixture.worktree(0).join("tracked.txt"),
        "uncommitted source",
    )
    .unwrap();
    let source_dirty = fixture.preview(0, "main");
    assert!(source_dirty.blockers.is_empty());
    assert!(source_dirty
        .warnings
        .iter()
        .any(|message| message.contains("Uncommitted worktree changes")));
    assert!(source_dirty
        .source_changes
        .iter()
        .any(|change| change.path == "tracked.txt" && change.worktree_status == "M"));
    assert!(source_dirty
        .comparison
        .files
        .iter()
        .all(|file| file.path != "tracked.txt"));

    let original = &fixture.repositories[0];
    fs::write(original.join("tracked.txt"), "target unstaged").unwrap();
    let dirty = fixture.preview(0, "main");
    assert!(dirty
        .blockers
        .iter()
        .any(|message| message.contains("staged, unstaged, or untracked")));
    let repo = Repository::open(original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let staged = fixture.preview(0, "main");
    assert!(staged
        .target_changes
        .iter()
        .any(|change| change.path == "tracked.txt" && change.index_status == "M"));
    assert!(staged
        .blockers
        .iter()
        .any(|message| message.contains("staged, unstaged, or untracked")));
    repo.reset(
        &repo.head().unwrap().peel(git2::ObjectType::Commit).unwrap(),
        git2::ResetType::Hard,
        None,
    )
    .unwrap();
    fs::write(original.join("untracked.txt"), "local target file").unwrap();
    let untracked = fixture.preview(0, "main");
    assert!(untracked
        .target_changes
        .iter()
        .any(|change| change.path == "untracked.txt" && change.worktree_status == "?"));
    assert!(untracked
        .blockers
        .iter()
        .any(|message| message.contains("staged, unstaged, or untracked")));
}

#[tokio::test]
async fn delivery_ignored_target_files_warn_unless_incoming_paths_overlap() {
    let fixture = fixture(1).await;
    let ignored = fixture.repositories[0].join("ignored");
    fs::create_dir(&ignored).unwrap();
    fs::write(ignored.join("cache.txt"), "keep local cache").unwrap();
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "safe incoming")],
        "Feature",
    );
    let safe = fixture.preview(0, "main");
    assert!(safe.blockers.is_empty());
    assert!(safe
        .warnings
        .iter()
        .any(|message| message.contains("Ignored files")));
    commit_files(
        fixture.worktree(0),
        &[("ignored/incoming.txt", "incoming")],
        "Incoming ignored path",
    );
    let overlap = fixture.preview(0, "main");
    assert!(overlap
        .blockers
        .iter()
        .any(|message| message.contains("overlap ignored files")));
    assert_eq!(
        fs::read_to_string(ignored.join("cache.txt")).unwrap(),
        "keep local cache"
    );
}

#[tokio::test]
async fn delivery_blocks_case_variant_ignored_paths_when_git_ignores_case() {
    let fixture = fixture(1).await;
    let origin = &fixture.repositories[0];
    Repository::open(origin)
        .unwrap()
        .config()
        .unwrap()
        .set_bool("core.ignorecase", true)
        .unwrap();
    fs::create_dir(origin.join("ignored")).unwrap();
    fs::write(origin.join("ignored/cache.txt"), "local cache").unwrap();
    commit_files(
        fixture.worktree(0),
        &[("IGNORED/incoming.txt", "incoming")],
        "Case variant incoming path",
    );
    let before = snapshot(&fixture.base);
    let preview = fixture.preview(0, "main");
    assert!(preview
        .blockers
        .iter()
        .any(|message| message.contains("overlap ignored files")));
    assert_eq!(snapshot(&fixture.base), before);
}

#[tokio::test]
async fn delivery_respects_filesystem_case_semantics_when_git_ignores_case_is_false() {
    let fixture = fixture(1).await;
    let origin = &fixture.repositories[0];
    Repository::open(origin)
        .unwrap()
        .config()
        .unwrap()
        .set_bool("core.ignorecase", false)
        .unwrap();
    fs::create_dir(origin.join("ignored")).unwrap();
    fs::write(origin.join("ignored/cache.txt"), "local cache").unwrap();
    commit_files(
        fixture.worktree(0),
        &[("IGNORED/incoming.txt", "incoming")],
        "Case variant incoming path",
    );
    // This is an existing ignored directory, not a filesystem probe created by observation.
    // Case-sensitive volumes must allow the distinct uppercase path; insensitive volumes
    // must detect the alias even though the user manually disabled Git's setting.
    let incoming_ancestor_exists = origin.join("IGNORED").is_dir();
    let before = snapshot(&fixture.base);
    let preview = fixture.preview(0, "main");
    assert_eq!(
        preview
            .blockers
            .iter()
            .any(|message| message.contains("overlap ignored files")),
        incoming_ancestor_exists,
    );
    assert!(!ignored_path_overlaps(
        origin,
        "ignored/",
        "IGNORED-other/file.txt",
        false
    ));
    assert_eq!(snapshot(&fixture.base), before);
}

#[tokio::test]
async fn delivery_reports_in_progress_git_operations_and_locks() {
    let fixture = fixture(1).await;
    let source_git = &fixture.record.members[0].expected_git_dir;
    let origin_git = &fixture.record.members[0].git_dir;
    fs::write(
        source_git.join("MERGE_HEAD"),
        format!("{}\n", fixture.record.plan.checkouts[0].start_oid),
    )
    .unwrap();
    fs::write(origin_git.join("index.lock"), "another git client").unwrap();
    let preview = fixture.preview(0, "main");
    assert!(preview
        .blockers
        .iter()
        .any(|message| message.contains("Worktree has an unfinished Git operation")));
    assert!(preview
        .blockers
        .iter()
        .any(|message| message.contains("Original checkout is locked")));
    assert!(fixture.overview().checkouts[0]
        .warnings
        .iter()
        .any(|message| message.contains("unfinished Git operation")));
    let mut interrupted = fixture.record.clone();
    interrupted.status = GroupStatus::CleanupRequired;
    interrupted.members[0].progress = MemberProgress::Removed;
    save(&fixture.state.storage_path, &interrupted).unwrap();
    assert!(overview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
    )
    .unwrap_err()
    .contains("resolve pending cleanup"));
    assert!(super::preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        &fixture.key(0),
        "main",
    )
    .is_err());
}

#[tokio::test]
async fn delivery_remains_available_after_dirty_worktrees_block_cleanup() {
    let fixture = fixture(2).await;
    fs::write(
        fixture.worktree(1).join("tracked.txt"),
        "unfinished feature",
    )
    .unwrap();
    let failure = remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap_err();
    assert!(failure.contains("changes"));
    let interrupted = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    assert_eq!(interrupted.status, GroupStatus::CleanupRequired);
    assert!(interrupted
        .members
        .iter()
        .all(|member| member.progress == MemberProgress::Created));

    let overview = fixture.overview();
    assert!(overview
        .checkouts
        .iter()
        .all(|checkout| checkout.error.is_none()));
    assert!(overview.checkouts[1]
        .warnings
        .iter()
        .any(|message| message.contains("changes")));
    let preview = fixture.preview(1, "main");
    assert!(preview.blockers.is_empty());
    assert!(preview
        .source_changes
        .iter()
        .any(|change| change.path == "tracked.txt"));
    assert!(preview
        .warnings
        .iter()
        .any(|message| message.contains("Uncommitted worktree changes")));
}

#[tokio::test]
async fn delivery_uses_saved_scope_and_rejects_replaced_checkouts() {
    let fixture = fixture(2).await;
    fixture
        .state
        .project_store()
        .upsert(Project::single_root(
            "parent",
            "Edited parent",
            &fixture.shared,
        ))
        .unwrap();
    fixture.state.workspaces.lock().await.remove("parent");
    assert!(fixture.preview(0, "main").blockers.is_empty());
    let other_scope = WorkspaceSpec::from_cwd(&fixture.shared);
    let excluded = overview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        &other_scope,
    )
    .unwrap();
    assert!(excluded
        .checkouts
        .iter()
        .all(|checkout| checkout.error.is_some()));
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        &other_scope,
        &fixture.key(0),
        "main"
    )
    .is_err());

    let gitfile = fixture.worktree(0).join(".git");
    fs::write(
        &gitfile,
        format!(
            "gitdir: {}\n",
            fixture.record.members[1].expected_git_dir.display()
        ),
    )
    .unwrap();
    let replaced = fixture.overview();
    assert!(replaced.checkouts[0].error.is_some());
    assert!(replaced.checkouts[1].error.is_none());
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        &fixture.key(0),
        "main"
    )
    .is_err());
}

#[tokio::test]
async fn delivery_surfaces_missing_origin_and_missing_target_branch() {
    let fixture = fixture(2).await;
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        &fixture.key(0),
        "missing-target"
    )
    .unwrap_err()
    .contains("Target branch missing-target is unavailable"));
    let origin = &fixture.repositories[0];
    fs::rename(origin, fixture.base.join("moved-origin")).unwrap();
    let overview = fixture.overview();
    assert!(
        overview.checkouts[0].error.is_some()
            || overview.checkouts[0]
                .warnings
                .iter()
                .any(|message| message.contains("unavailable"))
    );
    assert!(overview.checkouts[1].error.is_none());
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        &fixture.key(0),
        "main"
    )
    .is_err());
}

#[derive(Debug, PartialEq, Eq)]
enum FileSnapshot {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn snapshot(path: &Path) -> BTreeMap<PathBuf, FileSnapshot> {
    fn visit(base: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, FileSnapshot>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let relative = path.strip_prefix(base).unwrap().to_path_buf();
        if metadata.is_symlink() {
            entries.insert(
                relative,
                FileSnapshot::Symlink(fs::read_link(path).unwrap()),
            );
        } else if metadata.is_dir() {
            entries.insert(relative, FileSnapshot::Directory);
            for entry in fs::read_dir(path).unwrap() {
                visit(base, &entry.unwrap().path(), entries);
            }
        } else {
            entries.insert(relative, FileSnapshot::File(fs::read(path).unwrap()));
        }
    }
    let mut entries = BTreeMap::new();
    visit(path, path, &mut entries);
    entries
}

#[tokio::test]
async fn delivery_observation_preserves_refs_indexes_workdirs_and_group_metadata() {
    let fixture = fixture(2).await;
    commit_files(
        fixture.worktree(0),
        &[("incoming.txt", "incoming")],
        "Incoming",
    );
    commit_files(
        &fixture.repositories[0],
        &[("target.txt", "target")],
        "Target",
    );
    fs::write(
        fixture.worktree(1).join("tracked.txt"),
        "dirty second checkout",
    )
    .unwrap();
    let before = snapshot(&fixture.base);
    fixture.overview();
    let preview = fixture.preview(0, "main");
    assert_eq!(preview.comparison.ahead, 1);
    assert_eq!(preview.comparison.behind, 1);
    fixture.preview(1, "main");
    assert_eq!(snapshot(&fixture.base), before);
}

#[tokio::test]
async fn delivery_bounds_status_entries_and_exposes_truncation() {
    let fixture = fixture(1).await;
    for index in 0..=CHANGE_LIMIT {
        fs::write(
            fixture.worktree(0).join(format!("source-{index:03}.txt")),
            "source",
        )
        .unwrap();
        fs::write(
            fixture.repositories[0].join(format!("target-{index:03}.txt")),
            "target",
        )
        .unwrap();
    }
    let overview = fixture.overview();
    assert_eq!(overview.checkouts[0].changes.len(), CHANGE_LIMIT);
    assert!(overview.checkouts[0].changes_truncated);
    let preview = fixture.preview(0, "main");
    assert_eq!(preview.source_changes.len(), CHANGE_LIMIT);
    assert_eq!(preview.target_changes.len(), CHANGE_LIMIT);
    assert!(preview.source_changes_truncated);
    assert!(preview.target_changes_truncated);
    assert!(preview
        .blockers
        .iter()
        .any(|message| message.contains("staged, unstaged, or untracked")));
}

#[tokio::test]
async fn execution_fast_forwards_target_and_persists_the_completed_attempt() {
    let fixture = fixture(1).await;
    let source = commit_files(
        fixture.worktree(0),
        &[("delivered.txt", "committed delivery\n")],
        "Deliver feature",
    );
    let preview = fixture.preview(0, "main");
    let source_before = snapshot(fixture.worktree(0));
    let attempt_id = uuid::Uuid::new_v4().to_string();

    let attempt = fixture
        .execute(execution_request(&preview, &attempt_id))
        .unwrap();

    assert_attempt_status(&attempt, "completed");
    assert_eq!(attempt.id, attempt_id);
    assert_eq!(attempt.source_oid, source.to_string());
    assert_eq!(attempt.target_oid, preview.target.oid);
    assert_eq!(attempt.result_oid, Some(source.to_string()));
    assert_eq!(head_oid(&fixture.repositories[0]), source);
    assert_eq!(head_oid(fixture.worktree(0)), source);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert_eq!(
        fs::read_to_string(fixture.repositories[0].join("delivered.txt")).unwrap(),
        "committed delivery\n"
    );
    let target = Repository::open(&fixture.repositories[0]).unwrap();
    assert_eq!(target.head().unwrap().shorthand(), Some("main"));
    assert!(target.statuses(None).unwrap().is_empty());
    let persisted = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    assert_eq!(persisted.version, 2);
    assert_eq!(persisted.deliveries.len(), 1);
    assert_attempt_status(&persisted.deliveries[0], "completed");
    assert_eq!(fixture.overview().attempts[0].id, attempt_id);
}

#[tokio::test]
async fn execution_creates_a_merge_commit_with_target_then_source_parents() {
    let fixture = fixture(1).await;
    let source = commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature side",
    );
    let target = commit_files(
        &fixture.repositories[0],
        &[("target.txt", "target\n")],
        "Target side",
    );
    let preview = fixture.preview(0, "main");
    let source_before = snapshot(fixture.worktree(0));

    let attempt = fixture
        .execute(execution_request(
            &preview,
            &uuid::Uuid::new_v4().to_string(),
        ))
        .unwrap();

    assert_attempt_status(&attempt, "completed");
    let result = Oid::from_str(attempt.result_oid.as_deref().unwrap()).unwrap();
    let repository = Repository::open(&fixture.repositories[0]).unwrap();
    let merged = repository.find_commit(result).unwrap();
    assert_eq!(merged.parent_count(), 2);
    assert_eq!(merged.parent_id(0).unwrap(), target);
    assert_eq!(merged.parent_id(1).unwrap(), source);
    assert_eq!(head_oid(&fixture.repositories[0]), result);
    assert_eq!(head_oid(fixture.worktree(0)), source);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert_eq!(
        fs::read_to_string(fixture.repositories[0].join("feature.txt")).unwrap(),
        "feature\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repositories[0].join("target.txt")).unwrap(),
        "target\n"
    );
    assert!(repository.statuses(None).unwrap().is_empty());
    assert_eq!(repository.state(), git2::RepositoryState::Clean);
}

#[tokio::test]
async fn execution_rejects_stale_source_or_target_without_repository_writes() {
    for advance_source in [true, false] {
        let fixture = fixture(1).await;
        commit_files(
            fixture.worktree(0),
            &[("feature.txt", "feature\n")],
            "Feature",
        );
        let request = execution_request(
            &fixture.preview(0, "main"),
            &uuid::Uuid::new_v4().to_string(),
        );
        let changed = if advance_source {
            fixture.worktree(0)
        } else {
            &fixture.repositories[0]
        };
        commit_files(changed, &[("later.txt", "later\n")], "Moved after preview");
        let target_before = snapshot(&fixture.repositories[0]);
        let source_before = snapshot(fixture.worktree(0));

        assert!(fixture.execute(request).is_err());

        assert_eq!(snapshot(&fixture.repositories[0]), target_before);
        assert_eq!(snapshot(fixture.worktree(0)), source_before);
        assert!(load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .deliveries
            .is_empty());
    }
}

#[tokio::test]
async fn execution_repeated_attempt_does_not_merge_new_source_commits() {
    let fixture = fixture(1).await;
    let first = commit_files(
        fixture.worktree(0),
        &[("first.txt", "first\n")],
        "First delivery",
    );
    let preview = fixture.preview(0, "main");
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let completed = fixture
        .execute(execution_request(&preview, &attempt_id))
        .unwrap();
    assert_attempt_status(&completed, "completed");
    let next = commit_files(
        fixture.worktree(0),
        &[("next.txt", "not part of first delivery\n")],
        "Next source commit",
    );
    let before = snapshot(&fixture.repositories[0]);
    let source_before = snapshot(fixture.worktree(0));

    let repeated = fixture
        .execute(execution_request(&preview, &attempt_id))
        .unwrap();

    assert_attempt_status(&repeated, "completed");
    assert_eq!(repeated.id, completed.id);
    assert_eq!(repeated.result_oid, completed.result_oid);
    assert_eq!(head_oid(&fixture.repositories[0]), first);
    assert_eq!(head_oid(fixture.worktree(0)), next);
    assert_eq!(snapshot(&fixture.repositories[0]), before);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert_eq!(
        load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .deliveries
            .len(),
        1
    );
}

#[tokio::test]
async fn execution_rejects_a_target_dirtied_after_preview_without_repository_writes() {
    let fixture = fixture(1).await;
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let request = execution_request(
        &fixture.preview(0, "main"),
        &uuid::Uuid::new_v4().to_string(),
    );
    fs::write(
        fixture.repositories[0].join("tracked.txt"),
        "user's uncommitted target work\n",
    )
    .unwrap();
    let target_before = snapshot(&fixture.repositories[0]);
    let source_before = snapshot(fixture.worktree(0));

    assert!(fixture.execute(request).is_err());

    assert_eq!(snapshot(&fixture.repositories[0]), target_before);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert!(load(&fixture.state.storage_path, &fixture.record.plan.id)
        .unwrap()
        .deliveries
        .is_empty());
}

#[tokio::test]
async fn execution_changes_only_the_selected_repository_in_a_group() {
    let fixture = fixture(2).await;
    let selected = commit_files(
        fixture.worktree(0),
        &[("selected.txt", "selected delivery\n")],
        "Selected feature",
    );
    let other = commit_files(
        fixture.worktree(1),
        &[("other.txt", "other delivery\n")],
        "Other feature",
    );
    let other_origin_before = snapshot(&fixture.repositories[1]);
    let other_worktree_before = snapshot(fixture.worktree(1));
    let shared_before = snapshot(&fixture.shared);

    let attempt = fixture
        .execute(execution_request(
            &fixture.preview(0, "main"),
            &uuid::Uuid::new_v4().to_string(),
        ))
        .unwrap();

    assert_attempt_status(&attempt, "completed");
    assert_eq!(head_oid(&fixture.repositories[0]), selected);
    assert_eq!(head_oid(fixture.worktree(1)), other);
    assert_eq!(snapshot(&fixture.repositories[1]), other_origin_before);
    assert_eq!(snapshot(fixture.worktree(1)), other_worktree_before);
    assert_eq!(snapshot(&fixture.shared), shared_before);
    assert_eq!(fixture.preview(1, "main").comparison.ahead, 1);
}

#[tokio::test]
async fn unresolved_delivery_blocks_cleanup_even_when_force_is_requested() {
    let fixture = fixture(2).await;
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    fixture
        .execute(execution_request(
            &fixture.preview(0, "main"),
            &uuid::Uuid::new_v4().to_string(),
        ))
        .unwrap();
    // Simulate a process exit after the ref update but before its completion journal write.
    replace_saved_attempt_status(&fixture, "applying");
    let before = snapshot(&fixture.base);

    let error = remove(&fixture.state, &fixture.record.plan.id, true)
        .await
        .unwrap_err();

    assert!(error.to_lowercase().contains("delivery"), "{error}");
    assert_eq!(snapshot(&fixture.base), before);
    assert!(fixture.worktree(0).is_dir());
    assert!(fixture.worktree(1).is_dir());
}

#[tokio::test]
async fn inspection_recovers_a_completed_ref_update_without_replaying_the_merge() {
    let fixture = fixture(1).await;
    let source = commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let completed = fixture
        .execute(execution_request(&fixture.preview(0, "main"), &attempt_id))
        .unwrap();
    replace_saved_attempt_status(&fixture, "applying");
    let next = commit_files(
        fixture.worktree(0),
        &[("next.txt", "next source change\n")],
        "Next feature",
    );
    let target_before = snapshot(&fixture.repositories[0]);
    let source_before = snapshot(fixture.worktree(0));

    let inspected = fixture.inspect(&attempt_id).unwrap();

    assert_attempt_status(&inspected, "completed");
    assert_eq!(inspected.result_oid, completed.result_oid);
    assert_eq!(head_oid(&fixture.repositories[0]), source);
    assert_eq!(head_oid(fixture.worktree(0)), next);
    assert_eq!(snapshot(&fixture.repositories[0]), target_before);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    let persisted = load(&fixture.state.storage_path, &fixture.record.plan.id).unwrap();
    assert_attempt_status(&persisted.deliveries[0], "completed");
}

#[tokio::test]
async fn unfinished_delivery_blocks_detaching_the_project_until_inspected() {
    let fixture = fixture(1).await;
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let attempt_id = uuid::Uuid::new_v4().to_string();
    fixture
        .execute(execution_request(&fixture.preview(0, "main"), &attempt_id))
        .unwrap();
    replace_saved_attempt_status(&fixture, "applying");
    let before = snapshot(&fixture.base);

    assert!(delivery_execution::lock_for_detach(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
    )
    .is_err());

    assert_eq!(snapshot(&fixture.base), before);
    assert!(fixture
        .state
        .workspaces
        .lock()
        .await
        .contains_key(&fixture.record.plan.id));
    let inspected = fixture.inspect(&attempt_id).unwrap();
    assert_attempt_status(&inspected, "completed");
    assert!(delivery_execution::lock_for_detach(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
    )
    .unwrap()
    .is_some());
}

#[tokio::test]
async fn recovery_finishes_only_the_saved_result_after_checkout_preceded_ref_update() {
    let fixture = fixture(1).await;
    let source = commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let original_target = head_oid(&fixture.repositories[0]);
    let attempt_id = uuid::Uuid::new_v4().to_string();
    fixture
        .execute(execution_request(&fixture.preview(0, "main"), &attempt_id))
        .unwrap();
    simulate_checkout_before_ref_update(&fixture);
    let next = commit_files(
        fixture.worktree(0),
        &[("next.txt", "next feature\n")],
        "Source advanced after the saved attempt",
    );
    fs::create_dir(fixture.repositories[0].join("ignored")).unwrap();
    fs::write(
        fixture.repositories[0].join("ignored/cache.txt"),
        "preserve local cache\n",
    )
    .unwrap();
    let before_inspect = snapshot(&fixture.repositories[0]);
    let source_before = snapshot(fixture.worktree(0));

    let inspected = fixture.inspect(&attempt_id).unwrap();

    assert_attempt_status(&inspected, "readyToFinish");
    assert_eq!(head_oid(&fixture.repositories[0]), original_target);
    assert_eq!(snapshot(&fixture.repositories[0]), before_inspect);

    let finished = fixture.finish(&attempt_id).unwrap();

    assert_attempt_status(&finished, "completed");
    assert_eq!(finished.result_oid, Some(source.to_string()));
    assert_eq!(head_oid(&fixture.repositories[0]), source);
    assert_eq!(head_oid(fixture.worktree(0)), next);
    assert!(!fixture.repositories[0].join("next.txt").exists());
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert_eq!(
        fs::read_to_string(fixture.repositories[0].join("ignored/cache.txt")).unwrap(),
        "preserve local cache\n"
    );
    assert!(Repository::open(&fixture.repositories[0])
        .unwrap()
        .statuses(Some(git2::StatusOptions::new().include_ignored(false)))
        .unwrap()
        .is_empty());
    assert_attempt_status(
        &load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .deliveries[0],
        "completed",
    );
    let before_repeat = snapshot(&fixture.repositories[0]);
    assert_attempt_status(&fixture.finish(&attempt_id).unwrap(), "completed");
    assert_eq!(snapshot(&fixture.repositories[0]), before_repeat);
}

#[tokio::test]
async fn recovery_refuses_finalization_when_the_target_was_edited_after_inspection() {
    let fixture = fixture(1).await;
    commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let attempt_id = uuid::Uuid::new_v4().to_string();
    fixture
        .execute(execution_request(&fixture.preview(0, "main"), &attempt_id))
        .unwrap();
    simulate_checkout_before_ref_update(&fixture);
    assert_attempt_status(&fixture.inspect(&attempt_id).unwrap(), "readyToFinish");
    fs::write(
        fixture.repositories[0].join("feature.txt"),
        "operator's later change\n",
    )
    .unwrap();
    let before = snapshot(&fixture.base);

    assert!(fixture.finish(&attempt_id).is_err());

    assert_eq!(snapshot(&fixture.base), before);
    assert_attempt_status(&fixture.inspect(&attempt_id).unwrap(), "needsAttention");
    assert_eq!(
        fs::read_to_string(fixture.repositories[0].join("feature.txt")).unwrap(),
        "operator's later change\n"
    );
}

#[tokio::test]
async fn recovery_of_preparing_with_unchanged_target_does_not_execute_the_saved_intent() {
    let fixture = fixture(1).await;
    let source = commit_files(
        fixture.worktree(0),
        &[("feature.txt", "feature\n")],
        "Feature",
    );
    let preview = fixture.preview(0, "main");
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut record = fixture.record.clone();
    record.version = 2;
    record.deliveries.push(DeliveryAttempt {
        id: attempt_id.clone(),
        checkout_key: preview.checkout_key.clone(),
        source_oid: preview.source.oid.clone(),
        target_oid: preview.target.oid.clone(),
        target_branch: preview.target.branch.clone(),
        result_oid: None,
        status: DeliveryAttemptStatus::Preparing,
        created_at: now.clone(),
        updated_at: now,
        error: None,
    });
    save(&fixture.state.storage_path, &record).unwrap();
    let target_before = snapshot(&fixture.repositories[0]);
    let source_before = snapshot(fixture.worktree(0));

    let inspected = fixture.inspect(&attempt_id).unwrap();

    assert_attempt_status(&inspected, "unchanged");
    assert_eq!(inspected.result_oid, None);
    assert_eq!(snapshot(&fixture.repositories[0]), target_before);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);
    assert_attempt_status(
        &fixture
            .execute(execution_request(&preview, &attempt_id))
            .unwrap(),
        "unchanged",
    );
    assert_eq!(snapshot(&fixture.repositories[0]), target_before);
    assert_eq!(snapshot(fixture.worktree(0)), source_before);

    let new_attempt = fixture
        .execute(execution_request(
            &preview,
            &uuid::Uuid::new_v4().to_string(),
        ))
        .unwrap();
    assert_attempt_status(&new_attempt, "completed");
    assert_eq!(head_oid(&fixture.repositories[0]), source);
}

#[tokio::test]
async fn nested_delivery_blocks_removing_its_origin_and_other_group_merges_until_resolved() {
    let fixture = fixture(1).await;
    commit_files(
        fixture.worktree(0),
        &[("parent-feature.txt", "parent feature\n")],
        "Parent feature",
    );
    let parent_preview = fixture.preview(0, "main");
    let nested = build_record(
        WorktreeRequest {
            parent_id: fixture.record.plan.id.clone(),
            thread_id: None,
            name: "Nested delivery group".into(),
            copy_agents_md: false,
            execution_root_id: None,
            checkouts: vec![CheckoutRequest {
                target: git_targets::target_at(fixture.worktree(0)).unwrap(),
                branch: "nested-delivery".into(),
                start_point: "HEAD".into(),
            }],
        },
        fixture.workspace().clone(),
        &fixture.base.join("managed"),
        None,
    )
    .unwrap();
    save(&fixture.state.storage_path, &nested).unwrap();
    create(&fixture.state, &nested.plan.id, nested.plan.source.clone())
        .await
        .unwrap();
    let mut nested = load(&fixture.state.storage_path, &nested.plan.id).unwrap();
    let nested_workdir = nested.plan.checkouts[0].destination.clone();
    assert_eq!(nested.plan.checkouts[0].source_workdir, fixture.worktree(0));
    assert_eq!(
        nested.members[0].common_dir,
        fixture.record.members[0].common_dir
    );
    commit_files(
        &nested_workdir,
        &[("nested-feature.txt", "nested feature\n")],
        "Nested feature",
    );
    let nested_preview = preview(
        &fixture.state.storage_path,
        &nested.plan.id,
        &nested.plan.workspace,
        &nested.members[0].expected_git_dir.to_string_lossy(),
        &fixture.record.plan.checkouts[0].branch,
    )
    .unwrap();
    assert!(nested_preview.blockers.is_empty());
    nested.version = 2;
    nested.deliveries.push(preparing_attempt(&nested_preview));
    let origin_before = snapshot(&fixture.repositories[0]);
    let parent_before = snapshot(fixture.worktree(0));
    let nested_before = snapshot(&nested_workdir);

    for status in [
        DeliveryAttemptStatus::Preparing,
        DeliveryAttemptStatus::Applying,
    ] {
        nested.deliveries[0].result_oid =
            (status == DeliveryAttemptStatus::Applying).then(|| nested_preview.source.oid.clone());
        nested.deliveries[0].status = status;
        save(&fixture.state.storage_path, &nested).unwrap();
        let saved_nested = serde_json::to_value(&nested).unwrap();
        for force in [false, true] {
            let error = remove(&fixture.state, &fixture.record.plan.id, force)
                .await
                .unwrap_err();
            assert!(error.contains(&nested.deliveries[0].id), "{error}");
            assert!(fixture.worktree(0).is_dir());
            assert_eq!(snapshot(&fixture.repositories[0]), origin_before);
            assert_eq!(snapshot(fixture.worktree(0)), parent_before);
            assert_eq!(snapshot(&nested_workdir), nested_before);
            assert!(fixture
                .state
                .workspaces
                .lock()
                .await
                .contains_key(&fixture.record.plan.id));
            assert_eq!(
                serde_json::to_value(load(&fixture.state.storage_path, &nested.plan.id).unwrap())
                    .unwrap(),
                saved_nested
            );
        }
        let error = fixture
            .execute(execution_request(
                &parent_preview,
                &uuid::Uuid::new_v4().to_string(),
            ))
            .unwrap_err();
        assert!(error.contains(&nested.deliveries[0].id), "{error}");
        assert_eq!(snapshot(&fixture.repositories[0]), origin_before);
        assert_eq!(snapshot(fixture.worktree(0)), parent_before);
        assert!(load(&fixture.state.storage_path, &fixture.record.plan.id)
            .unwrap()
            .deliveries
            .is_empty());
    }

    let inspected = delivery_execution::inspect(
        &fixture.state.storage_path,
        &nested.plan.id,
        &nested.plan.workspace,
        &nested.deliveries[0].id,
    )
    .unwrap();
    assert_attempt_status(&inspected, "unchanged");
    remove(&fixture.state, &fixture.record.plan.id, false)
        .await
        .unwrap();
    assert!(!fixture.worktree(0).exists());
    assert_eq!(snapshot(&nested_workdir), nested_before);
}

#[tokio::test]
async fn preview_rejects_nul_target_branch_without_panicking() {
    let fixture = fixture(1).await;
    let before = snapshot(&fixture.base);
    assert!(preview(
        &fixture.state.storage_path,
        &fixture.record.plan.id,
        fixture.workspace(),
        &fixture.key(0),
        "main\0suffix",
    )
    .is_err());
    assert_eq!(snapshot(&fixture.base), before);
}

#[tokio::test]
async fn execution_rejects_nul_target_branch_without_panicking() {
    let fixture = fixture(1).await;
    let mut request = execution_request(
        &fixture.preview(0, "main"),
        &uuid::Uuid::new_v4().to_string(),
    );
    request.target_branch = "main\0suffix".into();
    let before = snapshot(&fixture.base);
    assert!(fixture.execute(request).is_err());
    assert_eq!(snapshot(&fixture.base), before);
}

#[tokio::test]
async fn journal_validation_rejects_nul_target_branch_without_panicking() {
    let fixture = fixture(1).await;
    let mut record = fixture.record.clone();
    let mut attempt = preparing_attempt(&fixture.preview(0, "main"));
    attempt.target_branch = "main\0suffix".into();
    record.version = 2;
    record.deliveries.push(attempt);
    save(&fixture.state.storage_path, &record).unwrap();
    let before = snapshot(&fixture.base);
    assert!(load(&fixture.state.storage_path, &fixture.record.plan.id).is_err());
    assert_eq!(snapshot(&fixture.base), before);
}
