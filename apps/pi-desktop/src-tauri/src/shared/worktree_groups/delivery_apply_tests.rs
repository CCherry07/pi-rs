use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use super::*;

fn repository() -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let attributes = repo.path().join("empty-attributes");
    fs::write(&attributes, "").unwrap();
    let hooks = repo.path().join("test-hooks");
    fs::create_dir(&hooks).unwrap();
    let mut config = repo.config().unwrap();
    config
        .set_str("core.attributesfile", attributes.to_str().unwrap())
        .unwrap();
    config
        .set_str("core.hookspath", hooks.to_str().unwrap())
        .unwrap();
    config.set_str("user.name", "Delivery tests").unwrap();
    config
        .set_str("user.email", "delivery@example.invalid")
        .unwrap();
    config.set_bool("commit.gpgsign", false).unwrap();
    (directory, repo)
}

fn commit(repo: &Repository, files: &[(&str, &[u8])], parents: &[Oid]) -> Oid {
    let mut index = Index::new().unwrap();
    for (path, bytes) in files {
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100644,
                uid: 0,
                gid: 0,
                file_size: bytes.len() as u32,
                id: repo.blob(bytes).unwrap(),
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })
            .unwrap();
    }
    let tree = index.write_tree_to(repo).unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|oid| repo.find_commit(*oid).unwrap())
        .collect();
    let signature = repo.signature().unwrap();
    repo.commit(
        None,
        &signature,
        &signature,
        "Test commit",
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .unwrap()
}

fn checkout(repo: &Repository, oid: Oid) {
    repo.reference("refs/heads/target", oid, true, "test setup")
        .unwrap();
    repo.set_head("refs/heads/target").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
}

fn advance(repo: &Repository) -> (Oid, Oid) {
    let target = commit(repo, &[("file.txt", b"before\n")], &[]);
    let source = commit(repo, &[("file.txt", b"after\n")], &[target]);
    checkout(repo, target);
    (target, source)
}

fn snapshot(path: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                visit(base, &entry.path(), result)
            } else {
                result.insert(
                    entry.path().strip_prefix(base).unwrap().to_path_buf(),
                    (
                        fs::read(entry.path()).unwrap(),
                        metadata.modified().unwrap(),
                    ),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(path, path, &mut result);
    result
}

#[test]
fn fast_forward_and_read_only_recovery_classification() {
    let (directory, repo) = repository();
    let (target, source) = advance(&repo);
    let before = snapshot(directory.path());
    assert_eq!(prepare(&repo, source, target).unwrap(), source);
    assert_eq!(
        classify(&repo, "target", target, None).unwrap(),
        ApplyState::Unchanged
    );
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::Unchanged
    );
    assert_eq!(snapshot(directory.path()), before);
    apply(&repo, "target", target, source).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(source));
    assert_eq!(
        fs::read(directory.path().join("file.txt")).unwrap(),
        b"after\n"
    );
    let completed = snapshot(directory.path());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::Completed
    );
    assert_eq!(snapshot(directory.path()), completed);
    assert_eq!(prepare(&repo, target, source).unwrap(), source);
}

#[test]
fn prepares_normal_merge_with_target_then_source_parents_and_only_object_writes() {
    let (directory, repo) = repository();
    let base = commit(
        &repo,
        &[("file.txt", b"one\ntwo\nthree\nfour\nfive\nsix\n")],
        &[],
    );
    let source = commit(
        &repo,
        &[("file.txt", b"SOURCE\ntwo\nthree\nfour\nfive\nsix\n")],
        &[base],
    );
    let target = commit(
        &repo,
        &[("file.txt", b"one\ntwo\nthree\nfour\nfive\nTARGET\n")],
        &[base],
    );
    checkout(&repo, target);
    let without_objects = |mut files: BTreeMap<PathBuf, (Vec<u8>, SystemTime)>| {
        files.retain(|path, _| !path.starts_with(".git/objects"));
        files
    };
    let before = without_objects(snapshot(directory.path()));
    let result = prepare(&repo, source, target).unwrap();
    let merged = repo.find_commit(result).unwrap();
    assert_eq!(
        merged.parent_ids().collect::<Vec<_>>(),
        vec![target, source]
    );
    assert_eq!(without_objects(snapshot(directory.path())), before);
    apply(&repo, "target", target, result).unwrap();
    assert_eq!(
        fs::read(directory.path().join("file.txt")).unwrap(),
        b"SOURCE\ntwo\nthree\nfour\nfive\nTARGET\n"
    );
    assert_eq!(
        classify(&repo, "target", target, Some(result)).unwrap(),
        ApplyState::Completed
    );
}

