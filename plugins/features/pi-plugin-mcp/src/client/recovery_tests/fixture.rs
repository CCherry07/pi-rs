use super::*;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use http::{HeaderMap, HeaderValue, StatusCode};
use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

#[derive(Clone, Copy)]
pub(super) enum Reply {
    Status(u16),
    StatusRpc(u16),
    Auth,
    Scope,
    HeldScope,
    Rpc,
    Network,
    Ok,
}

pub(super) struct StateData {
    pub starts: AtomicUsize,
    pub token_calls: AtomicUsize,
    pub lists: AtomicUsize,
    pub calls: Mutex<Vec<(String, String)>>,
    pub authorization: Mutex<Vec<Option<String>>>,
    pub deleted: Mutex<Vec<String>>,
    pub hold_json_body: AtomicBool,
    pub executed: AtomicUsize,
    pub tools: Mutex<Vec<McpToolSpec>>,
    pub replies: Mutex<VecDeque<Reply>>,
    pub resource_replies: Mutex<VecDeque<Reply>>,
    pub pages: Mutex<Vec<(String, Option<String>)>>,
    pub rounds: AtomicUsize,
    pub hold_init: AtomicBool,
    pub init_entered: Semaphore,
    pub init_release: Semaphore,
    pub held_entered: Semaphore,
    pub held_release: Arc<Semaphore>,
    pub repeat_cursor: AtomicBool,
}

pub(super) struct Fixture {
    pub config: McpServerConfig,
    pub state: Arc<StateData>,
    auth: McpClientAuth,
    dir: Option<tempfile::TempDir>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn remote(name: &str, read_only: bool) -> McpToolSpec {
    serde_json::from_value(json!({"name":name,"description":"frozen description","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":read_only}})).unwrap()
}

impl Fixture {
    pub async fn new(read_only: bool) -> Self {
        let state = Arc::new(StateData {
            starts: AtomicUsize::new(0),
            token_calls: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            authorization: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            hold_json_body: AtomicBool::new(false),
            executed: AtomicUsize::new(0),
            tools: Mutex::new(vec![
                remote("echo", read_only),
                remote("held-json", false),
                remote("held-sse", false),
            ]),
            replies: Mutex::new(VecDeque::new()),
            resource_replies: Mutex::new(VecDeque::new()),
            pages: Mutex::new(Vec::new()),
            rounds: AtomicUsize::new(0),
            hold_init: AtomicBool::new(false),
            init_entered: Semaphore::new(0),
            init_release: Semaphore::new(0),
            held_entered: Semaphore::new(0),
            held_release: Arc::new(Semaphore::new(0)),
            repeat_cursor: AtomicBool::new(false),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            "fixture",
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        )
        .headers(BTreeMap::from([(
            "Authorization".into(),
            "Bearer fixture".into(),
        )]));
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let metadata = json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),"token_endpoint":format!("{origin}/token"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":["none","client_secret_post"],"code_challenge_methods_supported":["S256"]});
        let app = Router::new().route("/mcp", post(endpoint).delete(|State(state): State<Arc<StateData>>, headers: HeaderMap| async move {
                state.deleted.lock().unwrap().push(headers["mcp-session-id"].to_str().unwrap().to_owned());
                StatusCode::NO_CONTENT
            }))
            .route("/metadata", axum::routing::get(move || { let metadata = metadata.clone(); async move { Json(metadata) } }))
            .route("/token", post(|State(state): State<Arc<StateData>>| async move {
                let number = state.token_calls.fetch_add(1, Ordering::AcqRel) + 1;
                Json(json!({"access_token":format!("rotated-{number}"),"refresh_token":format!("refresh-{number}"),"token_type":"Bearer","expires_in":3600}))
            }))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            config,
            state,
            auth: McpClientAuth::default(),
            dir: None,
            task,
        }
    }

    pub fn oauth(mut self) -> Self {
        let McpTransport::Http { url, headers } = &mut self.config.transport else {
            unreachable!()
        };
        headers.clear();
        let dir = tempfile::tempdir().unwrap();
        let store = crate::McpOAuthCredentialStore::new(dir.path().join("auth.json"));
        store
            .save_tokens(
                "fixture",
                url,
                "fixture-client",
                crate::McpOAuthTokens {
                    access_token: "initial".into(),
                    token_type: "Bearer".into(),
                    refresh_token: Some("refresh".into()),
                    expires_in: Some(3600),
                    scope: None,
                },
            )
            .unwrap();
        self.config.oauth = Some(McpOAuthConfig {
            client_id: Some("fixture-client".into()),
            auth_server_metadata_url: Some(url.replace("/mcp", "/metadata")),
            ..Default::default()
        });
        self.auth = McpClientAuth::default().with_oauth_store(store);
        self.dir = Some(dir);
        self
    }

