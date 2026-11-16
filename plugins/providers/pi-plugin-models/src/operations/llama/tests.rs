use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

#[derive(Clone, Copy)]
enum Readout {
    All,
    Deeper,
    Missing,
    Underflow,
}

struct FakeServer {
    calls: Mutex<Vec<(String, Value, bool)>>,
    readout: Readout,
    split_words: bool,
    fail_token_once: AtomicBool,
}

impl FakeServer {
    fn new(readout: Readout) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            readout,
            split_words: false,
            fail_token_once: AtomicBool::new(false),
        }
    }

    fn post(
        &self,
        url: String,
        payload: Value,
        observe: bool,
    ) -> BoxFuture<'static, Result<Value, ProviderError>> {
        assert_eq!(payload["model"], "qwen");
        self.calls
            .lock()
            .unwrap()
            .push((url.clone(), payload.clone(), observe));
        let result = if url.ends_with("/tokenize") {
            assert!(!observe);
            assert_eq!(payload["add_special"], false);
            assert_eq!(payload["parse_special"], false);
            if self.fail_token_once.swap(false, Ordering::SeqCst) {
                Err(ProviderError::Failure("retry me".into()))
            } else {
                let content = payload["content"].as_str().unwrap();
                let content = if self.split_words {
                    content.to_owned()
                } else {
                    content.replace("Yes", "Y").replace("No", "N")
                };
                // Use object token IDs as some llama-server versions do.
                Ok(
                    json!({"tokens": content.chars().map(|character| json!({"id": character as u32})).collect::<Vec<_>>()}),
                )
            }
        } else if url.ends_with("/apply-template") {
            assert!(!observe);
            assert_eq!(payload["chat_template_kwargs"]["enable_thinking"], false);
            assert_eq!(payload["messages"][0]["content"], SYSTEM_PROMPT);
            Ok(
                json!({"prompt": format!("{}\n<think>", payload["messages"][1]["content"].as_str().unwrap())}),
            )
        } else if url.ends_with("/completion") {
            assert!(observe);
            assert_eq!(payload["n_predict"], 1);
            assert_eq!(payload["post_sampling_probs"], false);
            assert_eq!(payload["cache_prompt"], true);
            assert_eq!(payload["temperature"], 0);
            let prompt = payload["prompt"].as_str().unwrap();
            assert!(prompt.ends_with("<think></think>"));
            let labels = if prompt.ends_with("Answer Yes or No.\n<think></think>") {
                vec![89, 78]
            } else if prompt.ends_with("Answer with one level number.\n<think></think>") {
                vec![48, 49, 50]
            } else {
                vec![65, 66]
            };
            let depth = payload["n_probs"].as_u64().unwrap();
            let missing = matches!(self.readout, Readout::Missing)
                || (matches!(self.readout, Readout::Deeper) && depth < 4096);
            let entries = labels.into_iter().enumerate().filter(|(index, _)| !missing || *index == 0).map(|(index, id)| {
                json!({"id": id, "logprob": if matches!(self.readout, Readout::Underflow) { -1e31 } else { -(index as f64) }})
            }).collect::<Vec<_>>();
            Ok(json!({"completion_probabilities": [{"top_logprobs": entries}]}))
        } else {
            panic!("unexpected llama-server path: {url}")
        };
        Box::pin(async move { result })
    }

    fn completion_depths(&self) -> Vec<u64> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(url, _, _)| url.ends_with("/completion"))
            .map(|(_, payload, _)| payload["n_probs"].as_u64().unwrap())
            .collect()
    }
}

fn model() -> ClassifierModelSpec {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let mut model = ClassifierModelSpec::new("llama.cpp", "qwen", "Qwen", "llama-cpp-classify");
    model.base_url = Some(format!(
        "http://llama-{}.test/v1/",
        COUNT.fetch_add(1, Ordering::SeqCst)
    ));
    model
}

fn context() -> ClassifierContext {
    serde_json::from_value(json!({"state": {"message": "Help"}, "questions": {
        "team": {"type": "choice", "instructions": "Choose team", "criteria": {"billing": "Payments", "technical": "Bugs"}},
        "urgent": {"type": "bool", "instructions": "Urgent?", "criteria": {"true": "Help soon", "false": "No hurry"}},
        "severity": {"type": "score", "instructions": "How severe?", "criteria": ["low", "medium", "high"]}
    }})).unwrap()
}