#[test]
fn rename_modify_merge_uses_the_preview_result() {
    let (directory, repo) = repository();
    let base = commit(&repo, &[("old.txt", b"before\n")], &[]);
    let source = commit(&repo, &[("new.txt", b"before\n")], &[base]);
    let target = commit(&repo, &[("old.txt", b"after\n")], &[base]);
    checkout(&repo, target);
    let result = prepare(&repo, source, target).unwrap();
    apply(&repo, "target", target, result).unwrap();
    assert!(!directory.path().join("old.txt").exists());
    assert_eq!(
        fs::read(directory.path().join("new.txt")).unwrap(),
        b"after\n"
    );
}

#[test]
fn rejects_staged_unstaged_and_untracked_changes_without_writes() {
    for change in ["staged", "unstaged", "untracked"] {
        let (directory, repo) = repository();
        let (target, source) = advance(&repo);
        let path = if change == "untracked" {
            "untracked.txt"
        } else {
            "file.txt"
        };
        fs::write(directory.path().join(path), "local edits\n").unwrap();
        if change == "staged" {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new(path)).unwrap();
            index.write().unwrap();
        }
        let before = snapshot(directory.path());
        assert!(apply(&repo, "target", target, source).is_err(), "{change}");
        assert_eq!(
            classify(&repo, "target", target, Some(source)).unwrap(),
            ApplyState::NeedsAttention
        );
        assert_eq!(snapshot(directory.path()), before, "{change}");
    }
}

#[test]
fn refuses_head_and_branch_locks_without_touching_any_state() {
    for name in ["HEAD", "refs/heads/target"] {
        let (directory, repo) = repository();
        let (target, source) = advance(&repo);
        let mut transaction = repo.transaction().unwrap();
        transaction.lock_ref(name).unwrap();
        let before = snapshot(directory.path());
        assert!(apply(&repo, "target", target, source).is_err());
        assert!(finish(&repo, "target", target, source).is_err());
        assert_eq!(
            classify(&repo, "target", target, Some(source)).unwrap(),
            ApplyState::NeedsAttention
        );
        assert_eq!(snapshot(directory.path()), before);
    }
}

#[test]
fn recovery_only_updates_ref_after_exact_prepared_checkout() {
    let (directory, repo) = repository();
    let (target, source) = advance(&repo);
    assert!(finish(&repo, "target", target, source).is_err());
    let result = repo.find_commit(source).unwrap();
    repo.checkout_tree(
        result.as_object(),
        Some(git2::build::CheckoutBuilder::new().safe()),
    )
    .unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(target));
    let before = snapshot(directory.path());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::ReadyToFinish
    );
    assert_eq!(snapshot(directory.path()), before);
    let index_before = fs::read(repo.path().join("index")).unwrap();
    finish(&repo, "target", target, source).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(source));
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_before);
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::Completed
    );
}

#[test]
fn dirty_or_moved_recovery_remains_untouched() {
    let (directory, repo) = repository();
    let (target, source) = advance(&repo);
    let result = repo.find_commit(source).unwrap();
    repo.checkout_tree(
        result.as_object(),
        Some(git2::build::CheckoutBuilder::new().safe()),
    )
    .unwrap();
    fs::write(directory.path().join("file.txt"), "operator edits").unwrap();
    let before = snapshot(directory.path());
    assert!(finish(&repo, "target", target, source).is_err());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
    assert_eq!(snapshot(directory.path()), before);
    checkout(&repo, source);
    repo.set_head_detached(source).unwrap();
    let detached = snapshot(directory.path());
    assert!(apply(&repo, "target", target, source).is_err());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
    assert_eq!(snapshot(directory.path()), detached);
}

