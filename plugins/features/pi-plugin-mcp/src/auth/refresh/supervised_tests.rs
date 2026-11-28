use super::*;
use crate::client::operation::{DispatchGate, OperationError, OperationGuard};
use axum::response::IntoResponse;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug)]
enum ClientKind {
    Fixed,
    Public,
    Dcr,
    Cimd,
    #[cfg(unix)]
    Local,
}

fn kinds() -> Vec<ClientKind> {
    #[allow(unused_mut)]
    let mut kinds = vec![
        ClientKind::Fixed,
        ClientKind::Public,
        ClientKind::Dcr,
        ClientKind::Cimd,
    ];
    #[cfg(unix)]
    kinds.push(ClientKind::Local);
    kinds
}

#[derive(Clone, Copy)]
enum Reply {
    Auto,
    Challenge,
    Plain401,
    Plain403,
    Scope401,
    Scope403,
}

struct Fixture {
    directory: tempfile::TempDir,
    url: String,
    client_id: String,
    store: McpOAuthCredentialStore,
    client: Arc<OAuthHttpClient>,
    plan: Arc<Mutex<VecDeque<Reply>>>,
    posts: Arc<AtomicUsize>,
    exchanges: Arc<AtomicUsize>,
    forms: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
    accepted: Arc<Mutex<String>>,
    fail: Arc<AtomicBool>,
    unchanged: Arc<AtomicBool>,
    hold: Arc<AtomicBool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    rejected: Arc<Semaphore>,
    hold_rejections: Arc<AtomicBool>,
    reject_release: Arc<Semaphore>,
    body_hold: Arc<AtomicBool>,
    body_entered: Arc<Semaphore>,
    body_release: Arc<Semaphore>,
    cleanup_headers: Arc<Mutex<Vec<Option<String>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(kind: ClientKind, mode: ConnectionMode) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let metadata = json!({
            "issuer": origin, "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "token_endpoint_auth_methods_supported": ["none", "client_secret_post"]
        });
        let plan = Arc::new(Mutex::new(VecDeque::new()));
        let posts = Arc::new(AtomicUsize::new(0));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let forms = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(Mutex::new("initial".to_owned()));
        let fail = Arc::new(AtomicBool::new(false));
        let unchanged = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let rejected = Arc::new(Semaphore::new(0));
        let hold_rejections = Arc::new(AtomicBool::new(false));
        let reject_release = Arc::new(Semaphore::new(0));
        let body_hold = Arc::new(AtomicBool::new(false));
        let body_entered = Arc::new(Semaphore::new(0));
        let body_release = Arc::new(Semaphore::new(0));
        let cleanup_headers = Arc::new(Mutex::new(Vec::new()));
        let router = axum::Router::new()
            .route("/metadata", axum::routing::get({let metadata = metadata.clone(); move || {let metadata = metadata.clone(); async {axum::Json(metadata)}}}))
            .route("/.well-known/oauth-authorization-server", axum::routing::get(move || {let metadata = metadata.clone(); async {axum::Json(metadata)}}))
            .route("/token", axum::routing::post({
                let (exchanges, forms, accepted, fail, unchanged, hold, entered, release) = (exchanges.clone(), forms.clone(), accepted.clone(), fail.clone(), unchanged.clone(), hold.clone(), entered.clone(), release.clone());
                move |body: String| {
                    let (exchanges, forms, accepted, fail, unchanged, hold, entered, release) = (exchanges.clone(), forms.clone(), accepted.clone(), fail.clone(), unchanged.clone(), hold.clone(), entered.clone(), release.clone());
                    async move {
                        let form = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect::<BTreeMap<_, _>>();
                        assert_eq!(form["grant_type"], "refresh_token");
                        assert!(form.contains_key("resource"));
                        forms.lock().unwrap().push(form);
                        let exchange = exchanges.fetch_add(1, Ordering::SeqCst) + 1;
                        entered.add_permits(1);
                        if hold.load(Ordering::SeqCst) { release.acquire().await.unwrap().forget(); }
                        if fail.load(Ordering::SeqCst) {
                            return (http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error":"server_error","error_description":"private fixture secret"}))).into_response();
                        }
                        let token = if unchanged.load(Ordering::SeqCst) { "initial".to_owned() } else { format!("fresh-{exchange}") };
                        *accepted.lock().unwrap() = token.clone();
                        axum::Json(json!({"access_token":token,"token_type":"Bearer","refresh_token":format!("rotated-{exchange}"),"expires_in":3600})).into_response()
                    }
                }
            }))
            .route("/mcp", axum::routing::post({
                let (plan, posts, accepted, rejected, hold_rejections, reject_release, body_hold, body_entered, body_release) = (plan.clone(), posts.clone(), accepted.clone(), rejected.clone(), hold_rejections.clone(), reject_release.clone(), body_hold.clone(), body_entered.clone(), body_release.clone());
                move |headers: http::HeaderMap, axum::Json(message): axum::Json<Value>| {
                    let (plan, posts, accepted, rejected, hold_rejections, reject_release, body_hold, body_entered, body_release) = (plan.clone(), posts.clone(), accepted.clone(), rejected.clone(), hold_rejections.clone(), reject_release.clone(), body_hold.clone(), body_entered.clone(), body_release.clone());
                    async move {
                        let id = &message["id"];
                        if id.is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                        if message["method"] == "initialize" {
                            return axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"budget-fixture","version":"1"}}})).into_response();
                        }
                        posts.fetch_add(1, Ordering::SeqCst);
                        let reply = plan.lock().unwrap().pop_front().unwrap_or(Reply::Auto);
                        let allowed = headers.get("authorization").and_then(|value| value.to_str().ok()) == Some(format!("Bearer {}", accepted.lock().unwrap()).as_str());
                        let rejection = match reply {
                            Reply::Plain401 => Some((401, None)),
                            Reply::Plain403 => Some((403, None)),
                            Reply::Scope401 => Some((401, Some("Bearer error=insufficient_scope, scope=admin"))),
                            Reply::Scope403 => Some((403, Some("Bearer error=insufficient_scope, scope=admin"))),
                            Reply::Challenge => Some((401, Some("Bearer error=invalid_token"))),
                            Reply::Auto if !allowed => Some((401, Some("Bearer error=invalid_token"))),
                            Reply::Auto => None,
                        };
                        if let Some((status, challenge)) = rejection {
                            rejected.add_permits(1);
                            if hold_rejections.load(Ordering::SeqCst) { reject_release.acquire().await.unwrap().forget(); }
                            let mut response = http::StatusCode::from_u16(status).unwrap().into_response();
                            if let Some(challenge) = challenge { response.headers_mut().insert("www-authenticate", http::HeaderValue::from_static(challenge)); }
                            return response;
                        }
                        let result = if message["method"] == "tools/call" { json!({"content":[]}) } else { json!({}) };
                        let reply = json!({"jsonrpc":"2.0","id":id,"result":result});
                        if body_hold.load(Ordering::SeqCst) {
                            let body = axum::body::Body::from_stream(futures::stream::once(async move {
                                body_entered.add_permits(1);
                                body_release.acquire().await.unwrap().forget();
                                Ok::<_, std::convert::Infallible>(reply.to_string())
                            }));
                            return ([("content-type", "application/json")], body).into_response();
                        }
                        axum::Json(reply).into_response()
                    }
                }
            }).delete({
                let cleanup_headers = cleanup_headers.clone();
                move |headers: http::HeaderMap| {
                    let cleanup_headers = cleanup_headers.clone();
                    async move {
                        cleanup_headers.lock().unwrap().push(headers.get("authorization")
                            .map(|value| value.to_str().unwrap().to_owned()));
                        (http::StatusCode::FORBIDDEN,
                            [("www-authenticate", "Bearer error=insufficient_scope, scope=cleanup-private")],
                            "cleanup-body-private").into_response()
                    }
                }
            }));
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let store = McpOAuthCredentialStore::memory();
        let client_id = if matches!(kind, ClientKind::Cimd) {
            "https://pi.dev/oauth/client.json"
        } else {
            "fixture-client"
        }
        .to_owned();
        save(&store, &url, &client_id, "initial", 3600);
        let mut settings = McpOAuthConfig {
            client_id: Some(client_id.clone()),
            auth_server_metadata_url: Some(format!("{origin}/metadata")),
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut auth = McpClientAuth::default().with_oauth_store(store.clone());
        if matches!(kind, ClientKind::Fixed) {
            settings.client_secret = Some("fixed-secret".into());
        }
        #[cfg(unix)]
        if matches!(kind, ClientKind::Local) {
            std::fs::write(directory.path().join("secret"), "late-secret").unwrap();
            settings.client_secret =
                Some("!printf x >> runs; read value < secret; printf '%s' \"$value\"".into());
            auth.local_values = Some(crate::config::LocalValues {
                cwd: directory.path().into(),
                allow_commands: true,
            });
        }
        let client = Arc::new(
            oauth_http_client(
                "fixture",
                &url,
                (!matches!(kind, ClientKind::Dcr)).then_some(&settings),
                &auth,
                mode,
            )
            .await
            .unwrap_or_else(|error| panic!("{kind:?}: {error}")),
        );
        assert!(!directory.path().join("runs").exists());
        Self {
            directory,
            url,
            client_id,
            store,
            client,
            plan,
            posts,
            exchanges,
            forms,
            accepted,
            fail,
            unchanged,
            hold,
            entered,
            release,
            rejected,
            hold_rejections,
            reject_release,
            body_hold,
            body_entered,
            body_release,
            cleanup_headers,
            task,
        }
    }

    fn call(
        &self,
        control: Option<RequestControl>,
        id: u64,
    ) -> tokio::task::JoinHandle<Result<StreamableHttpPostResponse, HttpError>> {
        let client = self.client.clone();
        let uri = Arc::<str>::from(self.url.clone());
        tokio::spawn(async move {
            let mut message: ClientJsonRpcMessage =
                serde_json::from_value(json!({"jsonrpc":"2.0","id":id,"method":"ping"})).unwrap();
            if let (Some(control), ClientJsonRpcMessage::Request(request)) = (control, &mut message)
            {
                control.attach(&mut request.request);
            }
            client
                .post_message(uri, message, None, None, HashMap::new())
                .await
        })
    }

    fn reject(&self) {
        *self.accepted.lock().unwrap() = "requires-refresh".into();
    }
}

