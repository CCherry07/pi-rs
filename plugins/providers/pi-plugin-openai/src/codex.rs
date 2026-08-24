use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::responses::{input_items, stream as responses_stream, tools as response_tools};
use async_trait::async_trait;
use base64::Engine;
use pi_core::{
    AbortSignal, Provider, ProviderAvailability, ProviderCallContext, ProviderError, ProviderId,
    ProviderRequest, ProviderStream,
};
use pi_provider::{HttpTransport, ReqwestTransport, TransportError, collect_body_limited};
use serde_json::{Value, json};

const PROVIDER_ID: &str = "openai-codex";
const API_NAME: &str = "openai-codex-responses";
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

#[derive(Debug, Clone, Default)]
pub struct CodexCredentials {
    access_token: Option<String>,
    account_id: Option<String>,
    source: Option<PathBuf>,
}

impl CodexCredentials {
    pub fn discover() -> Self {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Self::default();
        };
        Self::discover_from([
            home.join(".codex/auth.json"),
            home.join(".config/codex/auth.json"),
        ])
    }

    pub fn discover_from(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        paths
            .into_iter()
            .find_map(|path| Self::read(&path))
            .unwrap_or_default()
    }

    fn read(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let access_token = token_candidate(&value, &["tokens", "access_token"])
            .or_else(|| token_candidate(&value, &["tokens", "accessToken"]))
            .or_else(|| token_candidate(&value, &["access_token"]))
            .or_else(|| token_candidate(&value, &["accessToken"]))?;
        let account_id = chatgpt_account_id(&access_token)?;
        Some(Self {
            access_token: Some(access_token),
            account_id: Some(account_id),
            source: Some(path.to_path_buf()),
        })
    }

    pub fn from_access_token(access_token: impl Into<String>) -> Self {
        let access_token = access_token.into();
        let account_id = chatgpt_account_id(&access_token);
        Self {
            access_token: Some(access_token),
            account_id,
            source: None,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.access_token.is_some() && self.account_id.is_some()
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

fn token_candidate(value: &Value, path: &[&str]) -> Option<String> {
    let token = path
        .iter()
        .try_fold(value, |value, key| value.get(*key))?
        .as_str()?
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .or_else(|| claims.get("chatgpt_account_id"))
        .or_else(|| claims.pointer("/https://api.openai.com/auth/chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub struct OpenAiCodexProvider {
    credentials: CodexCredentials,
    transport: Arc<dyn HttpTransport>,
}

impl OpenAiCodexProvider {
    pub fn discover() -> Self {
        Self::new(CodexCredentials::discover())
    }

    pub fn new(credentials: CodexCredentials) -> Self {
        Self::with_transport(credentials, Arc::new(ReqwestTransport::new()))
    }

    pub fn with_transport(
        credentials: CodexCredentials,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            credentials,
            transport,
        }
    }

    fn headers(
        &self,
        request: &ProviderRequest,
    ) -> Result<BTreeMap<String, String>, ProviderError> {
        let token = self.credentials.access_token.as_deref().ok_or_else(|| {
            ProviderError::Failure(
                "OpenAI Codex requires a login in ~/.codex/auth.json".to_string(),
            )
        })?;
        let account_id = self.credentials.account_id.as_deref().ok_or_else(|| {
            ProviderError::Failure(
                "OpenAI Codex token is missing chatgpt_account_id; run codex login again"
                    .to_string(),
            )
        })?;
        let mut headers = BTreeMap::from([
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {token}")),
            ("chatgpt-account-id".to_string(), account_id.to_string()),
            (
                "OpenAI-Beta".to_string(),
                "responses=experimental".to_string(),
            ),
            ("originator".to_string(), "pi".to_string()),
            ("User-Agent".to_string(), "pi_rs".to_string()),
        ]);
        for (name, value) in &request.headers {
            insert_header(&mut headers, name, value);
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for OpenAiCodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn availability(&self) -> ProviderAvailability {
        if self.credentials.is_configured() {
            ProviderAvailability::Available
        } else {
            ProviderAvailability::MissingCredentials
        }
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        context: ProviderCallContext,
        signal: AbortSignal,
    ) -> Result<ProviderStream, ProviderError> {
        let headers = self.headers(&request)?;
        let payload = context
            .before_provider_request(&signal, responses_request_body(&request))
            .await?;
        let response = self
            .transport
            .post_json(CODEX_RESPONSES_URL, &headers, &payload, signal.clone())
            .await
            .map_err(map_transport_error)?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let body = collect_body_limited(response.body, 64 * 1024)
                .await
                .map_err(map_transport_error)?;
            return Err(ProviderError::Failure(format!("HTTP {status}: {body}")));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().contains("text/event-stream"))
        {
            return Err(ProviderError::Protocol(format!(
                "unexpected Content-Type {:?}; expected text/event-stream",
                response.content_type.as_deref().unwrap_or("<missing>")
            )));
        }

        Ok(responses_stream(
            ProviderId::new(PROVIDER_ID),
            request.model,
            API_NAME,
            response.body,
            signal,
        ))
    }
}

pub(crate) fn responses_request_body(request: &ProviderRequest) -> Value {
    let input = input_items(&request.messages);
    let tools = response_tools(&request.tools);
    let effort = match request.thinking_level {
        pi_core::ThinkingLevel::Off => "low",
        level => level.as_str(),
    };
    let mut body = json!({
        "model": request.model.as_str(),
        "input": input,
        "instructions": request.system_prompt,
        "stream": true,
        "store": false,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "text": {"verbosity": "medium"},
        "include": ["reasoning.encrypted_content"],
        "reasoning": {"effort": effort, "summary": "auto"}
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Value::Object(body) = &mut body {
        body.extend(request.sampling_params.clone());
    }
    body
}

fn insert_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.to_string());
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Aborted => ProviderError::Aborted,
        other => ProviderError::Failure(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{ContentBlock, Message};

    use super::*;

    fn jwt(account_id: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({"https://api.openai.com/auth": {"chatgpt_account_id": account_id}}).to_string(),
        );
        format!("header.{payload}.signature")
    }

    #[test]
    fn discovers_codex_cli_token_and_account_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        std::fs::write(
            &path,
            json!({"tokens": {"access_token": jwt("acct-1")}}).to_string(),
        )
        .unwrap();

        let credentials = CodexCredentials::discover_from([path.clone()]);

        assert!(credentials.is_configured());
        assert_eq!(credentials.account_id.as_deref(), Some("acct-1"));
        assert_eq!(credentials.source(), Some(path.as_path()));
    }

    #[test]
    fn builds_codex_responses_payload() {
        let request = ProviderRequest {
            model: pi_core::ModelId::new("gpt-5.5"),
            model_spec: None,
            system_prompt: "system".to_string(),
            messages: vec![Message::User(pi_core::UserMessage {
                content: vec![ContentBlock::Text(pi_core::TextContent::new("hello"))],
                timestamp_ms: 0,
            })],
            tools: Vec::new(),
            thinking_level: pi_core::ThinkingLevel::High,
            max_output_tokens: Some(123),
            headers: BTreeMap::new(),
            sampling_params: BTreeMap::new(),
            session_id: None,
        };
        let body = responses_request_body(&request);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("max_output_tokens").is_none());
    }
}