#[test]
fn preserves_ignored_files_and_refuses_overwriting_them() {
    for collision in [false, true] {
        let (directory, repo) = repository();
        let target = commit(
            &repo,
            &[(".gitignore", b"cache/\n"), ("file", b"before")],
            &[],
        );
        let mut files: Vec<(&str, &[u8])> = vec![(".gitignore", b"cache/\n"), ("file", b"after")];
        if collision {
            files.push(("cache/keep", b"incoming"));
        }
        let source = commit(&repo, &files, &[target]);
        checkout(&repo, target);
        fs::create_dir(directory.path().join("cache")).unwrap();
        fs::write(directory.path().join("cache/keep"), "local ignored data").unwrap();
        let before = snapshot(directory.path());
        let applied = apply(&repo, "target", target, source);
        assert_eq!(applied.is_err(), collision);
        assert_eq!(
            fs::read(directory.path().join("cache/keep")).unwrap(),
            b"local ignored data"
        );
        if collision {
            assert_eq!(repo.head().unwrap().target(), Some(target));
            assert_eq!(snapshot(directory.path()), before);
        }
    }
}

#[test]
fn refuses_active_hooks_signing_filters_attributes_and_sparse_index() {
    let (directory, repo) = repository();
    let (target, source) = advance(&repo);
    let hook = repo.path().join("test-hooks/post-merge");
    fs::write(&hook, "#!/bin/sh\nexit 99\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(prepare(&repo, source, target)
        .unwrap_err()
        .contains("post-merge"));
    let before = snapshot(directory.path());
    assert!(apply(&repo, "target", target, source)
        .unwrap_err()
        .contains("post-merge"));
    assert_eq!(snapshot(directory.path()), before);
    fs::remove_file(hook).unwrap();
    for (key, value) in [
        ("commit.gpgsign", "true"),
        ("filter.test.smudge", "cat"),
        ("core.sparsecheckout", "true"),
        ("core.autocrlf", "input"),
    ] {
        repo.config().unwrap().set_str(key, value).unwrap();
        assert!(support_blockers(&repo, source, target)
            .unwrap()
            .iter()
            .any(|item| item.contains(key)));
        repo.config().unwrap().remove(key).unwrap();
    }
    fs::write(directory.path().join(".gitattributes"), "* filter=test\n").unwrap();
    assert!(prepare(&repo, source, target)
        .unwrap_err()
        .contains("attributes"));
    fs::remove_file(directory.path().join(".gitattributes")).unwrap();
    let mut index = repo.index().unwrap();
    let mut entry = index.get_path(Path::new("file.txt"), 0).unwrap();
    entry.flags |= git2::IndexEntryFlag::VALID.bits();
    index.add(&entry).unwrap();
    index.write().unwrap();
    fs::write(directory.path().join("file.txt"), "hidden local changes").unwrap();
    assert!(apply(&repo, "target", target, source)
        .unwrap_err()
        .contains("assume-unchanged"));
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
}

