use super::*;
use pi_core::WorkspaceRoot;

#[cfg(unix)]
#[test]
fn dropping_group_lock_releases_it_with_an_inherited_descriptor_open() {
    let temp = tempfile::tempdir().unwrap();
    let storage = temp.path().join("workspaces.json");
    let id = Uuid::new_v4().to_string();
    let held = lock(&storage, &id).unwrap();
    // A forked child shares this open file description until exec closes the inherited FD.
    let inherited = held.0.try_clone().unwrap();
    assert!(lock(&storage, &id).is_err());
    drop(held);

    let next = lock(&storage, &id).expect("The operation guard must release its lock on drop");
    drop(inherited);
    assert!(lock(&storage, &id).is_err());
    drop(next);
    assert!(lock(&storage, &id).is_ok());
}

fn repo(path: &Path) -> Repository {
    let repo = Repository::init(path).unwrap();
    fs::create_dir_all(path.join("src/nested")).unwrap();
    fs::write(path.join("src/nested/file.txt"), "initial").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

fn spec(paths: &[&Path], cwd: &Path) -> WorkspaceSpec {
    WorkspaceSpec::new(
        paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                WorkspaceRoot::external(index.to_string(), format!("Root {index}"), *path)
            })
            .collect(),
        WorkspaceRootId::new("0"),
        cwd,
    )
    .unwrap()
}

fn request(paths: &[&Path]) -> WorktreeRequest {
    WorktreeRequest {
        parent_id: "parent".into(),
        thread_id: Some("thread".into()),
        name: "Feature".into(),
        copy_agents_md: false,
        execution_root_id: None,
        checkouts: paths
            .iter()
            .enumerate()
            .map(|(index, path)| CheckoutRequest {
                target: git_targets::target_at(path).unwrap(),
                branch: format!("feature/{index}"),
                start_point: "HEAD".into(),
            })
            .collect(),
    }
}

#[test]
fn preview_maps_checkout_offsets_and_shared_roots_without_creating_worktrees() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let shared = temp.path().join("shared");
    let a_repo = repo(&a);
    repo(&b);
    fs::create_dir(&shared).unwrap();
    let source = spec(&[&a, &a.join("src"), &b, &shared], &a.join("src/nested"));
    let record = build_record(
        request(&[&a, &b]),
        source.clone(),
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    assert!(!record.group_dir.exists());
    assert!(!temp.path().join("storage").exists());
    assert!(!a_repo.path().join("worktrees").exists());
    assert!(a_repo
        .find_branch("feature/0", git2::BranchType::Local)
        .is_err());
    assert_eq!(
        record.plan.workspace.roots()[1].path,
        record.plan.checkouts[0].destination.join("src")
    );
    assert_eq!(
        record.plan.workspace.cwd(),
        record.plan.checkouts[0].destination.join("src/nested")
    );
    assert_eq!(
        record.plan.workspace.roots()[3].path,
        shared.canonicalize().unwrap()
    );
    assert_eq!(record.plan.checkouts[0].root_ids.len(), 2);
    verify_source(&record, &source).unwrap();
}

#[test]
fn linked_worktrees_require_distinct_branches_and_keep_distinct_members() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let a_repo = repo(&a);
    let linked = temp.path().join("linked");
    a_repo.worktree("linked", &linked, None).unwrap();
    let source = spec(&[&a, &linked], &a);
    let mut req = request(&[&a, &linked]);
    req.checkouts[1].branch = req.checkouts[0].branch.clone();
    assert!(
        build_record(req, source.clone(), &temp.path().join("storage"), None)
            .unwrap_err()
            .contains("different new branches")
    );
    let record = build_record(
        request(&[&a, &linked]),
        source,
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    assert_eq!(record.members.len(), 2);
    assert_eq!(record.members[0].common_dir, record.members[1].common_dir);
    assert_ne!(record.members[0].git_dir, record.members[1].git_dir);
}

#[test]
fn nested_checkouts_require_an_explicit_root_and_do_not_capture_container_roots() {
    let temp = tempfile::tempdir().unwrap();
    let container = temp.path().join("container");
    let nested = container.join("nested");
    repo(&nested);
    let source = spec(&[&container], &container);
    assert!(build_record(
        request(&[&nested]),
        source,
        &temp.path().join("storage"),
        None
    )
    .unwrap_err()
    .contains("added as a project root"));
    let source = spec(&[&container, &nested], &container);
    let mut req = request(&[&nested]);
    assert!(build_record(
        request(&[&nested]),
        source.clone(),
        &temp.path().join("storage"),
        None
    )
    .unwrap_err()
    .contains("select an execution root"));
    req.execution_root_id = Some(WorkspaceRootId::new("1"));
    let record = build_record(req, source, &temp.path().join("storage"), None).unwrap();
    assert_eq!(
        record.plan.checkouts[0].root_ids,
        vec![WorkspaceRootId::new("1")]
    );
    assert_eq!(
        record.plan.workspace.roots()[0].path,
        container.canonicalize().unwrap()
    );
}

