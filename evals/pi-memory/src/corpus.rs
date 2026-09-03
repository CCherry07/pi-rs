use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use pi_plugin_memory_local::{MemoryMutation, MemoryRecord, MemoryScope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::EvalError;
use crate::filler::generate_filler_sessions;

const CORPUS_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CorpusManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub seed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvalAbility {
    StaticState,
    DynamicState,
    Procedure,
    Gotcha,
    PremiseAwareness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvalLanguageRelation {
    SameLanguage,
    CrossLanguage,
    MixedLanguage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForbiddenReason {
    WrongScope,
    Stale,
    Distractor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForbiddenRecord {
    pub record_id: String,
    pub reason: ForbiddenReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalQuestion {
    pub id: String,
    pub ability: EvalAbility,
    pub language: String,
    pub language_relation: EvalLanguageRelation,
    pub query: String,
    pub scopes: Vec<MemoryScope>,
    pub limit: usize,
    pub evidence_hops: Vec<Vec<String>>,
    #[serde(default)]
    pub forbidden: Vec<ForbiddenRecord>,
    pub expected_answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalSession {
    pub id: String,
    pub mutations: Vec<MemoryMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HaystackSpec {
    pub schema_version: u32,
    pub name: String,
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub filler_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalSuiteSpec {
    pub schema_version: u32,
    pub name: String,
    pub haystack: String,
    pub question_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EvalCorpus {
    manifest: CorpusManifest,
    sessions: Vec<EvalSession>,
    questions: Vec<EvalQuestion>,
    haystacks: BTreeMap<String, HaystackSpec>,
    suites: BTreeMap<String, EvalSuiteSpec>,
}

#[derive(Debug, Clone)]
pub struct EvalSuite {
    corpus_name: String,
    corpus_version: String,
    seed: u64,
    suite: String,
    haystack: String,
    sessions: Vec<EvalSession>,
    questions: Vec<EvalQuestion>,
    records: HashMap<String, MemoryRecord>,
}

struct HaystackInventory {
    records: HashMap<String, MemoryRecord>,
    inactive: HashSet<String>,
}

impl EvalCorpus {
    pub fn bundled() -> Result<Self, EvalError> {
        Self::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/v1"))
    }

    pub fn load(directory: impl AsRef<Path>) -> Result<Self, EvalError> {
        let directory = directory.as_ref();
        let manifest = read_json(&directory.join("manifest.json"))?;
        let sessions = read_jsonl(&directory.join("sessions.jsonl"))?;
        let questions = read_jsonl(&directory.join("questions.jsonl"))?;
        let haystack_directory = directory.join("haystacks");
        let entries = std::fs::read_dir(&haystack_directory)
            .map_err(|error| EvalError::read(&haystack_directory, error))?;
        let mut paths = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| EvalError::read(&haystack_directory, error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        });
        paths.sort();

        let mut haystacks = BTreeMap::new();
        for path in paths {
            let haystack: HaystackSpec = read_json(&path)?;
            let file_name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    EvalError::invalid(format!("invalid haystack path {}", path.display()))
                })?;
            if haystack.name != file_name {
                return Err(EvalError::invalid(format!(
                    "haystack {} declares name {:?}",
                    path.display(),
                    haystack.name
                )));
            }
            if haystacks.insert(haystack.name.clone(), haystack).is_some() {
                return Err(EvalError::invalid(format!(
                    "duplicate haystack {file_name:?}"
                )));
            }
        }

        let suite_directory = directory.join("suites");
        let entries = std::fs::read_dir(&suite_directory)
            .map_err(|error| EvalError::read(&suite_directory, error))?;
        let mut paths = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| EvalError::read(&suite_directory, error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        });
        paths.sort();

        let mut suites = BTreeMap::new();
        for path in paths {
            let suite: EvalSuiteSpec = read_json(&path)?;
            let file_name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    EvalError::invalid(format!("invalid suite path {}", path.display()))
                })?;
            if suite.name != file_name {
                return Err(EvalError::invalid(format!(
                    "suite {} declares name {:?}",
                    path.display(),
                    suite.name
                )));
            }
            if suites.insert(suite.name.clone(), suite).is_some() {
                return Err(EvalError::invalid(format!(
                    "duplicate evaluation suite {file_name:?}"
                )));
            }
        }

        let corpus = Self {
            manifest,
            sessions,
            questions,
            haystacks,
            suites,
        };
        corpus.validate()?;
        Ok(corpus)
    }

    pub fn manifest(&self) -> &CorpusManifest {
        &self.manifest
    }

    pub fn suites(&self) -> impl Iterator<Item = &str> {
        self.suites.keys().map(String::as_str)
    }

    pub fn suite(&self, name: &str) -> Result<EvalSuite, EvalError> {
        let suite = self.suites.get(name).ok_or_else(|| {
            EvalError::invalid(format!(
                "unknown evaluation suite {name:?}; available: {}",
                self.suites().collect::<Vec<_>>().join(", ")
            ))
        })?;
        let haystack = self
            .haystacks
            .get(&suite.haystack)
            .expect("validated suite haystack");
        let sessions = self.sessions_for_haystack(haystack);
        let records = sessions
            .iter()
            .flat_map(|session| session.mutations.iter())
            .filter_map(|mutation| match mutation {
                MemoryMutation::Remember { record, .. } => {
                    Some((record.id.clone(), record.clone()))
                }
                MemoryMutation::Forget { .. } => None,
            })
            .collect();
        let questions_by_id = self
            .questions
            .iter()
            .map(|question| (question.id.as_str(), question))
            .collect::<HashMap<_, _>>();
        let questions = suite
            .question_ids
            .iter()
            .map(|id| {
                (**questions_by_id
                    .get(id.as_str())
                    .expect("validated suite question"))
                .clone()
            })
            .collect();
        Ok(EvalSuite {
            corpus_name: self.manifest.name.clone(),
            corpus_version: self.manifest.version.clone(),
            seed: self.manifest.seed,
            suite: name.to_string(),
            haystack: suite.haystack.clone(),
            sessions,
            questions,
            records,
        })
    }

    fn validate(&self) -> Result<(), EvalError> {
        if self.manifest.schema_version != CORPUS_SCHEMA_VERSION {
            return Err(EvalError::invalid(format!(
                "unsupported corpus schema {}; expected {CORPUS_SCHEMA_VERSION}",
                self.manifest.schema_version
            )));
        }
        if self.manifest.name.trim().is_empty() || self.manifest.version.trim().is_empty() {
            return Err(EvalError::invalid(
                "manifest name and version must not be empty",
            ));
        }
        if self.sessions.is_empty()
            || self.questions.is_empty()
            || self.haystacks.is_empty()
            || self.suites.is_empty()
        {
            return Err(EvalError::invalid(
                "corpus requires sessions, questions, haystacks, and evaluation suites",
            ));
        }

        let mut session_ids = HashSet::new();
        let mut mutation_ids = HashSet::new();
        let mut records = HashMap::new();
        let mut inactive_targets = HashSet::new();
        for session in &self.sessions {
            require_non_empty("session id", &session.id)?;
            if !session_ids.insert(session.id.clone()) {
                return Err(EvalError::invalid(format!(
                    "duplicate session id {:?}",
                    session.id
                )));
            }
            if session.mutations.is_empty() {
                return Err(EvalError::invalid(format!(
                    "session {:?} has no mutations",
                    session.id
                )));
            }
            for mutation in &session.mutations {
                mutation.validate().map_err(|error| {
                    EvalError::invalid(format!(
                        "session {:?} contains invalid mutation: {error}",
                        session.id
                    ))
                })?;
                if !mutation_ids.insert(mutation.id().to_string()) {
                    return Err(EvalError::invalid(format!(
                        "duplicate mutation id {:?}",
                        mutation.id()
                    )));
                }
                match mutation {
                    MemoryMutation::Remember { record, .. } => {
                        require_origin(&session.id, &record.origin.session_id, mutation.id())?;
                        if let Some(target) = &record.supersedes {
                            inactive_targets.insert(target.clone());
                        }
                        if records.insert(record.id.clone(), record).is_some() {
                            return Err(EvalError::invalid(format!(
                                "duplicate record id {:?}",
                                record.id
                            )));
                        }
                    }
                    MemoryMutation::Forget {
                        target_id, origin, ..
                    } => {
                        require_origin(&session.id, &origin.session_id, mutation.id())?;
                        inactive_targets.insert(target_id.clone());
                    }
                }
            }
        }
        for target in &inactive_targets {
            if !records.contains_key(target) {
                return Err(EvalError::invalid(format!(
                    "supersession or tombstone references missing record {target:?}"
                )));
            }
        }

        let mut question_ids = HashSet::new();
        let mut invocation_keys = HashSet::new();
        for question in &self.questions {
            validate_question_shape(question)?;
            if !question_ids.insert(question.id.clone()) {
                return Err(EvalError::invalid(format!(
                    "duplicate question id {:?}",
                    question.id
                )));
            }
            let invocation_key = (
                question.query.clone(),
                question.scopes.clone(),
                question.limit,
            );
            if !invocation_keys.insert(invocation_key) {
                return Err(EvalError::invalid(format!(
                    "question {:?} duplicates another backend-visible input",
                    question.id
                )));
            }
        }

        let mut inventories = HashMap::new();
        for (haystack_name, haystack) in &self.haystacks {
            if haystack.schema_version != CORPUS_SCHEMA_VERSION {
                return Err(EvalError::invalid(format!(
                    "haystack {haystack_name:?} uses unsupported schema {}",
                    haystack.schema_version
                )));
            }
            if haystack.session_ids.is_empty() {
                return Err(EvalError::invalid(format!(
                    "haystack {haystack_name:?} has no sessions"
                )));
            }
            if haystack.filler_count > 10_000 {
                return Err(EvalError::invalid(format!(
                    "haystack {haystack_name:?} requests too many filler sessions: {}",
                    haystack.filler_count
                )));
            }
            let mut selected_ids = HashSet::new();
            for session_id in &haystack.session_ids {
                if !selected_ids.insert(session_id) {
                    return Err(EvalError::invalid(format!(
                        "haystack {haystack_name:?} repeats session {session_id:?}"
                    )));
                }
                if !session_ids.contains(session_id) {
                    return Err(EvalError::invalid(format!(
                        "haystack {haystack_name:?} references missing session {session_id:?}"
                    )));
                }
            }
            inventories.insert(
                haystack_name.as_str(),
                self.validate_haystack(haystack_name, haystack)?,
            );
        }

        let questions_by_id = self
            .questions
            .iter()
            .map(|question| (question.id.as_str(), question))
            .collect::<HashMap<_, _>>();
        let mut covered_questions = HashSet::new();
        for (name, suite) in &self.suites {
            if suite.schema_version != CORPUS_SCHEMA_VERSION {
                return Err(EvalError::invalid(format!(
                    "suite {name:?} uses unsupported schema {}",
                    suite.schema_version
                )));
            }
            let inventory = inventories.get(suite.haystack.as_str()).ok_or_else(|| {
                EvalError::invalid(format!(
                    "suite {name:?} references missing haystack {:?}",
                    suite.haystack
                ))
            })?;
            if suite.question_ids.is_empty() {
                return Err(EvalError::invalid(format!(
                    "suite {name:?} has no questions"
                )));
            }
            let mut selected_ids = HashSet::new();
            let mut selected_questions = Vec::with_capacity(suite.question_ids.len());
            for question_id in &suite.question_ids {
                if !selected_ids.insert(question_id.as_str()) {
                    return Err(EvalError::invalid(format!(
                        "suite {name:?} repeats question {question_id:?}"
                    )));
                }
                let question = questions_by_id.get(question_id.as_str()).ok_or_else(|| {
                    EvalError::invalid(format!(
                        "suite {name:?} references missing question {question_id:?}"
                    ))
                })?;
                covered_questions.insert(question_id.as_str());
                selected_questions.push(*question);
            }
            validate_suite_questions(name, &selected_questions, inventory)?;
        }
        for question in &self.questions {
            if !covered_questions.contains(question.id.as_str()) {
                return Err(EvalError::invalid(format!(
                    "question {:?} is not assigned to any evaluation suite",
                    question.id
                )));
            }
        }
        Ok(())
    }

    fn sessions_for_haystack(&self, haystack: &HaystackSpec) -> Vec<EvalSession> {
        let by_id = self
            .sessions
            .iter()
            .map(|session| (session.id.as_str(), session))
            .collect::<HashMap<_, _>>();
        let mut sessions = haystack
            .session_ids
            .iter()
            .map(|id| (**by_id.get(id.as_str()).expect("validated haystack session")).clone())
            .collect::<Vec<_>>();
        sessions.extend(generate_filler_sessions(
            self.manifest.seed,
            &haystack.name,
            haystack.filler_count,
        ));
        sessions
    }

    fn validate_haystack(
        &self,
        haystack_name: &str,
        haystack: &HaystackSpec,
    ) -> Result<HaystackInventory, EvalError> {
        let sessions = self.sessions_for_haystack(haystack);
        let mut session_ids = HashSet::new();
        let mut mutation_ids = HashSet::new();
        let mut records = HashMap::<String, MemoryRecord>::new();
        let mut inactive = HashSet::<String>::new();
        for session in &sessions {
            if !session_ids.insert(session.id.as_str()) {
                return Err(EvalError::invalid(format!(
                    "haystack {haystack_name:?} contains duplicate expanded session {:?}",
                    session.id
                )));
            }
            for mutation in &session.mutations {
                mutation.validate().map_err(|error| {
                    EvalError::invalid(format!(
                        "haystack {haystack_name:?} generated invalid mutation: {error}"
                    ))
                })?;
                require_origin(
                    &session.id,
                    match mutation {
                        MemoryMutation::Remember { record, .. } => &record.origin.session_id,
                        MemoryMutation::Forget { origin, .. } => &origin.session_id,
                    },
                    mutation.id(),
                )?;
                if !mutation_ids.insert(mutation.id()) {
                    return Err(EvalError::invalid(format!(
                        "haystack {haystack_name:?} contains duplicate mutation {:?}",
                        mutation.id()
                    )));
                }
                match mutation {
                    MemoryMutation::Remember { record, .. } => {
                        if records.insert(record.id.clone(), record.clone()).is_some() {
                            return Err(EvalError::invalid(format!(
                                "haystack {haystack_name:?} contains duplicate record {:?}",
                                record.id
                            )));
                        }
                        if let Some(target) = &record.supersedes {
                            inactive.insert(target.clone());
                        }
                    }
                    MemoryMutation::Forget { target_id, .. } => {
                        inactive.insert(target_id.clone());
                    }
                }
            }
        }
        for target in &inactive {
            if !records.contains_key(target.as_str()) {
                return Err(EvalError::invalid(format!(
                    "haystack {haystack_name:?} contains an update without target {target:?}"
                )));
            }
        }
        Ok(HaystackInventory { records, inactive })
    }
}

fn validate_suite_questions(
    suite: &str,
    questions: &[&EvalQuestion],
    inventory: &HaystackInventory,
) -> Result<(), EvalError> {
    for question in questions {
        let evidence_ids = question
            .evidence_hops
            .iter()
            .flatten()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        for record_id in &evidence_ids {
            let record = inventory.records.get(*record_id).ok_or_else(|| {
                EvalError::invalid(format!(
                    "question {:?} evidence {record_id:?} is absent from suite {suite:?}",
                    question.id
                ))
            })?;
            if inventory.inactive.contains(*record_id) {
                return Err(EvalError::invalid(format!(
                    "question {:?} uses inactive evidence {record_id:?}",
                    question.id
                )));
            }
            if !question.scopes.contains(&record.scope) {
                return Err(EvalError::invalid(format!(
                    "question {:?} evidence {record_id:?} is outside its scopes",
                    question.id
                )));
            }
        }
        let mut forbidden_ids = HashSet::new();
        for forbidden in &question.forbidden {
            if !forbidden_ids.insert(forbidden.record_id.as_str()) {
                return Err(EvalError::invalid(format!(
                    "question {:?} repeats forbidden record {:?}",
                    question.id, forbidden.record_id
                )));
            }
            if evidence_ids.contains(forbidden.record_id.as_str()) {
                return Err(EvalError::invalid(format!(
                    "question {:?} marks record {:?} as evidence and forbidden",
                    question.id, forbidden.record_id
                )));
            }
            let record = inventory
                .records
                .get(forbidden.record_id.as_str())
                .ok_or_else(|| {
                    EvalError::invalid(format!(
                        "question {:?} forbidden record {:?} is absent from suite {suite:?}",
                        question.id, forbidden.record_id
                    ))
                })?;
            match forbidden.reason {
                ForbiddenReason::WrongScope if question.scopes.contains(&record.scope) => {
                    return Err(EvalError::invalid(format!(
                        "question {:?} wrong-scope record {:?} is visible in its scopes",
                        question.id, forbidden.record_id
                    )));
                }
                ForbiddenReason::Stale
                    if !inventory.inactive.contains(forbidden.record_id.as_str()) =>
                {
                    return Err(EvalError::invalid(format!(
                        "question {:?} stale record {:?} is still active",
                        question.id, forbidden.record_id
                    )));
                }
                ForbiddenReason::Distractor
                    if inventory.inactive.contains(forbidden.record_id.as_str())
                        || !question.scopes.contains(&record.scope) =>
                {
                    return Err(EvalError::invalid(format!(
                        "question {:?} distractor {:?} must be active and visible",
                        question.id, forbidden.record_id
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

impl EvalSuite {
    pub fn corpus_name(&self) -> &str {
        &self.corpus_name
    }

    pub fn corpus_version(&self) -> &str {
        &self.corpus_version
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn name(&self) -> &str {
        &self.suite
    }

    pub fn haystack(&self) -> &str {
        &self.haystack
    }

    pub fn questions(&self) -> &[EvalQuestion] {
        &self.questions
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn mutations(&self) -> Vec<MemoryMutation> {
        self.sessions
            .iter()
            .flat_map(|session| session.mutations.iter().cloned())
            .collect()
    }

    pub(crate) fn record(&self, id: &str) -> Option<&MemoryRecord> {
        self.records.get(id)
    }
}

fn validate_question_shape(question: &EvalQuestion) -> Result<(), EvalError> {
    require_non_empty("question id", &question.id)?;
    require_non_empty("question language", &question.language)?;
    require_non_empty("question query", &question.query)?;
    require_non_empty("question expected answer", &question.expected_answer)?;
    if question.scopes.is_empty() {
        return Err(EvalError::invalid(format!(
            "question {:?} requires at least one scope",
            question.id
        )));
    }
    let mut scope_keys = HashSet::new();
    for scope in &question.scopes {
        if !scope_keys.insert(scope.key()) {
            return Err(EvalError::invalid(format!(
                "question {:?} repeats scope {:?}",
                question.id,
                scope.key()
            )));
        }
    }
    if question.limit == 0 {
        return Err(EvalError::invalid(format!(
            "question {:?} has a zero result limit",
            question.id
        )));
    }
    if question.evidence_hops.is_empty() || question.evidence_hops.iter().any(Vec::is_empty) {
        return Err(EvalError::invalid(format!(
            "question {:?} requires at least one non-empty evidence hop",
            question.id
        )));
    }
    Ok(())
}

fn require_origin(session_id: &str, origin_id: &str, mutation_id: &str) -> Result<(), EvalError> {
    if session_id == origin_id {
        Ok(())
    } else {
        Err(EvalError::invalid(format!(
            "mutation {mutation_id:?} origin {origin_id:?} does not match session {session_id:?}"
        )))
    }
}

fn require_non_empty(label: &str, value: &str) -> Result<(), EvalError> {
    if value.trim().is_empty() {
        Err(EvalError::invalid(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, EvalError> {
    let file = File::open(path).map_err(|error| EvalError::read(path, error))?;
    serde_json::from_reader(BufReader::new(file)).map_err(|source| EvalError::Json {
        path: path.to_path_buf(),
        line: None,
        source,
    })
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, EvalError> {
    let file = File::open(path).map_err(|error| EvalError::read(path, error))?;
    let mut values = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| EvalError::read(path, error))?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(
            serde_json::from_str(&line).map_err(|source| EvalError::Json {
                path: PathBuf::from(path),
                line: Some(index + 1),
                source,
            })?,
        );
    }
    Ok(values)
}