fn operation() -> (OperationGuard, Arc<DispatchGate>, RequestControl) {
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    (guard, gate, control)
}

fn save(store: &McpOAuthCredentialStore, url: &str, client_id: &str, token: &str, expires: u64) {
    store
        .save_tokens(
            "fixture",
            url,
            client_id,
            McpOAuthTokens {
                access_token: token.into(),
                token_type: "Bearer".into(),
                refresh_token: Some("refresh".into()),
                expires_in: Some(expires),
                scope: None,
            },
        )
        .unwrap();
}

async fn permit(semaphore: &Semaphore, count: u32) {
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        semaphore.acquire_many(count),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
}

#[tokio::test]
async fn all_supervised_oauth_modes_share_operation_allowance_and_keep_sdk_refresh_authority() {
    for kind in kinds() {
        let f = Fixture::new(kind, ConnectionMode::Supervised).await;
        assert!(
            f.client.mode == ConnectionMode::Supervised,
            "supervised {kind:?} must use the existing coordinator, not SDK hidden replay"
        );
        f.reject();
        let (_guard, _gate, control) = operation();
        assert!(f.call(Some(control.clone()), 1).await.unwrap().is_ok());
        assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(f.posts.load(Ordering::SeqCst), 2);
        assert!(!control.try_recover().unwrap());
        assert_eq!(f.forms.lock().unwrap()[0]["client_id"], f.client_id);
        let form = f.forms.lock().unwrap()[0].clone();
        match kind {
            ClientKind::Fixed => assert_eq!(form["client_secret"], "fixed-secret"),
            #[cfg(unix)]
            ClientKind::Local => {
                assert_eq!(form["client_secret"], "late-secret");
                assert_eq!(
                    std::fs::read(f.directory.path().join("runs")).unwrap(),
                    b"x"
                );
            }
            _ => assert!(!form.contains_key("client_secret")),
        }
        assert_eq!(
            f.directory.path().join("runs").exists(),
            form.get("client_secret")
                .is_some_and(|secret| secret == "late-secret")
        );
        f.reject();
        let result = f.call(Some(control), 2).await.unwrap();
        assert!(
            matches!(result, Err(StreamableHttpError::Client(error)) if error.auth_rejection().and_then(|auth| auth.challenge()) == Some("Bearer error=invalid_token"))
        );
        assert_eq!(
            f.exchanges.load(Ordering::SeqCst),
            1,
            "a later rejected page cannot refresh again"
        );
        assert_eq!(
            f.posts.load(Ordering::SeqCst),
            3,
            "normal page dispatch is not a recovery replay"
        );
    }
}

