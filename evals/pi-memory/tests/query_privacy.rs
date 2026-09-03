use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_memory_eval::{
    EvalBackend, EvalBackendError, EvalCorpus, EvalInput, EvalObservation, EvalRunner, RunnerConfig,
};

#[derive(Clone, Default)]
struct RecordingBackend {
    inputs: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl EvalBackend for RecordingBackend {
    async fn gather(&self, input: EvalInput) -> Result<EvalObservation, EvalBackendError> {
        self.inputs
            .lock()
            .expect("recording lock")
            .push(serde_json::to_value(input).expect("serializable input"));
        Ok(EvalObservation::default())
    }
}

#[tokio::test]
async fn backend_input_cannot_contain_gold_metadata() {
    let corpus = EvalCorpus::bundled().expect("bundled corpus");
    let suite = corpus.suite("smoke").expect("smoke suite");
    let backend = RecordingBackend::default();
    let runner = EvalRunner::new(RunnerConfig {
        timeout: Duration::from_millis(500),
    });

    let report = runner.run("recording", &suite, &backend).await;
    assert_eq!(report.summary.completed, suite.questions().len());
    let inputs = backend.inputs.lock().expect("recorded inputs");
    assert_eq!(inputs.len(), suite.questions().len());
    for input in inputs.iter() {
        let mut keys = input
            .as_object()
            .expect("input object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, ["limit", "query", "scopes"]);
        let serialized = input.to_string();
        for forbidden in [
            "questionId",
            "ability",
            "languageRelation",
            "evidenceHops",
            "forbidden",
            "expectedAnswer",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }
}