#[test]
fn pinned_commit_survives_head_movement_but_workspace_and_branch_changes_expire_preview() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let a_repo = repo(&a);
    let source = spec(&[&a], &a);
    let record = build_record(
        request(&[&a]),
        source.clone(),
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    let old = a_repo.head().unwrap().peel_to_commit().unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    a_repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "new head",
            &old.tree().unwrap(),
            &[&old],
        )
        .unwrap();
    verify_source(&record, &source).unwrap();
    assert_eq!(record.plan.checkouts[0].start_oid, old.id().to_string());
    let changed = spec(&[&a, &a.join("src")], &a);
    assert!(verify_source(&record, &changed)
        .unwrap_err()
        .contains("workspace changed"));
    a_repo.branch("feature/0", &old, false).unwrap();
    assert!(verify_source(&record, &source)
        .unwrap_err()
        .contains("already exists"));
}

#[test]
fn refuses_roots_missing_from_commit_and_changed_nested_membership() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    repo(&a);
    let untracked = a.join("untracked");
    fs::create_dir(&untracked).unwrap();
    let source = spec(&[&untracked], &untracked);
    assert!(
        build_record(request(&[&a]), source, &temp.path().join("storage"), None)
            .unwrap_err()
            .contains("absent from")
    );
    let source = spec(&[&a], &a.join("src"));
    let record = build_record(
        request(&[&a]),
        source.clone(),
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    repo(&a.join("src"));
    assert!(verify_source(&record, &source)
        .unwrap_err()
        .contains("Git membership changed"));
}

#[test]
fn prepared_records_survive_restart_are_locked_and_discard_only_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    repo(&a);
    let source = spec(&[&a], &a);
    let mut record =
        build_record(request(&[&a]), source, &temp.path().join("storage"), None).unwrap();
    let storage = temp.path().join("app/workspaces.json");
    save(&storage, &record).unwrap();
    let id = &record.plan.id;
    assert_eq!(
        source_context(&storage, id).unwrap(),
        ("parent".into(), Some("thread".into()))
    );
    assert_eq!(list(&storage).unwrap()[0].member_count, 1);
    let held = lock(&storage, id).unwrap();
    assert!(discard(&storage, id).unwrap_err().contains("in progress"));
    drop(held);
    record.status = GroupStatus::Creating;
    save(&storage, &record).unwrap();
    assert!(discard(&storage, id)
        .unwrap_err()
        .contains("already started"));
    record.status = GroupStatus::Prepared;
    save(&storage, &record).unwrap();
    discard(&storage, id).unwrap();
    assert!(a.join("src/nested/file.txt").exists());
    assert!(list(&storage).unwrap().is_empty());
}

#[test]
fn rejects_corrupted_records_and_path_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    repo(&a);
    let storage = temp.path().join("app/workspaces.json");
    assert!(record_path(&storage, "../outside").is_err());
    let mut record = build_record(
        request(&[&a]),
        spec(&[&a], &a),
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    record.plan.checkouts[0].destination = a.clone();
    save(&storage, &record).unwrap();
    assert!(load(&storage, &record.plan.id)
        .unwrap_err()
        .contains("Invalid managed worktree paths"));
    record.version = 42;
    save(&storage, &record).unwrap();
    assert!(list(&storage).unwrap_err().contains("unsupported"));
}

#[test]
fn journal_rejects_an_unmapped_workspace_and_inconsistent_progress() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let a_repo = repo(&a);
    let storage = temp.path().join("app/workspaces.json");
    let mut record = build_record(
        request(&[&a]),
        spec(&[&a], &a),
        &temp.path().join("storage"),
        None,
    )
    .unwrap();
    let mapped = record.plan.workspace.clone();
    record.plan.workspace = record.plan.source.clone();
    save(&storage, &record).unwrap();
    assert!(load(&storage, &record.plan.id)
        .unwrap_err()
        .contains("does not match"));
    assert!(!a_repo.path().join("worktrees").exists());
    record.plan.workspace = mapped;
    record.members[0].progress = MemberProgress::Created;
    save(&storage, &record).unwrap();
    assert!(discard(&storage, &record.plan.id)
        .unwrap_err()
        .contains("progress"));
    assert!(record_path(&storage, &record.plan.id).unwrap().exists());
}

#[test]
fn branch_conflicts_are_rejected_before_any_git_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let a_repo = repo(&a);
    let source = spec(&[&a], &a);
    let mut req = request(&[&a]);
    req.checkouts[0].branch = "HEAD".into();
    assert!(
        build_record(req, source.clone(), &temp.path().join("storage"), None)
            .unwrap_err()
            .contains("Invalid new branch")
    );
    let commit = a_repo.head().unwrap().peel_to_commit().unwrap();
    a_repo.branch("feature", &commit, false).unwrap();
    assert!(
        build_record(request(&[&a]), source, &temp.path().join("storage"), None)
            .unwrap_err()
            .contains("conflicts with existing branch")
    );
    assert!(!a_repo.path().join("worktrees").exists());
    let linked = temp.path().join("linked");
    a_repo.worktree("linked", &linked, None).unwrap();
    let mut req = request(&[&a, &linked]);
    req.checkouts[0].branch = "task".into();
    req.checkouts[1].branch = "task/sub".into();
    assert!(build_record(
        req,
        spec(&[&a, &linked], &a),
        &temp.path().join("storage"),
        None
    )
    .unwrap_err()
    .contains("path conflicts"));
    assert!(a_repo.find_branch("task", git2::BranchType::Local).is_err());
}
