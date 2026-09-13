use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{
    ArtifactReference, EvalError, EvalRun, HarnessComparisonReport, format_comparison_report,
};

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, EvalError> {
        let root = root.into();
        create_private_directory(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn persist_comparison_report(
        &self,
        report: &HarnessComparisonReport,
    ) -> Result<(), EvalError> {
        write_private_json(&self.root.join("report.json"), report)?;
        let mut text = format_comparison_report(report);
        text.push('\n');
        write_private(&self.root.join("report.txt"), text.as_bytes())
    }

    pub(crate) fn persist_run(
        &self,
        run: &mut EvalRun,
        session_jsonl: Option<&str>,
    ) -> Result<(), EvalError> {
        let relative_directory = PathBuf::from("runs").join(&run.run_id);
        let directory = self.root.join(&relative_directory);
        create_private_directory(&directory)?;

        let observation_path = directory.join("observation.json");
        write_private_json(&observation_path, &run.observation)?;
        run.artifacts.push(reference(
            "observation.json",
            relative_directory.join("observation.json"),
        ));

        let changes_path = directory.join("workspace-changes.json");
        write_private_json(&changes_path, &run.observation.workspace_changes)?;
        run.artifacts.push(reference(
            "workspace-changes.json",
            relative_directory.join("workspace-changes.json"),
        ));

        if let Some(session_jsonl) = session_jsonl {
            let session_path = directory.join("session.jsonl");
            write_private(&session_path, session_jsonl.as_bytes())?;
            run.artifacts.push(reference(
                "session.jsonl",
                relative_directory.join("session.jsonl"),
            ));
        }

        let run_path = directory.join("run.json");
        run.artifacts
            .push(reference("run.json", relative_directory.join("run.json")));
        write_private_json(&run_path, run)?;

        let encoded = serde_json::to_vec(run)
            .map_err(|error| EvalError::Artifact(format!("cannot encode eval run: {error}")))?;
        let runs_path = self.root.join("runs.jsonl");
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&runs_path).map_err(|error| {
            EvalError::Artifact(format!("cannot open {}: {error}", runs_path.display()))
        })?;
        file.write_all(&encoded)
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|error| {
                EvalError::Artifact(format!("cannot append {}: {error}", runs_path.display()))
            })?;
        Ok(())
    }
}

fn reference(name: &str, path: PathBuf) -> ArtifactReference {
    ArtifactReference {
        name: name.to_string(),
        path: path
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    }
}

fn create_private_directory(path: &Path) -> Result<(), EvalError> {
    std::fs::create_dir_all(path).map_err(|error| {
        EvalError::Artifact(format!("cannot create {}: {error}", path.display()))
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(0o700)
    })
    .map_err(|error| EvalError::Artifact(format!("cannot protect {}: {error}", path.display())))?;
    Ok(())
}

fn write_private_json(path: &Path, value: &impl serde::Serialize) -> Result<(), EvalError> {
    let mut encoded = serde_json::to_vec_pretty(value).map_err(|error| {
        EvalError::Artifact(format!("cannot encode {}: {error}", path.display()))
    })?;
    encoded.push(b'\n');
    write_private(path, &encoded)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), EvalError> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        EvalError::Artifact(format!("cannot create {}: {error}", path.display()))
    })?;
    file.write_all(bytes)
        .map_err(|error| EvalError::Artifact(format!("cannot write {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalExecutionOutcome, EvalObservation, EvalUsage, model::EVAL_RUN_SCHEMA_VERSION};

    #[test]
    fn store_writes_jsonl_and_run_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(directory.path()).unwrap();
        let mut run = EvalRun {
            schema_version: EVAL_RUN_SCHEMA_VERSION,
            run_id: "run-1".to_string(),
            case_id: "smoke/basic".to_string(),
            variant: "candidate".to_string(),
            provider: "fixture".to_string(),
            model: "fixture".to_string(),
            repetition: 1,
            started_at_ms: 1,
            duration_ms: 2,
            execution_outcome: EvalExecutionOutcome::Completed,
            passed: true,
            observation: EvalObservation {
                system_prompt: None,
                final_response: "ok".to_string(),
                transcript: Vec::new(),
                workspace_changes: Vec::new(),
                usage: EvalUsage::default(),
                errors: Vec::new(),
            },
            grades: Vec::new(),
            artifacts: Vec::new(),
        };
        store
            .persist_run(&mut run, Some("{\"version\":4}\n"))
            .unwrap();
        assert!(directory.path().join("runs.jsonl").exists());
        assert!(directory.path().join("runs/run-1/session.jsonl").exists());
        let persisted: EvalRun = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join("runs/run-1/run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.artifacts.len(), 4);
    }
}