#[tokio::test]
async fn spent_allowance_does_not_join_refresh_and_proactive_renewal_on_later_pages_is_free() {
    for kind in kinds() {
        let f = Fixture::new(kind, ConnectionMode::Supervised).await;
        let (_guard, _gate, control) = operation();
        assert!(control.try_recover().unwrap());
        f.reject();
        assert!(matches!(
            f.call(Some(control.clone()), 1).await.unwrap(),
            Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some()
        ));
        assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
        save(&f.store, &f.url, &f.client_id, "expired", 0);
        #[cfg(unix)]
        if matches!(kind, ClientKind::Local) {
            std::fs::write(f.directory.path().join("secret"), "changed-secret").unwrap();
        }
        assert!(f.call(Some(control), 2).await.unwrap().is_ok());
        #[cfg(unix)]
        if matches!(kind, ClientKind::Local) {
            assert_eq!(
                f.forms.lock().unwrap()[0]["client_secret"],
                "changed-secret"
            );
        }
        assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn every_oauth_mode_observes_exact_plain_and_scope_rejection_matrix() {
    for kind in kinds() {
        for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
            for reply in [
                Reply::Plain401,
                Reply::Challenge,
                Reply::Plain403,
                Reply::Scope401,
                Reply::Scope403,
            ] {
                let f = Fixture::new(kind, mode).await;
                f.plan.lock().unwrap().push_back(reply);
                let (_guard, _gate, control) = operation();
                let result = f.call(Some(control.clone()), 1).await.unwrap();
                let reactive = matches!(reply, Reply::Plain401 | Reply::Challenge);
                if reactive {
                    assert!(result.is_ok());
                } else {
                    let Err(StreamableHttpError::Client(error)) = result else {
                        panic!("expected private rejection")
                    };
                    if matches!(reply, Reply::Plain403) {
                        assert_eq!(error.status(), Some(http::StatusCode::FORBIDDEN));
                        assert!(error.auth_rejection().is_none());
                    } else {
                        let auth = error.auth_rejection().unwrap();
                        assert!(auth.is_insufficient_scope());
                        assert!(auth.requires_sign_in());
                        assert!(auth.has_auth_provider());
                        assert!(!format!("{error:?}").contains("scope=admin"));
                    }
                }
                assert_eq!(f.exchanges.load(Ordering::SeqCst), usize::from(reactive));
                assert_eq!(f.posts.load(Ordering::SeqCst), 1 + usize::from(reactive));
                assert_eq!(
                    control.try_recover().unwrap(),
                    !(reactive && mode == ConnectionMode::Supervised)
                );
            }
        }
    }
    let f = Fixture::new(ClientKind::Public, ConnectionMode::Supervised).await;
    f.store.sign_out("fixture", &f.url).unwrap();
    let (_guard, _gate, control) = operation();
    let Err(StreamableHttpError::Client(error)) = f.call(Some(control.clone()), 1).await.unwrap()
    else {
        panic!("expected grant rejection")
    };
    let auth = error.auth_rejection().unwrap();
    assert!(auth.requires_sign_in() && auth.has_auth_provider());
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
    assert!(control.try_recover().unwrap());
}

#[tokio::test]
async fn explicit_fixed_uses_same_coordinator_and_contextless_supervised_call_is_separately_bounded()
 {
    let explicit = Fixture::new(ClientKind::Fixed, ConnectionMode::Explicit).await;
    assert_eq!(explicit.client.mode, ConnectionMode::Explicit);
    explicit.reject();
    assert!(explicit.call(None, 1).await.unwrap().is_ok());
    let supervised = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
    supervised
        .plan
        .lock()
        .unwrap()
        .extend([Reply::Challenge, Reply::Challenge]);
    assert!(matches!(
        supervised.call(None, 1).await.unwrap(),
        Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some()
    ));
    assert_eq!(supervised.posts.load(Ordering::SeqCst), 2);
    assert_eq!(supervised.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_and_superseded_waiters_do_not_resend_but_rotated_tokens_are_saved() {
    for cancel in 0..3 {
        let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
        f.reject();
        f.hold.store(true, Ordering::SeqCst);
        let (guard, gate, control) = operation();
        let call = f.call(Some(control), 1);
        permit(&f.entered, 1).await;
        assert!(
            !guard.operation().try_recover().unwrap(),
            "reservation precedes exchange completion"
        );
        let expected = match cancel {
            0 => {
                drop(guard);
                OperationError::Cancelled
            }
            1 => {
                gate.supersede();
                OperationError::TransportSuperseded
            }
            _ => {
                gate.retire();
                OperationError::AuthorityRetired
            }
        };
        f.release.add_permits(1);
        let result = call.await.unwrap();
        let Err(StreamableHttpError::Io(error)) = result else {
            panic!("expected typed dispatch fence")
        };
        assert_eq!(
            error.get_ref().unwrap().downcast_ref::<OperationError>(),
            Some(&expected)
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        let stored = CredentialStore::load(&f.store.for_server("fixture", &f.url).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored
                .token_response
                .unwrap()
                .refresh_token()
                .unwrap()
                .secret(),
            "rotated-1"
        );
    }
}

#[tokio::test]
async fn callers_share_exchange_but_independently_reserve_before_joining_and_never_refund() {
    let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
    f.reject();
    f.hold.store(true, Ordering::SeqCst);
    f.hold_rejections.store(true, Ordering::SeqCst);
    let (_first_guard, _first_gate, first) = operation();
    let (_second_guard, _second_gate, second) = operation();
    let a = f.call(Some(first.clone()), 1);
    let b = f.call(Some(second.clone()), 2);
    permit(&f.rejected, 2).await;
    f.reject_release.add_permits(2);
    permit(&f.entered, 1).await;
    f.release.add_permits(1);
    assert!(a.await.unwrap().is_ok());
    assert!(b.await.unwrap().is_ok());
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
    assert!(!first.try_recover().unwrap());
    assert!(!second.try_recover().unwrap());
    for unchanged in [true, false] {
        let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
        f.reject();
        f.unchanged.store(unchanged, Ordering::SeqCst);
        f.fail.store(!unchanged, Ordering::SeqCst);
        let (_guard, _gate, control) = operation();
        let result = f.call(Some(control.clone()), 1).await.unwrap();
        if unchanged {
            assert!(
                result.is_ok(),
                "successful unchanged-token refresh must replay"
            );
        } else {
            assert!(
                !result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("private fixture secret")
            );
        }
        assert!(!control.try_recover().unwrap());
        assert_eq!(f.posts.load(Ordering::SeqCst), 1 + usize::from(unchanged));
    }
}

#[tokio::test]
async fn actual_sdk_post_delivery_carries_oauth_budget_for_every_supervised_mode() {
    use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
    use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
    use rmcp::transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    };
    for kind in kinds() {
        for spent in [false, true] {
            let f = Fixture::new(kind, ConnectionMode::Supervised).await;
            let transport = StreamableHttpClientTransport::with_client(
                f.client.as_ref().clone(),
                StreamableHttpClientTransportConfig::with_uri(f.url.clone()),
            );
            let mut service = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
            )
            .await
            .unwrap()
            .unwrap();
            let (_guard, _gate, control) = operation();
            if spent {
                assert!(control.try_recover().unwrap());
            }
            f.reject();
            let mut request = ClientRequest::CallToolRequest(CallToolRequest::new(
                CallToolRequestParams::new("echo"),
            ));
            control.attach(&mut request);
            let handle = service
                .peer()
                .send_cancellable_request(request, Default::default())
                .await
                .unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), handle.await_response())
                    .await
                    .unwrap();
            assert_eq!(result.is_err(), spent, "{kind:?}, spent={spent}");
            assert_eq!(f.exchanges.load(Ordering::SeqCst), usize::from(!spent));
            assert_eq!(f.posts.load(Ordering::SeqCst), if spent { 1 } else { 2 });
            assert!(!control.try_recover().unwrap());
            let _ = service
                .close_with_timeout(std::time::Duration::from_secs(1))
                .await;
        }
    }
}

