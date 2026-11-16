use pi_core::{ContentBlock, ImageContent, ImageModelSpec, ImagesContext, ModelCost, Usage};
use serde_json::{Value, json};

pub(super) fn payload(model: &ImageModelSpec, input: &ImagesContext) -> Value {
    let content=input.input.iter().filter_map(|block| match block {
        ContentBlock::Text(text) => Some(json!({"type":"text","text":text.text})),
        ContentBlock::Image(image) => Some(json!({"type":"image_url","image_url":{"url":format!("data:{};base64,{}",image.mime_type,image.data)}})),
        _ => None,
    }).collect::<Vec<_>>();
    json!({"model":model.id,"messages":[{"role":"user","content":content}],"stream":false,"modalities":if model.output.contains(&pi_core::ModelInput::Text) { vec!["image","text"] } else { vec!["image"] }})
}
pub(super) fn content(response: &Value) -> Vec<ContentBlock> {
    let message = &response["choices"][0]["message"];
    let mut output = vec![];
    if let Some(text) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        output.push(ContentBlock::Text(pi_core::TextContent {
            text: text.into(),
            text_signature: None,
        }));
    }
    for image in message
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(url) = image["image_url"]
            .as_str()
            .or_else(|| image["image_url"]["url"].as_str())
        else {
            continue;
        };
        let Some((mime, data)) = url
            .strip_prefix("data:")
            .and_then(|data| data.split_once(";base64,"))
            .filter(|(mime, data)| !mime.is_empty() && !data.is_empty())
        else {
            continue;
        };
        output.push(ContentBlock::Image(ImageContent {
            data: data.into(),
            mime_type: mime.into(),
        }));
    }
    output
}
pub(super) fn usage(value: Option<&Value>, cost: &ModelCost) -> Option<Usage> {
    let value = value?.as_object()?;
    let prompt = super::count(value.get("prompt_tokens"));
    let output = super::count(value.get("completion_tokens"));
    let details = value.get("prompt_tokens_details");
    let cache_write = super::count(details.and_then(|d| d.get("cache_write_tokens")));
    let cache_read =
        super::count(details.and_then(|d| d.get("cached_tokens"))).saturating_sub(cache_write);
    let input = prompt
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
        ..Usage::default()
    };
    usage.cost = cost.calculate(&usage);
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_extract_only_inline_content_and_price_cache_usage() {
        let response = json!({"choices":[{"message":{"content":"done","images":[
            {"image_url":{"url":"data:image/png;base64,cG5n"}},
            {"image_url":"https://example.test/remote.png"},
            {"image_url":"data:image/jpeg;base64,anBlZw=="}
        ]}}]});
        let output = content(&response);
        assert_eq!(output.len(), 3);
        assert!(matches!(&output[1],ContentBlock::Image(i) if i.mime_type=="image/png"));
        let cost = ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.2,
            tiers: vec![],
        };
        let usage=usage(Some(&json!({"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":40,"cache_write_tokens":10}})),&cost).unwrap();
        assert_eq!(
            (
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                usage.total_tokens
            ),
            (60, 20, 30, 10, 120)
        );
        assert!((usage.cost.total - 0.000105).abs() < 1e-12);
    }
}
