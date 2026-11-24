//! OpenAI Decisions projection, matching legacy/pi/packages/ai/src/api/openai-decisions.ts.

use std::collections::BTreeMap;

use pi_core::{ClassifierAnswer, ClassifierContext, ClassifierQuestion};
use pi_plugin::ProviderError;
use serde_json::{Value, json};

pub(super) fn payload(model_id: &str, context: &ClassifierContext) -> Result<Value, ProviderError> {
    if context.images.len() > 128 {
        return Err(failure("accepts at most 128 images"));
    }
    let state = Value::Object(context.state.clone()).to_string();
    let input = if context.images.is_empty() {
        Value::String(state)
    } else {
        let mut content = vec![json!({"type":"input_text","text":state})];
        content.extend(context.images.iter().map(|image| {
            json!({"type":"input_image","image_url":format!("data:{};base64,{}",image.mime_type,image.data)})
        }));
        json!([{"role":"user","content":content}])
    };
    let questions: Vec<_> = context
        .questions
        .iter()
        .map(|(name, question)| match question {
            ClassifierQuestion::Choice {
                instructions,
                criteria,
            } => {
                let choices: Vec<_> = criteria
                    .iter()
                    .map(|(value, description)| {
                        if description.is_empty() {
                            json!({"value":value})
                        } else {
                            json!({"value":value,"description":description})
                        }
                    })
                    .collect();
                json!({"type":"choice","name":name,"instructions":instructions,"choices":choices})
            }
            ClassifierQuestion::Score {
                instructions,
                criteria,
            } => {
                let levels: Vec<_> = criteria
                    .iter()
                    .map(|label| json!({"label":label}))
                    .collect();
                json!({"type":"score","name":name,"instructions":instructions,"levels":levels})
            }
            ClassifierQuestion::Bool {
                instructions,
                criteria,
            } => {
                let meanings: Vec<_> = [
                    ("True", &criteria.when_true),
                    ("False", &criteria.when_false),
                ]
                .into_iter()
                .filter(|(_, meaning)| !meaning.is_empty())
                .map(|(label, meaning)| format!("{label} means: {meaning}"))
                .collect();
                let instructions = if meanings.is_empty() {
                    instructions.clone()
                } else {
                    format!("{instructions}\n\n{}", meanings.join("\n"))
                };
                json!({"type":"predicate","name":name,"instructions":instructions})
            }
        })
        .collect();
    Ok(json!({"model":model_id,"input":input,"questions":questions}))
}

pub(super) fn answers(
    body: &Value,
    context: &ClassifierContext,
) -> Result<pi_core::ClassifierAnswers, ProviderError> {
    let raw = body["answers"]
        .as_array()
        .ok_or_else(|| failure("returned an unexpected response"))?;
    // Duplicate names use the final answer, matching Pi's Map projection.
    let by_name: BTreeMap<_, _> = raw
        .iter()
        .filter_map(|answer| answer["name"].as_str().map(|name| (name, answer)))
        .collect();
    context
        .questions
        .iter()
        .map(|(id, question)| {
            let answer = by_name
                .get(id.as_str())
                .ok_or_else(|| failure(format!("did not return an answer for {id}")))?;
            if answer["type"] == "refusal" {
                return Err(failure(format!("refused to answer {id}")));
            }
            let value = match question {
                ClassifierQuestion::Choice { .. } => {
                    if answer["type"] != "choice" {
                        return Err(failure(format!("did not return a choice answer for {id}")));
                    }
                    let choice = answer["choice"].as_str().ok_or_else(|| {
                        failure(format!("did not return a choice answer for {id}"))
                    })?;
                    let probabilities = answer["probabilities"]
                        .as_array()
                        .ok_or_else(|| failure(format!("returned invalid probabilities for {id}")))?
                        .iter()
                        .map(|entry| {
                            let label = entry["value"].as_str().ok_or_else(|| {
                                failure(format!("returned invalid probabilities for {id}"))
                            })?;
                            Ok((
                                label.to_owned(),
                                number(
                                    &entry["probability"],
                                    &format!("probability for {id}.{label}"),
                                )?,
                            ))
                        })
                        .collect::<Result<_, ProviderError>>()?;
                    ClassifierAnswer::Choice {
                        choice: choice.to_owned(),
                        probabilities,
                        confidence: number(&answer["confidence"], &format!("confidence for {id}"))?,
                    }
                }
                ClassifierQuestion::Score { .. } => {
                    if answer["type"] != "score" {
                        return Err(failure(format!("did not return a score answer for {id}")));
                    }
                    ClassifierAnswer::Score {
                        score: number(&answer["score"], &format!("score for {id}"))?,
                        confidence: number(&answer["confidence"], &format!("confidence for {id}"))?,
                    }
                }
                ClassifierQuestion::Bool { .. } => {
                    if answer["type"] != "predicate" {
                        return Err(failure(format!(
                            "did not return a predicate answer for {id}"
                        )));
                    }
                    ClassifierAnswer::Bool {
                        probability: number(
                            &answer["probability"],
                            &format!("probability for {id}"),
                        )?,
                    }
                }
            };
            Ok((id.clone(), value))
        })
        .collect()
}