#[tokio::test]
async fn cancelled_proactive_waiter_saves_rotation_without_initial_dispatch_or_spending_allowance()
{
    let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
    save(&f.store, &f.url, &f.client_id, "expired", 0);
    f.hold.store(true, Ordering::SeqCst);
    let (guard, _gate, control) = operation();
    let call = f.call(Some(control), 1);
    permit(&f.entered, 1).await;
    assert!(
        guard.operation().try_recover().unwrap(),
        "proactive exchange must not reserve recovery"
    );
    drop(guard);
    f.release.add_permits(1);
    let Err(StreamableHttpError::Io(error)) = call.await.unwrap() else {
        panic!("cancelled proactive waiter must stay typed")
    };
    assert_eq!(
        error.get_ref().unwrap().downcast_ref::<OperationError>(),
        Some(&OperationError::Cancelled)
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let before = f
        .store
        .read(|document| serde_json::to_value(document).unwrap())
        .unwrap();
    f.client
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *f.cleanup_headers.lock().unwrap(),
        vec![Some("Bearer fresh-1".into())]
    );
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.store
            .read(|document| serde_json::to_value(document).unwrap())
            .unwrap(),
        before
    );
    let stored = CredentialStore::load(&f.store.for_server("fixture", &f.url).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored
            .token_response
            .unwrap()
            .refresh_token()
            .unwrap()
            .secret(),
        "rotated-1"
    );
}