    pub fn provider(mut self, token: Option<&str>) -> Self {
        struct Tokens(Option<String>);
        #[async_trait]
        impl crate::McpProviderTokenResolver for Tokens {
            async fn token(&self, provider: &str) -> Result<Option<String>, String> {
                assert_eq!(provider, "fixture-provider");
                Ok(self.0.clone())
            }
        }
        let McpTransport::Http { headers, .. } = &mut self.config.transport else {
            unreachable!()
        };
        headers.clear();
        self.config.auth = Some(McpProviderAuth {
            provider: "fixture-provider".into(),
        });
        self.auth = McpClientAuth::default()
            .with_provider_tokens(Arc::new(Tokens(token.map(str::to_owned))));
        self
    }

    pub async fn explicit(&self) -> McpToolSet {
        McpToolSet::connect_with_auth(vec![self.config.clone()], self.auth.clone())
            .await
            .unwrap()
    }

    pub async fn prepare(
        &self,
    ) -> (
        crate::supervisor::McpSupervisor,
        crate::supervisor::McpGenerationReadiness,
        McpToolSet,
    ) {
        let supervisor = crate::supervisor::McpSupervisor::new();
        let input = crate::supervisor::ConnectionInput::new(
            self.config.clone(),
            (&McpServerPolicy::direct()).into(),
            self.auth.clone(),
            [0; 32],
        )
        .unwrap();
        let candidate = supervisor.begin_candidate();
        supervisor.reconcile(candidate, vec![input]).unwrap();
        let (source, pool) = self.publish(&supervisor).await;
        (supervisor, source, pool)
    }

    pub async fn publish(
        &self,
        supervisor: &crate::supervisor::McpSupervisor,
    ) -> (crate::supervisor::McpGenerationReadiness, McpToolSet) {
        let snapshot = supervisor
            .wait_for(&["fixture".into()], Duration::from_secs(5))
            .await;
        let source = crate::supervisor::McpGenerationReadiness::new(supervisor.clone(), &snapshot);
        let crate::supervisor::ConnectionReadiness::Ready(inventory) =
            snapshot.servers["fixture"].clone()
        else {
            panic!("fixture setup must be ready");
        };
        let pool = McpToolSet::from_connections(
            vec![(
                inventory.connection.clone(),
                inventory.tools.clone(),
                McpServerPolicy::direct(),
            )],
            vec![],
        )
        .unwrap()
        .with_supervision(source.downgrade())
        .unwrap();
        source.downgrade().publish();
        (source, pool)
    }

    pub fn replies(&self, replies: &[Reply]) {
        self.state.replies.lock().unwrap().extend(replies);
    }
    pub fn resources(&self, replies: &[Reply]) {
        self.state.resource_replies.lock().unwrap().extend(replies);
    }
    pub fn calls(&self) -> usize {
        self.state.calls.lock().unwrap().len()
    }
    pub fn starts(&self) -> usize {
        self.state.starts.load(Ordering::Acquire)
    }
    pub fn executed(&self) -> usize {
        self.state.executed.load(Ordering::Acquire)
    }
}

pub(super) async fn entered(gate: &Semaphore, count: u32) {
    tokio::time::timeout(Duration::from_secs(5), gate.acquire_many(count))
        .await
        .expect("fixture event deadline")
        .unwrap()
        .forget();
}

fn result(id: &Value, value: Value) -> Response {
    Json(json!({"jsonrpc":"2.0","id":id,"result":value})).into_response()
}

fn reject(id: &Value, reply: Reply) -> Option<Response> {
    match reply {
        Reply::Status(status) => Some(StatusCode::from_u16(status).unwrap().into_response()),
        Reply::StatusRpc(status) => Some((StatusCode::from_u16(status).unwrap(), Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"fixture correlated error"}}))).into_response()),
        Reply::Auth => Some((StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=\"fixture\", error=\"invalid_token\"")]).into_response()),
        Reply::Rpc => Some(Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"fixture RPC failure"}})).into_response()),
        Reply::Network => Some(Response::builder().header("content-type", "application/json").body(Body::from_stream(futures::stream::once(async { Err::<String,_>(std::io::Error::other("fixture stream closed")) }))).unwrap()),
        Reply::Scope => Some((StatusCode::FORBIDDEN, [("www-authenticate", "Bearer error=\"insufficient_scope\", scope=\"private-scope-marker\"")]).into_response()),
        Reply::HeldScope => unreachable!("held rejection must pass the fixture gate first"),
        Reply::Ok => None,
    }
}