fn number(value: &Value, field: &str) -> Result<f64, ProviderError> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| failure(format!("returned invalid {field}")))
}

fn failure(message: impl std::fmt::Display) -> ProviderError {
    ProviderError::Failure(format!("OpenAI Decisions {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ClassifierContext {
        serde_json::from_value(json!({
            "state":{"task":"review"},
            "questions":{
                "approve":{"type":"bool","instructions":"Approve?","criteria":{"true":"allowed","false":"blocked"}},
                "kind":{"type":"choice","instructions":"Choose","criteria":{"a":"first","b":""}},
                "rank":{"type":"score","instructions":"Rank","criteria":["low","high"]}
            }
        })).unwrap()
    }

    #[test]
    fn projects_all_question_types_and_multimodal_input() {
        let mut context = context();
        let body = payload("classifier", &context).unwrap();
        assert_eq!(body["input"], r#"{"task":"review"}"#);
        assert_eq!(
            body["questions"][0],
            json!({"type":"predicate","name":"approve","instructions":"Approve?\n\nTrue means: allowed\nFalse means: blocked"})
        );
        assert_eq!(
            body["questions"][1]["choices"],
            json!([{"value":"a","description":"first"},{"value":"b"}])
        );
        assert_eq!(
            body["questions"][2]["levels"],
            json!([{"label":"low"},{"label":"high"}])
        );
        context.images.push(pi_core::ImageContent {
            data: "aGVsbG8=".into(),
            mime_type: "image/png".into(),
        });
        let body = payload("classifier", &context).unwrap();
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(
            body["input"][0]["content"][1]["image_url"],
            "data:image/png;base64,aGVsbG8="
        );
        context.images.resize(129, context.images[0].clone());
        assert!(payload("classifier", &context).is_err());
    }

    #[test]
    fn parses_named_answers_and_rejects_refusals_missing_or_malformed_answers() {
        let context = context();
        let mut body = json!({"answers":[
            {"name":"rank","type":"score","score":1,"confidence":0.8},
            {"name":"approve","type":"predicate","probability":0.9},
            {"name":"kind","type":"choice","choice":"a","confidence":0.7,"probabilities":[{"value":"a","probability":0.7},{"value":"b","probability":0.3}]}
        ]});
        let values = answers(&body, &context).unwrap();
        assert_eq!(
            values["approve"],
            ClassifierAnswer::Bool { probability: 0.9 }
        );
        assert!(
            matches!(&values["kind"], ClassifierAnswer::Choice { choice, probabilities, .. } if choice == "a" && probabilities["b"] == 0.3)
        );
        assert_eq!(
            values["rank"],
            ClassifierAnswer::Score {
                score: 1.0,
                confidence: 0.8
            }
        );
        body["answers"][1]["type"] = json!("refusal");
        assert!(
            answers(&body, &context)
                .unwrap_err()
                .to_string()
                .contains("refused")
        );
        body["answers"][1]["type"] = json!("predicate");
        body["answers"][1]["probability"] = json!("0.9");
        assert!(answers(&body, &context).is_err());
        assert!(answers(&json!({"answers":[]}), &context).is_err());
    }
}