#[tokio::test]
async fn exhausted_caller_does_not_wait_for_another_callers_shared_reactive_exchange() {
    let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
    f.reject();
    f.hold.store(true, Ordering::SeqCst);
    f.hold_rejections.store(true, Ordering::SeqCst);
    let (_first_guard, _first_gate, first) = operation();
    let first_call = f.call(Some(first), 1);
    permit(&f.rejected, 1).await;
    let (_second_guard, _second_gate, second) = operation();
    assert!(second.try_recover().unwrap());
    let second_call = f.call(Some(second), 2);
    permit(&f.rejected, 1).await;
    f.reject_release.add_permits(1);
    permit(&f.entered, 1).await;
    f.reject_release.add_permits(1);
    let second_call = tokio::time::timeout(std::time::Duration::from_secs(3), second_call)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        second_call,
        Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some()
    ));
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    f.release.add_permits(1);
    assert!(first_call.await.unwrap().is_ok());
}

#[tokio::test]
async fn proactive_refresh_then_rejected_token_can_still_use_the_one_reactive_allowance() {
    for kind in kinds() {
        let f = Fixture::new(kind, ConnectionMode::Supervised).await;
        save(&f.store, &f.url, &f.client_id, "expired", 0);
        f.plan.lock().unwrap().push_back(Reply::Challenge);
        let (_guard, _gate, control) = operation();
        assert!(f.call(Some(control.clone()), 1).await.unwrap().is_ok());
        assert_eq!(
            f.exchanges.load(Ordering::SeqCst),
            2,
            "proactive and reactive refresh both use SDK exchange"
        );
        assert_eq!(
            f.posts.load(Ordering::SeqCst),
            2,
            "only the rejected dispatch is replayed"
        );
        assert!(!control.try_recover().unwrap());
    }
}

struct ProbeResolver {
    calls: AtomicUsize,
    hold: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}

impl Default for ProbeResolver {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}

#[async_trait]
impl McpProviderTokenResolver for ProbeResolver {
    async fn token(&self, _provider: &str) -> Result<Option<String>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        if self.hold.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        Ok(Some("initial".into()))
    }
}

fn provider_call(
    f: &Fixture,
    resolver: Arc<ProbeResolver>,
    control: RequestControl,
    max_sse: bool,
) -> tokio::task::JoinHandle<Result<StreamableHttpPostResponse, HttpError>> {
    let uri = Arc::<str>::from(f.url.clone());
    tokio::spawn(async move {
        let client = ProviderAuthClient::new("fixture".into(), resolver);
        let mut message: ClientJsonRpcMessage =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"})).unwrap();
        if let ClientJsonRpcMessage::Request(request) = &mut message {
            control.attach(&mut request.request);
        }
        if max_sse {
            client
                .post_message_with_max_sse_event_size(
                    uri,
                    message,
                    None,
                    None,
                    HashMap::new(),
                    1024,
                )
                .await
        } else {
            client
                .post_message(uri, message, None, None, HashMap::new())
                .await
        }
    })
}

