use super::invalid;
use pi_core::{ClassifierAnswer, ClassifierContext, ClassifierModelSpec, ClassifierQuestion};
use pi_plugin::ProviderError;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn payload(
    model: &ClassifierModelSpec,
    input: &ClassifierContext,
) -> Result<(Value, bool), ProviderError> {
    let mut questions =
        serde_json::to_value(&input.questions).map_err(|e| invalid(e.to_string()))?;
    for question in questions
        .as_object_mut()
        .expect("typed questions serialize as object")
        .values_mut()
    {
        if question["type"] == "bool" {
            question["type"] = json!("noul");
        }
    }
    let mut request = json!({"state":input.state,"questions":questions});
    let cloudflare = model.api == "cloudflare-workers-ai-system-one";
    let openrouter = model.provider.as_str() == "openrouter"
        || model
            .base_url
            .as_deref()
            .is_some_and(|url| url.contains("openrouter.ai"));
    let mode = model
        .compat
        .as_ref()
        .and_then(|v| v.get("imageInput"))
        .and_then(Value::as_str)
        .unwrap_or(
            if cloudflare && model.id.as_str().starts_with("@cf/cloudflare/clef") {
                "images"
            } else if openrouter {
                "state-image-urls"
            } else {
                "assets"
            },
        );
    let opt_in = !input.images.is_empty()
        && model
            .compat
            .as_ref()
            .and_then(|v| v.get("sendImageOptInHeader"))
            .and_then(Value::as_bool)
            .unwrap_or(!cloudflare && !openrouter);
    if !input.images.is_empty() {
        match mode {
            "images" => {
                if input.images.len() > 4 {
                    return Err(invalid("System One accepts at most 4 images"));
                }
                if input.images.iter().any(|i| {
                    !matches!(
                        i.mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp"
                    )
                }) {
                    return Err(invalid("System One accepts PNG, JPEG, and WebP images"));
                }
                request["images"] = json!(
                    input
                        .images
                        .iter()
                        .map(|i| format!("data:{};base64,{}", i.mime_type, i.data))
                        .collect::<Vec<_>>()
                );
            }
            "state-image-urls" => {
                let mut state=input.images.iter().map(|i| json!({"type":"image_url","image_url":{"url":format!("data:{};base64,{}",i.mime_type,i.data)}})).collect::<Vec<_>>();
                state.push(json!(input.state));
                request["state"] = json!(state);
            }
            "assets" => {
                let mut assets = serde_json::Map::new();
                let mut state = vec![];
                for (index, image) in input.images.iter().enumerate() {
                    let format = match image.mime_type.as_str() {
                        "image/png" => "png",
                        "image/jpeg" => "jpeg",
                        _ => return Err(invalid("System One accepts PNG and JPEG images")),
                    };
                    let name = format!("<image-{}>", index + 1);
                    state.push(json!(name));
                    assets.insert(
                        name,
                        json!({"type":"image","format":format,"data_base64":image.data}),
                    );
                }
                state.push(json!(input.state));
                request["state"] = json!(state);
                request["assets"] = json!(assets);
            }
            _ => return Err(invalid("unsupported System One imageInput")),
        }
    }
    let body = if cloudflare {
        json!({"model":model.id,"input":request})
    } else {
        request["model"] = json!(model.id);
        request
    };
    Ok((body, opt_in))
}

pub(super) fn cloudflare_output(body: &Value) -> Result<&Value, ProviderError> {
    if body.get("success") == Some(&Value::Bool(false)) {
        let errors = body
            .get("errors")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        return Err(invalid(format!(
            "Cloudflare Workers AI request failed: {errors}"
        )));
    }
    let result = body
        .get("result")
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("Cloudflare Workers AI returned an unexpected response"))?;
    if result.get("answers").is_some() {
        return Ok(result);
    }
    if result.get("state").and_then(Value::as_str) != Some("Completed") {
        return Err(invalid(format!(
            "Cloudflare Workers AI run did not complete (state: {})",
            result["state"]
        )));
    }
    result
        .get("result")
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("Cloudflare Workers AI returned an unexpected response"))
}

