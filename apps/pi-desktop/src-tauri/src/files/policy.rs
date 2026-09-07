use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileScope {
    Workspace,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileKind {
    Agents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FilePolicy {
    pub(crate) filename: &'static str,
    pub(crate) root_context: &'static str,
    pub(crate) root_may_be_missing: bool,
    pub(crate) create_root: bool,
    pub(crate) allow_external_symlink_target: bool,
}

const AGENTS_FILENAME: &str = "AGENTS.md";
pub(crate) fn policy_for(scope: FileScope, kind: FileKind) -> Result<FilePolicy, String> {
    match (scope, kind) {
        (FileScope::Workspace, FileKind::Agents) => Ok(FilePolicy {
            filename: AGENTS_FILENAME,
            root_context: "workspace root",
            root_may_be_missing: false,
            create_root: false,
            allow_external_symlink_target: false,
        }),
        (FileScope::Global, FileKind::Agents) => Ok(FilePolicy {
            filename: AGENTS_FILENAME,
            root_context: "Pi agent directory",
            root_may_be_missing: true,
            create_root: true,
            allow_external_symlink_target: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{policy_for, FileKind, FileScope};

    #[test]
    fn workspace_agents_policy_is_strict() {
        let policy = policy_for(FileScope::Workspace, FileKind::Agents).expect("policy");
        assert_eq!(policy.filename, "AGENTS.md");
        assert_eq!(policy.root_context, "workspace root");
        assert!(!policy.root_may_be_missing);
        assert!(!policy.create_root);
        assert!(!policy.allow_external_symlink_target);
    }

    #[test]
    fn global_agents_policy_creates_root() {
        let policy = policy_for(FileScope::Global, FileKind::Agents).expect("policy");
        assert_eq!(policy.filename, "AGENTS.md");
        assert_eq!(policy.root_context, "Pi agent directory");
        assert!(policy.root_may_be_missing);
        assert!(policy.create_root);
        assert!(policy.allow_external_symlink_target);
    }
}
