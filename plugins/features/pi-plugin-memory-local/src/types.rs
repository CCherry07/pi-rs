use serde::{Deserialize, Serialize};

pub const MEMORY_EVENT_TYPE: &str = "pi.memory.v1";
pub const MAX_MEMORY_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_EVIDENCE_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MemoryScope {
    User,
    Project { root: String },
    Session { session_id: String },
}

impl MemoryScope {
    pub fn key(&self) -> String {
        match self {
            Self::User => "user".to_string(),
            Self::Project { root } => format!("project:{root}"),
            Self::Session { session_id } => format!("session:{session_id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryKind {
    #[default]
    Fact,
    Preference,
    Decision,
    Instruction,
    Summary,
}

impl MemoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::Instruction => "instruction",
            Self::Summary => "summary",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryOrigin {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEvidence {
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub text: String,
    pub origin: MemoryOrigin,
    pub evidence: MemoryEvidence,
    pub recorded_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

impl MemoryRecord {
    pub fn validate(&self) -> Result<(), MemoryValidationError> {
        validate_id("record id", &self.id)?;
        validate_text("memory text", &self.text, MAX_MEMORY_TEXT_BYTES)?;
        validate_text("memory evidence", &self.evidence.note, MAX_EVIDENCE_BYTES)?;
        validate_id("origin session id", &self.origin.session_id)?;
        match &self.scope {
            MemoryScope::User => {}
            MemoryScope::Project { root } => validate_id("project root", root)?,
            MemoryScope::Session { session_id } => {
                validate_id("scope session id", session_id)?;
            }
        }
        if let Some(target) = &self.supersedes {
            validate_id("superseded record id", target)?;
            if target == &self.id {
                return Err(MemoryValidationError::Invalid(
                    "a memory record cannot supersede itself".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
pub enum MemoryMutation {
    Remember {
        mutation_id: String,
        record: MemoryRecord,
    },
    Forget {
        mutation_id: String,
        target_id: String,
        reason: String,
        origin: MemoryOrigin,
        recorded_at_ms: i64,
    },
}

impl MemoryMutation {
    pub fn id(&self) -> &str {
        match self {
            Self::Remember { mutation_id, .. } | Self::Forget { mutation_id, .. } => mutation_id,
        }
    }

    pub fn validate(&self) -> Result<(), MemoryValidationError> {
        validate_id("mutation id", self.id())?;
        match self {
            Self::Remember { record, .. } => record.validate(),
            Self::Forget {
                target_id,
                reason,
                origin,
                ..
            } => {
                validate_id("forgotten record id", target_id)?;
                validate_text("forget reason", reason, MAX_EVIDENCE_BYTES)?;
                validate_id("origin session id", &origin.session_id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallQuery {
    pub text: String,
    pub scopes: Vec<MemoryScope>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryHit {
    pub record: MemoryRecord,
    pub score: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallResult {
    pub hits: Vec<MemoryHit>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReceipt {
    pub applied: usize,
    pub duplicates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexEntry {
    pub entry_id: String,
    pub role: String,
    pub text: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndexDocument {
    pub session_id: String,
    pub project_key: String,
    pub entries: Vec<SessionIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSearchQuery {
    pub text: String,
    pub project_key: String,
    pub session_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub entry_id: String,
    pub role: String,
    pub text: String,
    pub timestamp_ms: i64,
    pub score: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryValidationError {
    #[error("invalid memory mutation: {0}")]
    Invalid(String),
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Validation(#[from] MemoryValidationError),
    #[error("cannot initialize memory database {path}: {message}")]
    Initialize { path: String, message: String },
    #[error("memory provider failed: {0}")]
    Provider(String),
    #[error("memory maintenance failed: {0}")]
    Maintenance(String),
    #[error("memory worker failed: {0}")]
    Worker(String),
}

fn validate_id(label: &str, value: &str) -> Result<(), MemoryValidationError> {
    if value.trim().is_empty() {
        Err(MemoryValidationError::Invalid(format!(
            "{label} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn validate_text(label: &str, value: &str, max_bytes: usize) -> Result<(), MemoryValidationError> {
    if value.trim().is_empty() {
        return Err(MemoryValidationError::Invalid(format!(
            "{label} must not be empty"
        )));
    }
    if value.len() > max_bytes {
        return Err(MemoryValidationError::Invalid(format!(
            "{label} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> MemoryRecord {
        MemoryRecord {
            id: "record-1".to_string(),
            scope: MemoryScope::User,
            kind: MemoryKind::Preference,
            text: "Prefer Rust examples.".to_string(),
            origin: MemoryOrigin {
                session_id: "session-1".to_string(),
                entry_id: Some("entry-1".to_string()),
                tool_call_id: None,
            },
            evidence: MemoryEvidence {
                note: "The user explicitly requested this.".to_string(),
            },
            recorded_at_ms: 1,
            supersedes: None,
        }
    }

    #[test]
    fn record_validation_rejects_self_supersession() {
        let mut record = record();
        record.supersedes = Some(record.id.clone());
        assert!(record.validate().is_err());
    }

    #[test]
    fn scope_keys_are_stable_and_disjoint() {
        assert_eq!(MemoryScope::User.key(), "user");
        assert_eq!(
            MemoryScope::Project {
                root: "/repo".to_string()
            }
            .key(),
            "project:/repo"
        );
        assert_eq!(
            MemoryScope::Session {
                session_id: "abc".to_string()
            }
            .key(),
            "session:abc"
        );
    }
}
