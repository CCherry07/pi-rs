use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::*;

fn repository() -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    // Tests must not inherit workstation-specific merge attributes.
    let attributes = directory.path().join("empty-attributes");
    fs::write(&attributes, "").unwrap();
    repo.config()
        .unwrap()
        .set_str("core.attributesfile", attributes.to_str().unwrap())
        .unwrap();
    (directory, repo)
}

fn commit(repo: &Repository, files: &[(&str, &[u8])], parents: &[Oid], summary: &str) -> Oid {
    let mut index = git2::Index::new().unwrap();
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
    let tree_id = index.write_tree_to(repo).unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|oid| repo.find_commit(*oid).unwrap())
        .collect();
    let references: Vec<_> = parents.iter().collect();
    let signature = git2::Signature::now("Preview tests", "preview@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, summary, &tree, &references)
        .unwrap()
}

fn checkout(repo: &Repository, oid: Oid) {
    repo.reference("refs/heads/target", oid, true, "test setup")
        .unwrap();
    repo.set_head("refs/heads/target").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
}

fn snapshot(path: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn read(base: &Path, path: &Path, output: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                read(base, &entry.path(), output);
            } else {
                output.insert(
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
    read(path, path, &mut result);
    result
}

#[test]
fn classifies_exact_ancestry_and_incoming_commits() {
    let (directory, repo) = repository();
    let base = commit(&repo, &[("file", b"base")], &[], "Base");
    let source = commit(&repo, &[("file", b"source")], &[base], "Incoming");
    checkout(&repo, base);
    let before = snapshot(directory.path());

    let same = compare(&repo, base, base).unwrap();
    assert_eq!(same.kind, MergeKind::UpToDate);
    assert_eq!((same.ahead, same.behind), (0, 0));
    let forward = compare(&repo, source, base).unwrap();
    assert_eq!(forward.kind, MergeKind::FastForward);
    assert_eq!((forward.ahead, forward.behind), (1, 0));
    assert_eq!(forward.merge_base_oids, vec![base.to_string()]);
    assert_eq!(forward.commits[0].oid, source.to_string());
    assert_eq!(forward.commits[0].summary, "Incoming");
    assert_eq!(forward.files[0].status, "modified");
    let behind = compare(&repo, base, source).unwrap();
    assert_eq!(behind.kind, MergeKind::UpToDate);
    assert_eq!((behind.ahead, behind.behind), (0, 1));
    assert!(behind.commits.is_empty());
    assert!(behind.files.is_empty());
    assert_eq!(snapshot(directory.path()), before);
    assert_eq!(
        serde_json::to_value(forward).unwrap()["kind"],
        "fastForward"
    );
}

#[test]
fn clean_content_merge_preserves_refs_index_worktree_and_all_object_bytes_and_times() {
    let (directory, repo) = repository();
    let base = commit(
        &repo,
        &[("file.txt", b"one\ntwo\nthree\nfour\nfive\nsix\n")],
        &[],
        "Base",
    );
    let source = commit(
        &repo,
        &[("file.txt", b"SOURCE\ntwo\nthree\nfour\nfive\nsix\n")],
        &[base],
        "Source",
    );
    let target = commit(
        &repo,
        &[("file.txt", b"one\ntwo\nthree\nfour\nfive\nTARGET\n")],
        &[base],
        "Target",
    );
    // An existing merged blob also guards against alternate-ODB timestamp freshening.
    repo.blob(b"SOURCE\ntwo\nthree\nfour\nfive\nTARGET\n")
        .unwrap();
    checkout(&repo, target);
    repo.reference("refs/heads/source", source, true, "test setup")
        .unwrap();
    fs::write(directory.path().join("staged.txt"), "staged data").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    fs::write(directory.path().join("file.txt"), "unstaged local data").unwrap();
    fs::write(directory.path().join("untracked.txt"), "untracked data").unwrap();
    let before = snapshot(directory.path());

    let preview = compare(&repo, source, target).unwrap();
    assert_eq!(preview.kind, MergeKind::Mergeable);
    assert_eq!((preview.ahead, preview.behind), (1, 1));
    assert!(preview.conflicts.is_empty());
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn reports_text_and_binary_conflicts_with_paths() {
    for (name, base_bytes, source_bytes, target_bytes) in [
        (
            "text.txt",
            b"base\n".as_slice(),
            b"source\n".as_slice(),
            b"target\n".as_slice(),
        ),
        (
            "binary.bin",
            b"\0base".as_slice(),
            b"\0source".as_slice(),
            b"\0target".as_slice(),
        ),
    ] {
        let (directory, repo) = repository();
        let base = commit(&repo, &[(name, base_bytes)], &[], "Base");
        let source = commit(&repo, &[(name, source_bytes)], &[base], "Source");
        let target = commit(&repo, &[(name, target_bytes)], &[base], "Target");
        checkout(&repo, target);
        let before = snapshot(directory.path());
        let preview = compare(&repo, source, target).unwrap();
        assert_eq!(preview.kind, MergeKind::Conflicts);
        assert_eq!(preview.conflicts, vec![name]);
        assert_eq!(snapshot(directory.path()), before);
    }
}

#[test]
fn detects_rename_modify_and_rename_rename_results() {
    let (_directory, repo) = repository();
    let base = commit(&repo, &[("old.txt", b"base\n")], &[], "Base");
    let source = commit(&repo, &[("new.txt", b"base\n")], &[base], "Rename");
    let edited = commit(&repo, &[("old.txt", b"edited\n")], &[base], "Edit");
    let merged = compare(&repo, source, edited).unwrap();
    assert_eq!(merged.kind, MergeKind::Mergeable);
    assert_eq!(merged.files[0].path, "new.txt");
    assert_eq!(merged.files[0].old_path.as_deref(), Some("old.txt"));
    assert_eq!(merged.files[0].status, "renamed");

    let other_rename = commit(&repo, &[("other.txt", b"base\n")], &[base], "Other rename");
    let conflicted = compare(&repo, source, other_rename).unwrap();
    assert_eq!(conflicted.kind, MergeKind::Conflicts);
    assert!(conflicted.conflicts.contains(&"new.txt".into()));
    assert!(conflicted.conflicts.contains(&"other.txt".into()));
}

#[test]
fn unrelated_and_multiple_base_histories_do_not_claim_mergeability() {
    let (_directory, repo) = repository();
    let root = commit(&repo, &[("file", b"base")], &[], "Root");
    let unrelated = commit(&repo, &[("other", b"other")], &[], "Unrelated");
    let preview = compare(&repo, root, unrelated).unwrap();
    assert_eq!(preview.kind, MergeKind::Unrelated);
    assert!(preview.merge_base_oids.is_empty());
    assert!(preview.files.is_empty());
    assert_eq!((preview.ahead, preview.behind), (1, 1));

    let left = commit(&repo, &[("file", b"left")], &[root], "Left");
    let right = commit(&repo, &[("file", b"right")], &[root], "Right");
    let first = commit(&repo, &[("file", b"first")], &[left, right], "First merge");
    let second = commit(
        &repo,
        &[("file", b"second")],
        &[right, left],
        "Second merge",
    );
    let preview = compare(&repo, first, second).unwrap();
    assert_eq!(preview.kind, MergeKind::Unsupported);
    assert_eq!(preview.merge_base_oids.len(), 2);
    assert!(preview.files.is_empty());
    assert!(preview
        .warnings
        .iter()
        .any(|warning| warning.contains("Multiple merge bases")));
}

#[test]
fn attributes_and_custom_drivers_are_explicitly_unclassified() {
    let (directory, repo) = repository();
    let attrs = b"*.txt merge=custom\n".as_slice();
    let base = commit(
        &repo,
        &[("file.txt", b"base"), (".gitattributes", attrs)],
        &[],
        "Base",
    );
    let source = commit(
        &repo,
        &[("file.txt", b"source"), (".gitattributes", attrs)],
        &[base],
        "Source",
    );
    let target = commit(
        &repo,
        &[("file.txt", b"target"), (".gitattributes", attrs)],
        &[base],
        "Target",
    );
    repo.config()
        .unwrap()
        .set_str("merge.custom.driver", "false")
        .unwrap();
    checkout(&repo, target);
    let before = snapshot(directory.path());
    let preview = compare(&repo, source, target).unwrap();
    assert_eq!(preview.kind, MergeKind::Unsupported);
    assert!(preview
        .warnings
        .iter()
        .any(|warning| warning.contains("custom merge drivers")));
    assert_eq!(snapshot(directory.path()), before);
    assert_eq!(
        compare(&repo, source, base).unwrap().kind,
        MergeKind::FastForward
    );
}

#[test]
fn count_limits_distinguish_exactly_full_lists_from_truncated_lists() {
    let (_directory, repo) = repository();
    let base = commit(&repo, &[], &[], "Base");
    let first = commit(&repo, &[("a", b"a")], &[base], "First");
    let second = commit(&repo, &[("a", b"a"), ("b", b"b")], &[first], "Second");
    let third = commit(
        &repo,
        &[("a", b"a"), ("b", b"b"), ("c", b"c")],
        &[second],
        "Third",
    );
    let exact = compare_bounded(&repo, second, base, 2, 2, MAX_MERGE_BYTES).unwrap();
    assert_eq!((exact.commits.len(), exact.files.len()), (2, 2));
    assert!(!exact.commits_truncated && !exact.files_truncated);
    let truncated = compare_bounded(&repo, third, base, 2, 2, MAX_MERGE_BYTES).unwrap();
    assert_eq!((truncated.commits.len(), truncated.files.len()), (2, 2));
    assert!(truncated.commits_truncated && truncated.files_truncated);
    assert_eq!((truncated.ahead, truncated.behind), (3, 0));
}

#[test]
fn memory_budget_counts_changed_blobs_without_copying_unrelated_large_files() {
    let (_directory, repo) = repository();
    let large_unchanged = vec![b'x'; 128 * 1024];
    let base = commit(
        &repo,
        &[
            ("large", &large_unchanged),
            ("left", b"base"),
            ("right", b"base"),
        ],
        &[],
        "Base",
    );
    let source = commit(
        &repo,
        &[
            ("large", &large_unchanged),
            ("left", b"source"),
            ("right", b"base"),
        ],
        &[base],
        "Source",
    );
    let target = commit(
        &repo,
        &[
            ("large", &large_unchanged),
            ("left", b"base"),
            ("right", b"target"),
        ],
        &[base],
        "Target",
    );
    let preview = compare_bounded(&repo, source, target, MAX_COMMITS, MAX_FILES, 4 * 1024).unwrap();
    assert_eq!(preview.kind, MergeKind::Mergeable);
    let limited = compare_bounded(&repo, source, target, MAX_COMMITS, MAX_FILES, 1).unwrap();
    assert_eq!(limited.kind, MergeKind::Unsupported);
    assert!(limited
        .warnings
        .iter()
        .any(|warning| warning.contains("memory budget")));
}

#[test]
fn shallow_boundaries_do_not_report_unrelated_histories_as_certain() {
    let (_directory, repo) = repository();
    let base = commit(&repo, &[("file", b"base")], &[], "Base");
    let source = commit(&repo, &[("file", b"source")], &[base], "Source");
    let target = commit(&repo, &[("file", b"target")], &[base], "Target");
    fs::write(repo.path().join("shallow"), format!("{source}\n{target}\n")).unwrap();
    let reopened = Repository::open(repo.path()).unwrap();
    let preview = compare(&reopened, source, target).unwrap();
    assert_eq!(preview.kind, MergeKind::Unsupported);
    assert!(preview
        .warnings
        .iter()
        .any(|warning| warning.contains("Shallow history")));
}