async fn endpoint(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    let id = &message["id"];
    if id.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let session = headers
        .get("mcp-session-id")
        .and_then(|header| header.to_str().ok())
        .unwrap_or("")
        .to_owned();
    match message["method"].as_str().unwrap() {
        "server/discover" => Json(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}}),
        )
        .into_response(),
        "initialize" => {
            let epoch = state.starts.fetch_add(1, Ordering::AcqRel) + 1;
            if epoch > 1 && state.hold_init.load(Ordering::Acquire) {
                state.init_entered.add_permits(1);
                entered(&state.init_release, 1).await;
            }
            let mut response = result(
                id,
                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"fixture","version":"1"},"instructions":format!("instructions-{epoch}")}),
            );
            response.headers_mut().insert(
                "mcp-session-id",
                HeaderValue::from_str(&format!("session-{epoch}")).unwrap(),
            );
            response
        }
        "tools/list" => {
            state.lists.fetch_add(1, Ordering::AcqRel);
            result(id, json!({"tools":state.tools.lock().unwrap().clone()}))
        }
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap().to_owned();
            state.calls.lock().unwrap().push((session, name.clone()));
            state.authorization.lock().unwrap().push(
                headers
                    .get(http::header::AUTHORIZATION)
                    .map(|value| value.to_str().unwrap().to_owned()),
            );
            let answer = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"executed"}]}});
            if name == "held-json" {
                if state.hold_json_body.load(Ordering::Acquire) {
                    use futures::StreamExt;
                    let first = state.clone();
                    let last = state.clone();
                    let stream = futures::stream::once(async move {
                        first.held_entered.add_permits(1);
                        Ok::<_, Infallible>(String::from(" "))
                    })
                    .chain(futures::stream::once(async move {
                        entered(&last.held_release, 1).await;
                        last.executed.fetch_add(1, Ordering::AcqRel);
                        Ok::<_, Infallible>(answer.to_string())
                    }));
                    return Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from_stream(stream))
                        .unwrap();
                }
                state.held_entered.add_permits(1);
                entered(&state.held_release, 1).await;
                state.executed.fetch_add(1, Ordering::AcqRel);
                return Json(answer).into_response();
            }
            if name == "held-sse" {
                use futures::StreamExt;
                state.held_entered.add_permits(1);
                let gate = state.held_release.clone();
                let stream = futures::stream::once(async {
                    Ok::<_, Infallible>(String::from(": open\n\n"))
                })
                .chain(futures::stream::once(async move {
                    entered(&gate, 1).await;
                    state.executed.fetch_add(1, Ordering::AcqRel);
                    Ok::<_, Infallible>(format!("event: message\ndata: {answer}\n\n"))
                }));
                return Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
            let reply = state
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Reply::Ok);
            if matches!(reply, Reply::HeldScope) {
                state.held_entered.add_permits(1);
                entered(&state.held_release, 1).await;
                return reject(id, Reply::Scope).unwrap();
            }
            if let Some(response) = reject(id, reply) {
                return response;
            }
            state.executed.fetch_add(1, Ordering::AcqRel);
            Json(answer).into_response()
        }
        "resources/read" => {
            let reply = state
                .resource_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Reply::Ok);
            state
                .pages
                .lock()
                .unwrap()
                .push((session, Some("read".into())));
            if let Some(response) = reject(id, reply) {
                return response;
            }
            result(
                id,
                json!({"contents":[{"uri":"fixture://read","text":"resource","mimeType":"text/plain"}]}),
            )
        }
        "resources/list" | "resources/templates/list" => {
            let templates = message["method"] == "resources/templates/list";
            let cursor = message["params"]["cursor"].as_str().map(str::to_owned);
            state.pages.lock().unwrap().push((session, cursor.clone()));
            if cursor.is_some() {
                let reply = state
                    .resource_replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Reply::Ok);
                if let Some(response) = reject(id, reply) {
                    return response;
                }
            }
            let round = if cursor.is_none() {
                state.rounds.fetch_add(1, Ordering::AcqRel) + 1
            } else {
                state.rounds.load(Ordering::Acquire)
            };
            let name = format!(
                "round-{round}-{}",
                if cursor.is_none() { "first" } else { "last" }
            );
            let mut value = if templates {
                json!({"resourceTemplates":[{"name":name,"uriTemplate":format!("fixture://{name}/{{id}}")}]})
            } else {
                json!({"resources":[{"name":name,"uri":format!("fixture://{name}")}]})
            };
            if cursor.is_none() || state.repeat_cursor.load(Ordering::Acquire) {
                value["nextCursor"] = json!("next");
            }
            result(id, value)
        }
        method => panic!("unexpected request {method}"),
    }
}
