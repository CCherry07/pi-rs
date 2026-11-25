use super::*;
use axum::response::IntoResponse;
use pi_plugin_mcp::McpLibrary;
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Prompt {
    url: Mutex<Option<String>>,
    notices: Mutex<Vec<String>>,
    answer: &'static str,
}
#[async_trait]
impl McpOAuthPrompt for Prompt {
    fn show_authorization_url(&self, url: &str) -> Result<(), String> {
        *self.url.lock().unwrap() = Some(url.into());
        Ok(())
    }
    fn notify(&self, message: &str) -> Result<(), String> {
        self.notices.lock().unwrap().push(message.into());
        Ok(())
    }
    async fn redirect_url(&self, signal: AbortSignal) -> Result<Option<String>, String> {
        match self.answer {
            "paste" => Ok(Some(callback(
                self.url.lock().unwrap().as_ref().unwrap(),
                false,
            ))),
            "cancel" => Ok(None),
            _ => {
                signal.wait().await;
                Ok(None)
            }
        }
    }
}
fn callback(authorization: &str, wrong_state: bool) -> String {
    let url = Url::parse(authorization).unwrap();
    let params = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let mut callback = Url::parse(&params["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("code", "fixture-code")
        .append_pair(
            "state",
            if wrong_state {
                "wrong-state"
            } else {
                &params["state"]
            },
        );
    callback.into()
}
struct Browser {
    automatic: bool,
    fail: bool,
    opens: AtomicUsize,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}
#[async_trait]
impl BrowserOpener for Browser {
    async fn open(&self, url: &str) -> Result<(), ()> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(());
        }
        if self.automatic {
            let invalid = callback(url, true);
            let valid = callback(url, false);
            self.tasks.lock().unwrap().push(tokio::spawn(async move {
                let client = reqwest::Client::new();
                assert_eq!(client.get(invalid).send().await.unwrap().status(), 400);
                assert_eq!(client.get(valid).send().await.unwrap().status(), 200);
            }));
        }
        Ok(())
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    library: McpLibrary,
    prompt: Arc<Prompt>,
    browser: Arc<Browser>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(
        answer: &'static str,
        automatic: bool,
        fail_browser: bool,
        callback_url: Option<String>,
        use_challenge: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let metadata = json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"code_challenge_methods_supported":["S256"]});
        let resource = json!({"resource":format!("{origin}/mcp"),"authorization_servers":[origin]});
        let challenge = format!("Bearer resource_metadata=\"{origin}/protected-resource\"");
        let app = axum::Router::new()
            .route("/.well-known/oauth-authorization-server", axum::routing::get({let metadata = metadata.clone(); move || { let metadata = metadata.clone(); async move { axum::Json(metadata) } }}))
            .route("/metadata", axum::routing::get(move || { let metadata = metadata.clone(); async move { axum::Json(metadata) } }))
            .route("/protected-resource", axum::routing::get(move || { let resource = resource.clone(); async move { axum::Json(resource) } }))
            .route("/mcp", axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<Value>| {
                let challenge = challenge.clone();
                async move {
                    if headers.get("authorization").and_then(|header| header.to_str().ok()) != Some("Bearer fixture-access") {
                        return (axum::http::StatusCode::UNAUTHORIZED, [("www-authenticate",challenge)]).into_response();
                    }
                    if request["id"].is_null() { return axum::http::StatusCode::ACCEPTED.into_response(); }
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
                        "tools/list" => json!({"tools":[{"name":"whoami","description":"Signed in identity","inputSchema":{"type":"object"}}]}),
                        _ => return axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Use initialize"}})).into_response(),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
                }
            }))
            .route("/token", axum::routing::post(|body: String| async move {
                assert!(body.contains("code=fixture-code")); assert!(body.contains("code_verifier="));
                axum::Json(json!({"access_token":"fixture-access","token_type":"Bearer","expires_in":3600}))
            }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut oauth = json!({"clientId":"fixture-client"});
        if !use_challenge {
            oauth["authServerMetadataUrl"] = json!(format!("{origin}/metadata"));
        }
        if let Some(url) = callback_url {
            oauth["callbackUrl"] = json!(url);
        }
        std::fs::write(
            directory.path().join("mcp.json"),
            json!({"mcpServers":{"account":{"url":format!("{origin}/mcp"),"oauth":oauth}}})
                .to_string(),
        )
        .unwrap();
        let browser = Arc::new(Browser {
            automatic,
            fail: fail_browser,
            opens: AtomicUsize::new(0),
            tasks: Mutex::new(Vec::new()),
        });
        let library = McpLibrary::new(directory.path(), None, false).with_oauth_host(Arc::new(
            NativeMcpOAuthHost {
                browser: browser.clone(),
            },
        ));
        Self {
            directory,
            library,
            prompt: Arc::new(Prompt {
                url: Mutex::new(None),
                notices: Mutex::new(Vec::new()),
                answer,
            }),
            browser,
            server,
        }
    }
    fn callback_address(&self) -> String {
        let callback = callback(self.prompt.url.lock().unwrap().as_ref().unwrap(), false);
        let url = Url::parse(&callback).unwrap();
        format!("{}:{}", url.host_str().unwrap(), url.port().unwrap())
    }
    async fn finish(self) {
        let tasks = std::mem::take(&mut *self.browser.tasks.lock().unwrap());
        for task in tasks {
            task.await.unwrap();
        }
        assert!(!self.directory.path().join("sessions").exists());
        self.server.abort();
    }
}