#[tokio::test]
async fn provider_token_await_checks_dispatch_without_refresh_or_replay() {
    for max_sse in [false, true] {
        let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
        let resolver = Arc::new(ProbeResolver::default());
        resolver.hold.store(true, Ordering::SeqCst);
        let (_guard, gate, control) = operation();
        let call = provider_call(&f, resolver.clone(), control.clone(), max_sse);
        permit(&resolver.entered, 1).await;
        gate.supersede();
        resolver.release.add_permits(1);
        let Err(StreamableHttpError::Io(error)) = call.await.unwrap() else {
            panic!("provider resolver wait must not reauthorize superseded dispatch")
        };
        assert_eq!(
            error.get_ref().unwrap().downcast_ref::<OperationError>(),
            Some(&OperationError::TransportSuperseded)
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), 0);
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        let (_next_guard, _next_gate, next) = operation();
        resolver.hold.store(false, Ordering::SeqCst);
        f.plan.lock().unwrap().push_back(Reply::Challenge);
        assert!(matches!(
            provider_call(&f, resolver.clone(), next.clone(), max_sse)
                .await
                .unwrap(),
            Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some()
        ));
        assert!(
            next.try_recover().unwrap(),
            "provider tokens do not use OAuth recovery"
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            2,
            "exactly one resolution per request, no forced retry"
        );
        assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn unchanged_token_success_replays_once_in_every_oauth_mode_and_second_rejection_is_terminal()
{
    for kind in kinds() {
        for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
            for second in [Reply::Auto, Reply::Plain401, Reply::Scope403] {
                let f = Fixture::new(kind, mode).await;
                f.unchanged.store(true, Ordering::SeqCst);
                f.plan.lock().unwrap().extend([Reply::Plain401, second]);
                let (_guard, _gate, control) = operation();
                let result = f.call(Some(control.clone()), 1).await.unwrap();
                if matches!(second, Reply::Auto) {
                    assert!(result.is_ok());
                } else {
                    let Err(StreamableHttpError::Client(error)) = result else {
                        panic!("expected raw replay rejection")
                    };
                    let auth = error.auth_rejection().unwrap();
                    assert!(auth.has_auth_provider());
                    assert!(
                        !auth.requires_sign_in(),
                        "second response must not run another callback"
                    );
                }
                assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
                assert_eq!(f.posts.load(Ordering::SeqCst), 2);
                assert_eq!(
                    control.try_recover().unwrap(),
                    mode == ConnectionMode::Explicit
                );
            }
        }
    }
}

#[tokio::test]
async fn proactive_failure_uses_current_store_and_never_resurrects_logout_or_overwrites_login() {
    for kind in kinds() {
        for change in 0..3 {
            let f = Fixture::new(kind, ConnectionMode::Supervised).await;
            save(&f.store, &f.url, &f.client_id, "initial", 0);
            f.fail.store(true, Ordering::SeqCst);
            f.hold.store(true, Ordering::SeqCst);
            let (_guard, _gate, control) = operation();
            let call = f.call(Some(control.clone()), 1);
            permit(&f.entered, 1).await;
            let expected = match change {
                0 => Some("initial"),
                1 => {
                    f.store.sign_out("fixture", &f.url).unwrap();
                    None
                }
                _ => {
                    save(&f.store, &f.url, &f.client_id, "new-login", 3600);
                    *f.accepted.lock().unwrap() = "new-login".into();
                    Some("new-login")
                }
            };
            f.release.add_permits(1);
            let result = call.await.unwrap();
            assert_eq!(result.is_ok(), expected.is_some());
            if let Err(error) = result {
                assert!(!format!("{error:?}").contains("private fixture secret"));
            }
            assert_eq!(f.exchanges.load(Ordering::SeqCst), 1);
            assert_eq!(f.posts.load(Ordering::SeqCst), 1);
            assert!(control.try_recover().unwrap());
            let token = CredentialStore::load(&f.store.for_server("fixture", &f.url).unwrap())
                .await
                .unwrap()
                .and_then(|stored| stored.token_response)
                .map(|token| token.access_token().secret().to_owned());
            assert_eq!(token.as_deref(), expected);
        }
    }
}

#[tokio::test]
async fn provider_rejection_matrix_marks_actual_provider_without_callback_refresh_or_replay() {
    for max_sse in [false, true] {
        for reply in [
            Reply::Plain401,
            Reply::Challenge,
            Reply::Plain403,
            Reply::Scope401,
            Reply::Scope403,
        ] {
            let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
            f.plan.lock().unwrap().push_back(reply);
            let resolver = Arc::new(ProbeResolver::default());
            let (_guard, _gate, control) = operation();
            let Err(StreamableHttpError::Client(error)) =
                provider_call(&f, resolver.clone(), control.clone(), max_sse)
                    .await
                    .unwrap()
            else {
                panic!("expected provider rejection")
            };
            if matches!(reply, Reply::Plain403) {
                assert!(error.auth_rejection().is_none());
                assert_eq!(error.status(), Some(http::StatusCode::FORBIDDEN));
            } else {
                let auth = error.auth_rejection().unwrap();
                assert!(auth.has_auth_provider());
                assert!(!auth.requires_sign_in());
            }
            assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
            assert_eq!(f.posts.load(Ordering::SeqCst), 1);
            assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
            assert!(control.try_recover().unwrap());
        }
    }
}

#[test]
fn oauth_sdk_post_projection_preserves_authoritative_completion_error_precedence() {
    let (guard, _gate, control) = operation();
    let projection = || {
        drop(guard);
        Err(StreamableHttpError::Client(PiHttpClientError::protocol(
            "projection failed",
        )))
    };

    let Err(StreamableHttpError::Io(error)) =
        LocalOAuthClient::finish_sdk_post(Some(&control), projection)
    else {
        panic!("authoritative completion must override the projection error")
    };
    assert_eq!(
        error.get_ref().unwrap().downcast_ref::<OperationError>(),
        Some(&OperationError::Cancelled)
    );
}