fn choice_context() -> ClassifierContext {
    let mut context = context();
    context.questions.retain(|id, _| id == "team");
    context
}

#[tokio::test]
async fn protocol_maps_choice_bool_and_score_with_template_and_label_cache() {
    let model = model();
    let context = context();
    let server = FakeServer::new(Readout::All);
    let answers = classify(&model, &context, |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap();
    let p0 = 1.0 / (1.0 + (-1_f64).exp());
    let ClassifierAnswer::Choice {
        choice,
        probabilities,
        confidence,
    } = &answers["team"]
    else {
        panic!("missing choice")
    };
    assert_eq!(choice, "billing");
    assert!((probabilities["billing"] - p0).abs() < 1e-12);
    assert!((*confidence - (2.0 * p0 - 1.0)).abs() < 1e-12);
    let ClassifierAnswer::Bool { probability } = answers["urgent"] else {
        panic!("missing bool")
    };
    assert!((probability - p0).abs() < 1e-12);
    let ClassifierAnswer::Score { score, .. } = answers["severity"] else {
        panic!("missing score")
    };
    let expected =
        ((-1_f64).exp() + 2.0 * (-2_f64).exp()) / (1.0 + (-1_f64).exp() + (-2_f64).exp());
    assert!((score - expected).abs() < 1e-12);
    let token_count = || {
        server
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(url, _, _)| url.ends_with("/tokenize"))
            .count()
    };
    let first_tokens = token_count();
    assert!(first_tokens > 0);
    classify(&model, &context, |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap();
    assert_eq!(token_count(), first_tokens);
    for (url, _, _) in &*server.calls.lock().unwrap() {
        assert!(!url.contains("/v1/"));
    }
}

#[tokio::test]
async fn missing_labels_escalate_readout_instead_of_inventing_zero_probabilities() {
    let server = FakeServer::new(Readout::Deeper);
    classify(&model(), &choice_context(), |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap();
    assert_eq!(server.completion_depths(), [256, 4096]);
    let server = FakeServer::new(Readout::Missing);
    let error = classify(&model(), &choice_context(), |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not rank labels B for team within the top 32768 tokens")
    );
    assert_eq!(server.completion_depths(), [256, 4096, 32768]);
    let server = FakeServer::new(Readout::Underflow);
    let error = classify(&model(), &choice_context(), |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no probability to any answer label")
    );
}

#[tokio::test]
async fn all_questions_are_validated_before_transport_and_failed_tokens_retry() {
    let server = FakeServer::new(Readout::All);
    let mut context = choice_context();
    context.questions.insert(
        "z-invalid".into(),
        ClassifierQuestion::Score {
            instructions: "Rate".into(),
            criteria: vec!["only".into()],
        },
    );
    assert!(
        classify(&model(), &context, |url, value, observe| server
            .post(url, value, observe))
        .await
        .unwrap_err()
        .to_string()
        .contains("2 to 10 levels")
    );
    assert!(server.calls.lock().unwrap().is_empty());
    let classifier_model = model();
    server.fail_token_once.store(true, Ordering::SeqCst);
    assert!(
        classify(
            &classifier_model,
            &choice_context(),
            |url, value, observe| server.post(url, value, observe)
        )
        .await
        .is_err()
    );
    classify(
        &classifier_model,
        &choice_context(),
        |url, value, observe| server.post(url, value, observe),
    )
    .await
    .unwrap();
    let mut server = FakeServer::new(Readout::All);
    server.split_words = true;
    let error = classify(&model(), &context_with_bool(), |url, value, observe| {
        server.post(url, value, observe)
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("is not a single token"));
}

fn context_with_bool() -> ClassifierContext {
    let mut context = context();
    context.questions.retain(|id, _| id == "urgent");
    context
}

#[test]
fn repeated_state_and_overview_share_prompt_prefixes() {
    let context = context();
    let prompts = context
        .questions
        .values()
        .map(|question| render_question(&context, question).unwrap().content)
        .collect::<Vec<_>>();
    let prefix = |prompt: &str| prompt[..prompt.rfind("Question:").unwrap()].to_owned();
    assert!(
        prompts
            .windows(2)
            .all(|pair| prefix(&pair[0]) == prefix(&pair[1]))
    );
    assert_eq!(
        prompts[0]
            .matches("State:\n{\n \"message\": \"Help\"\n}")
            .count(),
        2
    );
}