#[tokio::test]
async fn browser_loopback_validates_state_uses_challenge_persists_and_reconnects() {
    let fixture = Fixture::new("wait", true, false, None, true).await;
    assert_eq!(
        fixture
            .library
            .login(
                "account",
                None,
                fixture.prompt.clone(),
                AbortHandle::new().1
            )
            .await
            .unwrap(),
        1
    );
    assert!(fixture.library.has_oauth_credentials("account").unwrap());
    assert!(
        tokio::net::TcpStream::connect(fixture.callback_address())
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .library
            .login(
                "account",
                None,
                fixture.prompt.clone(),
                AbortHandle::new().1
            )
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture.browser.opens.load(Ordering::SeqCst),
        1,
        "already authorized: no browser"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn configured_callback_bind_and_browser_failure_use_pasted_fallback() {
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_url = format!("http://{}/custom/callback", occupied.local_addr().unwrap());
    let fixture = Fixture::new("paste", false, true, Some(callback_url.clone()), false).await;
    assert_eq!(
        fixture
            .library
            .login(
                "account",
                None,
                fixture.prompt.clone(),
                AbortHandle::new().1
            )
            .await
            .unwrap(),
        1
    );
    let authorization = Url::parse(fixture.prompt.url.lock().unwrap().as_ref().unwrap()).unwrap();
    assert!(
        authorization
            .query_pairs()
            .any(|(key, value)| key == "redirect_uri" && value == callback_url)
    );
    assert_eq!(fixture.prompt.notices.lock().unwrap().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn cancellation_and_timeout_close_listener_without_fabricating_credentials() {
    for cancelled in [true, false] {
        let fixture = Fixture::new(
            if cancelled { "cancel" } else { "wait" },
            false,
            false,
            None,
            false,
        )
        .await;
        let operation = fixture.library.login(
            "account",
            None,
            fixture.prompt.clone(),
            AbortHandle::new().1,
        );
        if cancelled {
            assert!(matches!(operation.await, Err(McpOAuthError::Cancelled)));
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(150), operation)
                    .await
                    .is_err()
            );
        }
        assert!(!fixture.library.has_oauth_credentials("account").unwrap());
        assert!(
            tokio::net::TcpStream::connect(fixture.callback_address())
                .await
                .is_err()
        );
        fixture.finish().await;
    }
}

#[test]
fn browser_endpoint_cannot_launch_an_os_protocol_handler() {
    for value in [
        "file:///Applications/Calculator.app",
        "javascript:alert(1)",
        "data:text/plain,hi",
        "custom-app://launch",
        "https://user:secret@example.com/",
    ] {
        assert!(validate_browser_url(value).is_err());
    }
    assert!(validate_browser_url("https://example.com/authorize?state=test").is_ok());
    assert!(validate_browser_url("http://127.0.0.1:8989/authorize").is_ok());
}

#[test]
fn callback_parser_requires_exact_path_origin_and_one_state() {
    let redirect = McpOAuthRedirect {
        authorization_url: String::new(),
        redirect_url: "http://127.0.0.1:3456/callback/cimd-id".into(),
        state: "expected".into(),
    };
    for target in [
        "/callback?state=expected",
        "//evil.invalid/callback/cimd-id?state=expected",
        "/callback/cimd-id?state=expected&state=other",
        "/callback/cimd-id?state=wrong",
    ] {
        assert!(
            callback_target(
                format!("GET {target} HTTP/1.1\r\n\r\n").as_bytes(),
                &redirect
            )
            .is_none()
        );
    }
    assert!(
        callback_target(
            b"GET /callback/cimd-id?code=fixture&state=expected HTTP/1.1\r\n\r\n",
            &redirect
        )
        .is_some()
    );
}