#[tokio::test]
async fn completed_oauth_json_drains_after_supersession_but_not_authoritative_cancellation() {
    for kind in kinds() {
        for replay in [false, true] {
            for invalidation in 0..3 {
                let f = Fixture::new(kind, ConnectionMode::Supervised).await;
                if replay {
                    f.plan.lock().unwrap().push_back(Reply::Challenge);
                }
                f.body_hold.store(true, Ordering::SeqCst);
                let (guard, gate, control) = operation();
                let call = f.call(Some(control), 1);
                permit(&f.body_entered, 1).await;
                let expected = match invalidation {
                    0 => {
                        gate.supersede();
                        None
                    }
                    1 => {
                        drop(guard);
                        Some(OperationError::Cancelled)
                    }
                    _ => {
                        gate.retire();
                        Some(OperationError::AuthorityRetired)
                    }
                };
                f.body_release.add_permits(1);
                let result = call.await.unwrap();
                match expected {
                    None => assert!(result.is_ok(), "admitted OAuth response must drain"),
                    Some(expected) => {
                        let Err(StreamableHttpError::Io(error)) = result else {
                            panic!("expected authoritative fence")
                        };
                        assert_eq!(
                            error.get_ref().unwrap().downcast_ref::<OperationError>(),
                            Some(&expected)
                        );
                    }
                }
                assert_eq!(f.posts.load(Ordering::SeqCst), 1 + usize::from(replay));
                assert_eq!(f.exchanges.load(Ordering::SeqCst), usize::from(replay));
            }
        }
    }
}

#[tokio::test]
async fn acquired_before_abort_is_cached_for_cleanup_without_authorizing_initial_or_replay_post() {
    for initial in [false, true] {
        for token in [
            None,
            Some(String::new()),
            Some("acquired-before-abort".to_owned()),
        ] {
            let f = Fixture::new(ClientKind::Public, ConnectionMode::Supervised).await;
            assert!(
                f.client
                    .state
                    .remember_token(Some("older-token".into()))
                    .is_ok()
            );
            let (guard, _gate, control) = operation();
            let entered = Arc::new(Semaphore::new(0));
            let release = Arc::new(Semaphore::new(0));
            // Both actual initial/replay paths use this awaited acquisition seam. Cancel while
            // it is suspended, then supply the successful acquired value before the post-fence.
            let call = tokio::spawn({
                let client = f.client.clone();
                let token = token.clone();
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    if initial {
                        client
                            .acquire_token(Some(&control), async move {
                                entered.add_permits(1);
                                release.acquire().await.unwrap().forget();
                                Ok((token, 0_u64))
                            })
                            .await
                            .map(|_| ())
                    } else {
                        client
                            .acquire_token(Some(&control), async move {
                                entered.add_permits(1);
                                release.acquire().await.unwrap().forget();
                                Ok((token, ()))
                            })
                            .await
                            .map(|_| ())
                    }
                }
            });
            permit(&entered, 1).await;
            drop(guard);
            release.add_permits(1);
            let result = call.await.unwrap();
            let Err(StreamableHttpError::Io(error)) = result else {
                panic!("acquisition cannot grant dispatch")
            };
            assert_eq!(
                error.get_ref().unwrap().downcast_ref::<OperationError>(),
                Some(&OperationError::Cancelled)
            );
            let before = f
                .store
                .read(|document| serde_json::to_value(document).unwrap())
                .unwrap();
            f.client
                .delete_session(
                    Arc::from(f.url.as_str()),
                    Arc::from("cleanup"),
                    None,
                    HashMap::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                *f.cleanup_headers.lock().unwrap(),
                vec![
                    token
                        .filter(|token| !token.is_empty())
                        .map(|token| format!("Bearer {token}"))
                ]
            );
            assert_eq!(f.posts.load(Ordering::SeqCst), 0);
            assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
            assert_eq!(
                f.store
                    .read(|document| serde_json::to_value(document).unwrap())
                    .unwrap(),
                before
            );
        }
    }
}

