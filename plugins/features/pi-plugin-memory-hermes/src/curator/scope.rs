//! Scope selection is shared by the standalone command, session command and worker.
use std::path::{Path, PathBuf};

use super::{Config, Curator, runtime::Request};
use crate::skills::SkillScope;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub key: String,
    pub scope: SkillScope,
    pub root: PathBuf,
}

impl Target {
    pub fn discover(global: PathBuf, cwd: &Path, trusted: bool) -> Vec<Self> {
        let mut targets = vec![Self {
            key: "global".into(),
            scope: SkillScope::Global,
            root: global,
        }];
        if trusted {
            let project = crate::project::detect_project(cwd);
            if let (Some(name), Some(root)) = (project.name, project.root) {
                targets.push(Self {
                    key: format!("project:{name}"),
                    scope: SkillScope::Project,
                    root: root.join(".hermes/skills"),
                });
            }
        }
        targets
    }

    pub fn curator(&self, config: &Config) -> Curator {
        Curator::new(self.root.clone(), config.clone())
    }

    pub fn qualify(&self, name: &str) -> String {
        format!("{}:{name}", self.key)
    }

    pub fn report(&self, mut report: super::Report) -> super::Report {
        if self.scope == SkillScope::Project {
            report.backup = report.backup.map(|id| self.qualify(&id));
            for action in report.planned.iter_mut().chain(report.completed.iter_mut()) {
                action.name = self.qualify(&action.name);
            }
        }
        report
    }
}

/// Resolve every target before any mutation. Bare object names retain global semantics.
pub(super) fn select<'a>(
    targets: &[Target],
    arguments: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<(Target, Request)>, String> {
    let mut arguments = arguments.into_iter();
    let mut args = Vec::new();
    let mut scope = None;
    while let Some(arg) = arguments.next() {
        if arg == "--scope" {
            if scope.is_some() {
                return Err("Specify --scope only once".into());
            }
            scope = Some(
                arguments
                    .next()
                    .ok_or("Expected --scope global|project|all")?,
            );
        } else {
            args.push(arg);
        }
    }
    if scope.is_some_and(|s| !matches!(s, "global" | "project" | "all")) {
        return Err("Expected --scope global|project|all".into());
    }
    let mut request = Request::parse(args)?;
    let object = match &mut request {
        Request::Pin(id, _) | Request::Adopt(id) | Request::Archive(id) | Request::Restore(id) => {
            Some(id)
        }
        Request::Rollback(id) => id.as_mut(),
        _ => None,
    };
    let mut identity_target = None;
    if let Some(id) = object {
        if let Some((target, name)) = targets.iter().find_map(|target| {
            id.strip_prefix(&format!("{}:", target.key))
                .map(|name| (target, name.to_owned()))
        }) {
            if scope.is_some_and(|scope| scope != target.scope.as_str()) {
                return Err("Skill/archive ID conflicts with --scope".into());
            }
            identity_target = Some(target.key.clone());
            *id = name;
        } else if id.starts_with("project:") || id.starts_with("global:") {
            return Err("Target is not the current trusted project or global library".into());
        }
        super::metadata::name(id).map_err(|e| e.to_string())?;
    }
    let single = matches!(
        request,
        Request::Pin(..)
            | Request::Adopt(_)
            | Request::Archive(_)
            | Request::Restore(_)
            | Request::Rollback(_)
    );
    if single && scope == Some("all") {
        return Err("Select one scope for this operation".into());
    }
    let scope = scope.unwrap_or(if single { "global" } else { "all" });
    let selected = targets
        .iter()
        .filter(|target| {
            identity_target.as_ref().map_or_else(
                || scope == "all" || target.scope.as_str() == scope,
                |key| &target.key == key,
            )
        })
        .cloned()
        .map(|target| (target, request.clone()))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("Project Curator requires a current trusted Git checkout".into());
    }
    Ok(selected)
}

pub(super) fn output(results: Vec<(String, String)>) -> Result<String, String> {
    // Keep the existing single-global command output.
    if results.len() == 1 && results[0].0 == "global" {
        return Ok(results.into_iter().next().expect("one result").1);
    }
    let value = results
        .into_iter()
        .map(|(key, value)| {
            let value = serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
            (key, value)
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}