#[test]
fn conflicting_preparation_leaves_checkout_untouched() {
    let (directory, repo) = repository();
    let base = commit(&repo, &[("file", b"base")], &[]);
    let target = commit(&repo, &[("file", b"target")], &[base]);
    let source = commit(&repo, &[("file", b"source")], &[base]);
    checkout(&repo, target);
    let before = snapshot(directory.path());
    assert!(prepare(&repo, source, target)
        .unwrap_err()
        .contains("Conflicts"));
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn operations_index_locks_and_moved_refs_require_attention() {
    for file in ["index.lock", "MERGE_HEAD"] {
        let (directory, repo) = repository();
        let (target, source) = advance(&repo);
        fs::write(repo.path().join(file), format!("{source}\n")).unwrap();
        let before = snapshot(directory.path());
        assert!(apply(&repo, "target", target, source).is_err());
        assert!(finish(&repo, "target", target, source).is_err());
        assert_eq!(
            classify(&repo, "target", target, Some(source)).unwrap(),
            ApplyState::NeedsAttention
        );
        assert_eq!(snapshot(directory.path()), before);
    }
    let (directory, repo) = repository();
    let (target, source) = advance(&repo);
    let moved = commit(&repo, &[("file.txt", b"operator commit")], &[target]);
    checkout(&repo, moved);
    let before = snapshot(directory.path());
    assert!(apply(&repo, "target", target, source).is_err());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn linked_checkout_locks_its_head_and_the_common_branch_reference() {
    let (_directory, repo) = repository();
    let (target, source) = advance(&repo);
    let destination = tempfile::tempdir().unwrap();
    let path = destination.path().join("linked");
    let reference = repo
        .branch("destination", &repo.find_commit(target).unwrap(), false)
        .unwrap();
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(reference.get()));
    repo.worktree("destination", &path, Some(&options)).unwrap();
    let linked = Repository::open(&path).unwrap();
    assert_ne!(linked.path(), linked.commondir());
    let mut transaction = repo.transaction().unwrap();
    transaction.lock_ref("refs/heads/destination").unwrap();
    assert_eq!(
        classify(&linked, "destination", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
    assert!(apply(&linked, "destination", target, source).is_err());
    drop(transaction);
    apply(&linked, "destination", target, source).unwrap();
    assert_eq!(linked.head().unwrap().target(), Some(source));
    assert_eq!(repo.head().unwrap().target(), Some(target));
    assert_eq!(fs::read(path.join("file.txt")).unwrap(), b"after\n");
    assert_eq!(
        classify(&linked, "destination", target, Some(source)).unwrap(),
        ApplyState::Completed
    );
}

#[test]
fn concurrent_staging_is_rejected_throughout_checkout_instead_of_silently_overwritten() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (directory, repo) = repository();
    let target = commit(
        &repo,
        &[("changed.txt", b"before\n"), ("stable.txt", b"original\n")],
        &[],
    );
    let source = commit(
        &repo,
        &[("changed.txt", b"after\n"), ("stable.txt", b"original\n")],
        &[target],
    );
    checkout(&repo, target);
    let path = directory.path().to_path_buf();
    let (checkout_started, wait_for_checkout) = mpsc::sync_channel(0);
    let (resume_checkout, wait_for_staging) = mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        let repo = Repository::open(path).unwrap();
        let mut checkout = git2::build::CheckoutBuilder::new();
        let mut first_progress = true;
        checkout.progress(move |_, _, _| {
            if first_progress {
                first_progress = false;
                checkout_started.send(()).unwrap();
                wait_for_staging
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        });
        apply_with_checkout(&repo, "target", target, source, checkout)
    });
    wait_for_checkout
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let external = Repository::open(directory.path()).unwrap();
    let mut index = external.index().unwrap();
    let mut staged = index.get_path(Path::new("stable.txt"), 0).unwrap();
    let content = b"unique staged user content\n";
    staged.id = external.blob(content).unwrap();
    staged.file_size = content.len() as u32;
    index.add(&staged).unwrap();
    let staged_result = index.write();
    resume_checkout.send(()).unwrap();
    worker.join().unwrap().unwrap();

    assert_eq!(staged_result.unwrap_err().code(), ErrorCode::Locked);
    assert_eq!(repo.head().unwrap().target(), Some(source));
    assert!(!repo.path().join("index.lock").exists());
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::Completed
    );
}

#[test]
fn failed_checkout_preserves_the_original_index_branch_and_operator_changes() {
    let (directory, repo) = repository();
    let target = commit(
        &repo,
        &[("changed.txt", b"before\n"), ("stable.txt", b"original\n")],
        &[],
    );
    let source = commit(
        &repo,
        &[("changed.txt", b"after\n"), ("stable.txt", b"original\n")],
        &[target],
    );
    checkout(&repo, target);
    let original_index = fs::read(repo.path().join("index")).unwrap();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.progress(|_, _, _| {
        fs::write(directory.path().join("stable.txt"), "operator edit\n").unwrap();
    });

    assert!(apply_with_checkout(&repo, "target", target, source, checkout).is_err());

    assert_eq!(repo.head().unwrap().target(), Some(target));
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), original_index);
    assert_eq!(
        fs::read(directory.path().join("stable.txt")).unwrap(),
        b"operator edit\n"
    );
    assert!(!repo.path().join("index.lock").exists());
    assert!(fs::read_dir(repo.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".pi-delivery-index-")));
    assert_eq!(
        classify(&repo, "target", target, Some(source)).unwrap(),
        ApplyState::NeedsAttention
    );
}