#[tokio::test]
async fn oauth_cleanup_keeps_old_acquisition_after_login_change_but_never_exports_it_to_replacement()
 {
    for logout in [false, true] {
        let f = Fixture::new(ClientKind::Public, ConnectionMode::Explicit).await;
        assert!(f.call(None, 1).await.unwrap().is_ok());
        if logout {
            f.store.sign_out("fixture", &f.url).unwrap();
        } else {
            save(&f.store, &f.url, &f.client_id, "new-login", 3600);
            *f.accepted.lock().unwrap() = "new-login".into();
        }
        let before = f
            .store
            .read(|document| serde_json::to_value(document).unwrap())
            .unwrap();
        f.client
            .delete_session(
                Arc::from(f.url.as_str()),
                Arc::from("cleanup"),
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            f.store
                .read(|document| serde_json::to_value(document).unwrap())
                .unwrap(),
            before
        );
        assert_eq!(
            *f.cleanup_headers.lock().unwrap(),
            vec![Some("Bearer initial".into())]
        );

        let settings = McpOAuthConfig {
            client_id: Some(f.client_id.clone()),
            auth_server_metadata_url: Some(format!("{}/metadata", f.url.trim_end_matches("/mcp"))),
            ..Default::default()
        };
        let replacement = oauth_http_client(
            "fixture",
            &f.url,
            Some(&settings),
            &McpClientAuth::default().with_oauth_store(f.store.clone()),
            ConnectionMode::Explicit,
        )
        .await
        .unwrap();
        let before = f
            .store
            .read(|document| serde_json::to_value(document).unwrap())
            .unwrap();
        replacement
            .delete_session(
                Arc::from(f.url.as_str()),
                Arc::from("replacement"),
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            f.store
                .read(|document| serde_json::to_value(document).unwrap())
                .unwrap(),
            before
        );
        assert_eq!(
            *f.cleanup_headers.lock().unwrap(),
            vec![Some("Bearer initial".into()), None]
        );

        // A separate active replacement reads current storage, never either closed cache.
        let active = oauth_http_client(
            "fixture",
            &f.url,
            Some(&settings),
            &McpClientAuth::default().with_oauth_store(f.store.clone()),
            ConnectionMode::Explicit,
        )
        .await
        .unwrap();
        let message =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":2,"method":"ping"})).unwrap();
        assert_eq!(
            active
                .post_message(
                    Arc::from(f.url.as_str()),
                    message,
                    None,
                    None,
                    HashMap::new()
                )
                .await
                .is_ok(),
            !logout
        );
        active
            .delete_session(
                Arc::from(f.url.as_str()),
                Arc::from("active-replacement"),
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            f.cleanup_headers.lock().unwrap().last().unwrap().as_deref(),
            (!logout).then_some("Bearer new-login")
        );
        assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn provider_cleanup_caches_before_abort_and_replacement_starts_empty() {
    let f = Fixture::new(ClientKind::Public, ConnectionMode::Supervised).await;
    let resolver = Arc::new(ProbeResolver::default());
    resolver.hold.store(true, Ordering::SeqCst);
    let client = ProviderAuthClient::new("fixture".into(), resolver.clone());
    let (guard, _gate, control) = operation();
    let mut message: ClientJsonRpcMessage =
        serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"})).unwrap();
    if let ClientJsonRpcMessage::Request(request) = &mut message {
        control.attach(&mut request.request);
    }
    let call = tokio::spawn({
        let client = client.clone();
        let uri = Arc::from(f.url.as_str());
        async move {
            client
                .post_message(uri, message, None, None, HashMap::new())
                .await
        }
    });
    permit(&resolver.entered, 1).await;
    drop(guard);
    resolver.release.add_permits(1);
    assert!(matches!(
        call.await.unwrap(),
        Err(StreamableHttpError::Io(_))
    ));
    client
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    let replacement = ProviderAuthClient::new("fixture".into(), resolver.clone());
    replacement
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("replacement"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *f.cleanup_headers.lock().unwrap(),
        vec![Some("Bearer initial".into()), None]
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_cleanup_absorbs_poisoned_cache_without_lookup_or_dispatch() {
    let f = Fixture::new(ClientKind::Public, ConnectionMode::Supervised).await;
    let state = f.client.state.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = state.last_token.lock().unwrap();
            panic!("poison cleanup cache");
        })
        .join()
        .is_err()
    );
    f.client
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();

    let resolver = Arc::new(ProbeResolver::default());
    let provider = ProviderAuthClient::new("fixture".into(), resolver.clone());
    let cache = provider.last_token.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = cache.lock().unwrap();
            panic!("poison cleanup cache");
        })
        .join()
        .is_err()
    );
    provider
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    assert!(f.cleanup_headers.lock().unwrap().is_empty());
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cleanup_reuses_last_token_without_oauth_refresh_or_provider_resolution() {
    let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Explicit).await;
    assert!(f.call(None, 1).await.unwrap().is_ok());
    save(&f.store, &f.url, &f.client_id, "expired", 0);
    f.client
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);

    let resolver = Arc::new(ProbeResolver::default());
    let provider = ProviderAuthClient::new("fixture".into(), resolver.clone());
    let message = serde_json::from_value(json!({"jsonrpc":"2.0","id":2,"method":"ping"})).unwrap();
    provider
        .post_message(
            Arc::from(f.url.as_str()),
            message,
            None,
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    provider
        .delete_session(
            Arc::from(f.url.as_str()),
            Arc::from("cleanup"),
            None,
            HashMap::new(),
        )
        .await
        .unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn newer_login_or_logout_wins_over_supervised_prepared_refresh() {
    for logout in [true, false] {
        let f = Fixture::new(ClientKind::Fixed, ConnectionMode::Supervised).await;
        f.reject();
        f.hold.store(true, Ordering::SeqCst);
        let (_guard, _gate, control) = operation();
        let call = f.call(Some(control), 1);
        permit(&f.entered, 1).await;
        if logout {
            f.store.sign_out("fixture", &f.url).unwrap();
        } else {
            save(&f.store, &f.url, &f.client_id, "new-login", 3600);
        }
        f.release.add_permits(1);
        assert!(call.await.unwrap().is_err());
        assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        let stored = CredentialStore::load(&f.store.for_server("fixture", &f.url).unwrap())
            .await
            .unwrap();
        if logout {
            assert!(stored.is_none());
        } else {
            assert_eq!(
                stored
                    .unwrap()
                    .token_response
                    .unwrap()
                    .access_token()
                    .secret(),
                "new-login"
            );
        }
    }
}
