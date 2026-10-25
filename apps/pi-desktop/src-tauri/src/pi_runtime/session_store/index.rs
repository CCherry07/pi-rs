use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

use super::{path_is_archived, session_files, session_summary, SessionSummary};

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionSortKey {
    CreatedAt,
    #[default]
    UpdatedAt,
}

impl SessionSortKey {
    fn timestamp(self, summary: &SessionSummary) -> i64 {
        match self {
            Self::CreatedAt => summary.created_at_ms,
            Self::UpdatedAt => summary.updated_at_ms,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionListQuery {
    pub(crate) workspace_id: String,
    pub(crate) cursor: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct SessionListOptions {
    pub(crate) archived: bool,
    pub(crate) sort: SessionSortKey,
    pub(crate) limit: usize,
}

pub(crate) struct SessionPage {
    pub(crate) workspace_id: String,
    pub(crate) summaries: Vec<SessionSummary>,
    pub(crate) next_cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    version: u8,
    workspace_id: String,
    archived: bool,
    sort: SessionSortKey,
    timestamp: i64,
    id: String,
}

#[derive(PartialEq, Eq)]
struct FileVersion {
    modified: SystemTime,
    len: u64,
}

struct CachedSummary {
    version: FileVersion,
    summary: SessionSummary,
}

/// One disk scan per batch. Unchanged JSONL files retain their parsed summaries.
#[derive(Default)]
pub(super) struct SessionIndex {
    summaries: HashMap<PathBuf, CachedSummary>,
    #[cfg(test)]
    parsed_files: usize,
}

impl SessionIndex {
    pub(super) fn refresh(&mut self, root: &Path) -> Result<(), String> {
        let paths = session_files(root)?;
        let present: HashSet<_> = paths.iter().collect();
        self.summaries.retain(|path, _| present.contains(path));
        for path in paths {
            let version = std::fs::metadata(&path).and_then(|metadata| {
                Ok(FileVersion {
                    modified: metadata.modified()?,
                    len: metadata.len(),
                })
            });
            let Ok(version) = version else {
                self.summaries.remove(&path);
                continue;
            };
            if self
                .summaries
                .get(&path)
                .is_some_and(|entry| entry.version == version)
            {
                continue;
            }
            #[cfg(test)]
            {
                self.parsed_files += 1;
            }
            match session_summary(&path) {
                Ok(summary) => {
                    self.summaries
                        .insert(path, CachedSummary { version, summary });
                }
                Err(_) => {
                    self.summaries.remove(&path);
                }
            }
        }
        Ok(())
    }

    pub(super) fn pages(
        &self,
        projects: &[pi_sdk::projects::Project],
        queries: &[SessionListQuery],
        options: SessionListOptions,
    ) -> Result<Vec<SessionPage>, String> {
        let requested: HashSet<_> = queries
            .iter()
            .map(|query| query.workspace_id.as_str())
            .collect();
        for id in &requested {
            if !projects.iter().any(|project| project.id == *id) {
                return Err(format!("unknown project: {id}"));
            }
        }
        let mut by_project: HashMap<&str, Vec<&SessionSummary>> = HashMap::new();
        for (path, entry) in &self.summaries {
            if path_is_archived(path) != options.archived {
                continue;
            }
            let summary = &entry.summary;
            let project_id = summary.project_id.as_deref().or_else(|| {
                pi_sdk::projects::project_for_legacy_cwd(projects, &summary.cwd)
                    .map(|project| project.id.as_str())
            });
            if let Some(id) = project_id.filter(|id| requested.contains(id)) {
                by_project.entry(id).or_default().push(summary);
            }
        }
        for summaries in by_project.values_mut() {
            summaries.sort_by(|left, right| {
                options
                    .sort
                    .timestamp(right)
                    .cmp(&options.sort.timestamp(left))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        queries
            .iter()
            .map(|query| {
                let cursor = query
                    .cursor
                    .as_deref()
                    .map(|encoded| decode_cursor(encoded, query, options))
                    .transpose()?;
                let summaries = by_project
                    .get(query.workspace_id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut remaining = summaries.iter().copied().filter(|summary| {
                    cursor.as_ref().is_none_or(|cursor| {
                        let timestamp = options.sort.timestamp(summary);
                        timestamp < cursor.timestamp
                            || (timestamp == cursor.timestamp && summary.id > cursor.id)
                    })
                });
                let page: Vec<_> = remaining
                    .by_ref()
                    .take(options.limit.max(1))
                    .cloned()
                    .collect();
                let next_cursor = if remaining.next().is_some() {
                    page.last()
                        .map(|last| {
                            let cursor = Cursor {
                                version: 1,
                                workspace_id: query.workspace_id.clone(),
                                archived: options.archived,
                                sort: options.sort,
                                timestamp: options.sort.timestamp(last),
                                id: last.id.clone(),
                            };
                            serde_json::to_vec(&cursor)
                                .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
                                .map_err(|error| error.to_string())
                        })
                        .transpose()?
                } else {
                    None
                };
                Ok(SessionPage {
                    workspace_id: query.workspace_id.clone(),
                    summaries: page,
                    next_cursor,
                })
            })
            .collect()
    }
}

fn decode_cursor(
    encoded: &str,
    query: &SessionListQuery,
    options: SessionListOptions,
) -> Result<Cursor, String> {
    let cursor: Cursor = URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("invalid session cursor")?;
    if cursor.version != 1
        || cursor.workspace_id != query.workspace_id
        || cursor.archived != options.archived
        || cursor.sort != options.sort
    {
        return Err("session cursor does not match this listing".into());
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests;
