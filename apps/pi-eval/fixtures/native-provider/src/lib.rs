use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::stream;
use pi_plugin::prelude::*;

const PROVIDER_ID: &str = "eval-custom";
const MODEL_ID: &str = "eval-custom-chat";
const ERROR_SYSTEM_PROMPT: &str = "pi-eval:intentional-error";
const ERROR_MESSAGE: &str = "intentional native provider fixture failure";

#[derive(Default)]
pub struct EvalNativeProviderPlugin;

#[pi_plugin::native_provider]
impl ProviderPlugin for EvalNativeProviderPlugin {
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> Result<()> {
        context.register_provider(Arc::new(EvalNativeProvider))?;
        let mut model = ModelSpec::new(
            PROVIDER_ID,
            MODEL_ID,
            "Eval Custom Chat",
            "eval-custom-stream",
        );
        model.context_window = 16_384;
        model.max_tokens = 2_048;
        context.register_model(model)
    }
}

struct EvalNativeProvider;

#[async_trait]
impl Provider for EvalNativeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn name(&self) -> String {
        "Eval Custom Provider".to_string()
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        _context: ProviderCallContext,
        signal: AbortSignal,
    ) -> std::result::Result<ProviderStream, ProviderError> {
        signal.check().map_err(|_| ProviderError::Aborted)?;
        if request.model != ModelId::new(MODEL_ID) {
            return Err(ProviderError::Protocol(format!(
                "unsupported fixture model: {}",
                request.model
            )));
        }

        let metadata = ResponseMetadata::new(
            ProviderId::new(PROVIDER_ID),
            ModelId::new(MODEL_ID),
            "eval-custom-stream",
            now_ms(),
        );
        if request.system_prompt == ERROR_SYSTEM_PROMPT {
            let events = vec![
                Ok(StreamEvent::Start { metadata }),
                Ok(StreamEvent::TextStart { content_index: 0 }),
                Ok(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: "discarded-partial".to_string(),
                }),
                Err(ProviderError::Protocol(ERROR_MESSAGE.to_string())),
            ];
            return Ok(Box::pin(stream::iter(events)));
        }

        let events = vec![
            StreamEvent::Start { metadata },
            StreamEvent::TextStart { content_index: 0 },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: "NATIVE_".to_string(),
            },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: "PROVIDER_OK".to_string(),
            },
            StreamEvent::TextEnd {
                content_index: 0,
                text_signature: None,
            },
            StreamEvent::Done {
                reason: StopReason::Stop,
                usage: Usage {
                    input: 4,
                    output: 3,
                    total_tokens: 7,
                    ..Usage::default()
                },
            },
        ];
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}
