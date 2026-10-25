use std::time::{Duration, SystemTime};

use pi_sdk::projects::Project;
use pi_session::SessionHeader;

use super::*;

fn save(root: &Path, project: &Project, id: &str, created: i64, modified: i64) -> PathBuf {
    let path = root.join(format!("{id}.jsonl"));
    let mut header = SessionHeader::new(id, project.roots[0].path.clone());
    header.created_at = created;
    header.metadata = Some(project.session_metadata());
    pi_session::SessionStore::create(&path, header).unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_millis(modified as u64)),
        )
        .unwrap();
    path
}

fn query(project: &Project, cursor: Option<String>) -> SessionListQuery {
    SessionListQuery {
        workspace_id: project.id.clone(),
        cursor,
    }
}

fn options(sort: SessionSortKey, limit: usize) -> SessionListOptions {
    SessionListOptions {
        archived: false,
        sort,
        limit,
    }
}

#[test]
fn batches_projects_and_pages_past_one_hundred_without_losing_timestamp_ties() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("sessions");
    let first = Project::single_root("one", "one", directory.path());
    let second = Project::single_root("two", "two", directory.path());
    for index in 0..125 {
        save(
            &root,
            &first,
            &format!("a-{index:03}"),
            1000 + index / 2,
            2000 - index,
        );
    }
    save(&root, &second, "other-project", 9000, 9000);
    let projects = [first.clone(), second.clone()];
    let mut index = SessionIndex::default();
    index.refresh(&root).unwrap();
    let pages = index
        .pages(
            &projects,
            &[query(&first, None), query(&second, None)],
            options(SessionSortKey::CreatedAt, 17),
        )
        .unwrap();
    assert_eq!(pages[1].summaries[0].id, "other-project");
    assert!(pages[1].next_cursor.is_none());
    let mut ids = Vec::new();
    let mut cursor = None;
    loop {
        let page = index
            .pages(
                &projects,
                &[query(&first, cursor)],
                options(SessionSortKey::CreatedAt, 17),
            )
            .unwrap()
            .remove(0);
        ids.extend(page.summaries.into_iter().map(|summary| summary.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 125);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 125);
    assert_eq!(&ids[..3], ["a-124", "a-122", "a-123"]);
    let updated = index
        .pages(
            &projects,
            &[query(&first, None)],
            options(SessionSortKey::UpdatedAt, 17),
        )
        .unwrap();
    assert_eq!(updated[0].summaries[0].id, "a-000");
    assert_eq!(updated[0].summaries[0].created_at_ms, 1000);
    assert_eq!(updated[0].summaries[0].updated_at_ms, 2000);
}

#[test]
fn reuses_unchanged_summaries_and_observes_edits_archives_and_deletions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("sessions");
    let project = Project::single_root("one", "one", directory.path());
    let path = save(&root, &project, "session", 1000, 2000);
    let mut index = SessionIndex::default();
    index.refresh(&root).unwrap();
    index.refresh(&root).unwrap();
    assert_eq!(index.parsed_files, 1);
    let (log, _) = pi_session::SessionStore::open(&path).unwrap();
    log.set_name(Some("Updated title".into())).unwrap();
    index.refresh(&root).unwrap();
    assert_eq!(index.parsed_files, 2);
    let projects = [project.clone()];
    let request = [query(&project, None)];
    let options = options(SessionSortKey::CreatedAt, 20);
    let page = index.pages(&projects, &request, options).unwrap();
    assert_eq!(page[0].summaries[0].title, "Updated title");
    assert_eq!(page[0].summaries[0].created_at_ms, 1000);
    let archived = root.join("archived/session.jsonl");
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::rename(&path, &archived).unwrap();
    index.refresh(&root).unwrap();
    assert!(index.pages(&projects, &request, options).unwrap()[0]
        .summaries
        .is_empty());
    assert_eq!(
        index
            .pages(
                &projects,
                &request,
                SessionListOptions {
                    archived: true,
                    ..options
                }
            )
            .unwrap()[0]
            .summaries
            .len(),
        1
    );
    std::fs::remove_file(archived).unwrap();
    index.refresh(&root).unwrap();
    assert!(index.summaries.is_empty());
}

#[test]
fn cursor_is_bound_to_project_sort_and_archive_state_but_survives_a_deleted_row() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("sessions");
    let project = Project::single_root("one", "one", directory.path());
    let other = Project::single_root("two", "two", directory.path());
    save(&root, &project, "older", 1000, 1000);
    let newest = save(&root, &project, "newest", 2000, 2000);
    let projects = [project.clone(), other.clone()];
    let mut index = SessionIndex::default();
    index.refresh(&root).unwrap();
    let options = options(SessionSortKey::CreatedAt, 1);
    let cursor = index
        .pages(&projects, &[query(&project, None)], options)
        .unwrap()
        .remove(0)
        .next_cursor;
    assert!(index
        .pages(&projects, &[query(&other, cursor.clone())], options)
        .is_err());
    assert!(index
        .pages(
            &projects,
            &[query(&project, cursor.clone())],
            SessionListOptions {
                sort: SessionSortKey::UpdatedAt,
                ..options
            }
        )
        .is_err());
    assert!(index
        .pages(
            &projects,
            &[query(&project, cursor.clone())],
            SessionListOptions {
                archived: true,
                ..options
            }
        )
        .is_err());
    assert!(index
        .pages(
            &projects,
            &[query(&project, Some("invalid".into()))],
            options
        )
        .is_err());
    std::fs::remove_file(newest).unwrap();
    index.refresh(&root).unwrap();
    let page = index
        .pages(&projects, &[query(&project, cursor)], options)
        .unwrap();
    assert_eq!(page[0].summaries[0].id, "older");
    assert!(page[0].next_cursor.is_none());
}