pub(super) fn answers(
    body: &Value,
    context: &ClassifierContext,
) -> Result<BTreeMap<String, ClassifierAnswer>, ProviderError> {
    let answers = body
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("System One returned an unexpected response"))?;
    context
        .questions
        .iter()
        .map(|(id, question)| {
            let mut answer = answers
                .get(id)
                .filter(|a| a.is_object())
                .cloned()
                .ok_or_else(|| invalid(format!("System One did not return an answer for {id}")))?;
            let expected = match question {
                ClassifierQuestion::Choice { .. } => "choice",
                ClassifierQuestion::Score { .. } => "score",
                ClassifierQuestion::Bool { .. } => "noul",
            };
            if answer["type"] != expected {
                return Err(invalid(format!(
                    "System One returned an unexpected answer type for {id}"
                )));
            }
            if expected == "noul" {
                answer = json!({"type":"bool","probability":answer["noul"]});
            }
            serde_json::from_value(answer)
                .map(|answer| (id.clone(), answer))
                .map_err(|e| {
                    invalid(format!(
                        "System One returned an invalid answer for {id}: {e}"
                    ))
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::ImageContent;

    fn context() -> ClassifierContext {
        serde_json::from_value(json!({"state":{"task":"review"},"questions":{"approve":{"type":"bool","instructions":"Approve?","criteria":{"true":"yes","false":"no"}}}})).unwrap()
    }

    #[test]
    fn system_one_images_follow_native_and_proxy_compatibility() {
        let mut model =
            ClassifierModelSpec::new("typesafe", "jev-latest", "Jev", "typesafe-system-one");
        let mut input = context();
        input.images = vec![ImageContent {
            data: "cG5n".into(),
            mime_type: "image/png".into(),
        }];
        let (body, opt_in) = payload(&model, &input).unwrap();
        assert!(opt_in);
        assert_eq!(body["state"], json!(["<image-1>",{"task":"review"}]));
        assert_eq!(body["assets"]["<image-1>"]["data_base64"], "cG5n");
        assert_eq!(body["questions"]["approve"]["type"], "noul");
        model.provider = "openrouter".into();
        let (body, opt_in) = payload(&model, &input).unwrap();
        assert!(!opt_in);
        assert_eq!(
            body["state"][0]["image_url"]["url"],
            "data:image/png;base64,cG5n"
        );
        assert!(body.get("assets").is_none());
        model.compat = Some(json!({"imageInput":"assets","sendImageOptInHeader":true}));
        assert!(payload(&model, &input).unwrap().1);
    }

    #[test]
    fn cloudflare_clef_images_and_run_envelopes_match_pi() {
        let model = ClassifierModelSpec::new(
            "cloudflare-workers-ai",
            "@cf/cloudflare/clef",
            "Clef",
            "cloudflare-workers-ai-system-one",
        );
        let mut input = context();
        input.images = vec![ImageContent {
            data: "d2VicA==".into(),
            mime_type: "image/webp".into(),
        }];
        let (body, opt_in) = payload(&model, &input).unwrap();
        assert!(!opt_in);
        assert_eq!(
            body["input"]["images"],
            json!(["data:image/webp;base64,d2VicA=="])
        );
        let output = json!({"answers":{"approve":{"type":"noul","noul":0.8}}});
        for body in [
            json!({"success":true,"result":output}),
            json!({"success":true,"result":{"state":"Completed","result":output}}),
        ] {
            let parsed = answers(cloudflare_output(&body).unwrap(), &input).unwrap();
            assert_eq!(
                parsed["approve"],
                ClassifierAnswer::Bool { probability: 0.8 }
            );
        }
        assert!(cloudflare_output(&json!({"success":true,"result":{"state":"Running"}})).is_err());
        input.images.resize(5, input.images[0].clone());
        assert!(payload(&model, &input).is_err());
    }
}
