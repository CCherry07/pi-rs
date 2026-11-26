use super::*;
use axum::{http::StatusCode, response::IntoResponse};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::Semaphore;

struct Fixture {
    dir: tempfile::TempDir,
    url: String,
    store: McpOAuthCredentialStore,
    client: Arc<OAuthHttpClient>,
    secrets: Arc<Mutex<Vec<String>>>,
    accepted: Arc<Mutex<String>>,
    requests: Arc<Mutex<BTreeMap<u64, usize>>>,
    token_calls: Arc<AtomicUsize>,
    reject: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
    hold: Arc<AtomicBool>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    rejected: Arc<Semaphore>,
    hold_rejections: Arc<AtomicBool>,
    reject_release: Arc<Semaphore>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let metadata = json!({"issuer":origin,"authorization_endpoint":format!("{origin}/authorize"),
            "token_endpoint":format!("{origin}/token"),"token_endpoint_auth_methods_supported":["client_secret_post"]});
        let secrets = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(Mutex::new("initial".to_owned()));
        let requests = Arc::new(Mutex::new(BTreeMap::new()));
        let token_calls = Arc::new(AtomicUsize::new(0));
        let reject = Arc::new(AtomicBool::new(false));
        let fail = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let rejected = Arc::new(Semaphore::new(0));
        let hold_rejections = Arc::new(AtomicBool::new(false));
        let reject_release = Arc::new(Semaphore::new(0));
        let app = axum::Router::new()
            .route("/metadata", axum::routing::get(move || { let value = metadata.clone(); async { axum::Json(value) } }))
            .route("/token", axum::routing::post({
                let (secrets, accepted, token_calls, fail, hold, entered, release) = (
                    secrets.clone(), accepted.clone(), token_calls.clone(), fail.clone(), hold.clone(), entered.clone(), release.clone());
                move |body: String| {
                    let (secrets, accepted, token_calls, fail, hold, entered, release) = (
                        secrets.clone(), accepted.clone(), token_calls.clone(), fail.clone(), hold.clone(), entered.clone(), release.clone());
                    async move {
                        let form = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect::<BTreeMap<_, _>>();
                        assert_eq!(form["grant_type"], "refresh_token");
                        assert_eq!(form["client_id"], "fixture-client");
                        assert!(form.contains_key("resource"));
                        secrets.lock().unwrap().push(form["client_secret"].clone());
                        let count = token_calls.fetch_add(1, Ordering::SeqCst) + 1;
                        entered.add_permits(1);
                        if hold.load(Ordering::SeqCst) { release.acquire().await.unwrap().forget(); }
                        if fail.load(Ordering::SeqCst) {
                            return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error":"server_error","error_description":"fixture-private-endpoint-echo"}))).into_response();
                        }
                        let token = format!("fresh-{count}");
                        *accepted.lock().unwrap() = token.clone();
                        axum::Json(json!({"access_token":token,"token_type":"Bearer","refresh_token":format!("rotated-refresh-{count}"),"expires_in":3600})).into_response()
                    }
                }
            }))
            .route("/mcp", axum::routing::post({
                let (accepted, reject, requests, rejected, hold_rejections, reject_release) = (accepted.clone(), reject.clone(), requests.clone(), rejected.clone(), hold_rejections.clone(), reject_release.clone());
                move |headers: axum::http::HeaderMap, axum::Json(message): axum::Json<Value>| {
                    let id = message["id"].as_u64().unwrap();
                    *requests.lock().unwrap().entry(id).or_insert(0) += 1;
                    let token = format!("Bearer {}", accepted.lock().unwrap());
                    let allowed = !reject.load(Ordering::SeqCst) && headers.get("authorization").and_then(|value| value.to_str().ok()) == Some(&token);
                    let (rejected, hold_rejections, reject_release) = (rejected.clone(), hold_rejections.clone(), reject_release.clone());
                    async move {
                        if !allowed {
                            rejected.add_permits(1);
                            if hold_rejections.load(Ordering::SeqCst) { reject_release.acquire().await.unwrap().forget(); }
                            return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer error=invalid_token")]).into_response();
                        }
                        axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{}})).into_response()
                    }
                }
            }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let store = McpOAuthCredentialStore::new(dir.path().join("auth.json"));
        save(&store, &url, 3600);
        std::fs::write(dir.path().join("secret"), "initial-secret\n").unwrap();
        let mut auth = McpClientAuth::default().with_oauth_store(store.clone());
        auth.local_values = Some(crate::config::LocalValues {
            cwd: dir.path().into(),
            allow_commands: true,
        });
        let settings = McpOAuthConfig {
            client_id: Some("fixture-client".into()),
            client_secret: Some(
                "!printf x >> helper-runs; read value < secret; printf '%s' \"$value\"".into(),
            ),
            auth_server_metadata_url: Some(format!("{origin}/metadata")),
            ..Default::default()
        };
        let client = Arc::new(
            oauth_http_client(
                "fixture",
                &url,
                Some(&settings),
                &auth,
                ConnectionMode::Explicit,
            )
            .await
            .unwrap(),
        );
        assert!(
            !dir.path().join("helper-runs").exists(),
            "connection setup must not need a secret"
        );
        Self {
            dir,
            url,
            store,
            client,
            secrets,
            accepted,
            requests,
            token_calls,
            reject,
            fail,
            hold,
            entered,
            release,
            rejected,
            hold_rejections,
            reject_release,
            server,
        }
    }

    fn reject_current(&self) {
        *self.accepted.lock().unwrap() = "requires-refresh".into();
    }

    fn call(
        &self,
        id: u64,
    ) -> tokio::task::JoinHandle<
        Result<StreamableHttpPostResponse, HttpError>,
    > {
        let client = self.client.clone();
        let uri: Arc<str> = self.url.clone().into();
        tokio::spawn(async move {
            client
                .post_message(
                    uri,
                    serde_json::from_value(json!({"jsonrpc":"2.0","id":id,"method":"ping"}))
                        .unwrap(),
                    None,
                    None,
                    HashMap::new(),
                )
                .await
        })
    }

    fn helper_runs(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("helper-runs")).unwrap()
    }
}

fn save(store: &McpOAuthCredentialStore, url: &str, expires: u64) {
    store
        .save_tokens(
            "fixture",
            url,
            "fixture-client",
            McpOAuthTokens {
                access_token: "initial".into(),
                token_type: "Bearer".into(),
                refresh_token: Some("initial-refresh".into()),
                expires_in: Some(expires),
                scope: None,
            },
        )
        .unwrap();
}

async fn permits(semaphore: &Semaphore, count: u32) {
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
async fn usable_token_needs_no_helper_and_expiry_and_401_reread_rotating_secret() {
    let f = Fixture::new().await;
    std::fs::remove_file(f.dir.path().join("secret")).unwrap();
    assert!(f.call(1).await.unwrap().is_ok());
    assert!(!f.dir.path().join("helper-runs").exists());
    std::fs::write(f.dir.path().join("secret"), "rotated-one\n").unwrap();
    save(&f.store, &f.url, 0);
    assert!(f.call(2).await.unwrap().is_ok());
    assert_eq!(
        f.requests.lock().unwrap()[&2],
        1,
        "expired token refreshes before sending"
    );
    std::fs::write(f.dir.path().join("secret"), "rotated-two\n").unwrap();
    f.reject_current();
    assert!(f.call(3).await.unwrap().is_ok());
    assert_eq!(f.requests.lock().unwrap()[&3], 2);
    assert_eq!(&*f.secrets.lock().unwrap(), &["rotated-one", "rotated-two"]);
    assert_eq!(f.helper_runs(), b"xx");
}

#[tokio::test]
async fn concurrent_requests_share_refresh_and_cancelling_a_waiter_does_not_cancel_the_owner() {
    let f = Fixture::new().await;
    f.reject_current();
    f.hold.store(true, Ordering::SeqCst);
    let first = f.call(1);
    permits(&f.entered, 1).await;
    let others = (2..10).map(|id| f.call(id)).collect::<Vec<_>>();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    f.release.add_permits(1);
    for task in others {
        assert!(task.await.unwrap().is_ok());
    }
    assert_eq!(f.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.helper_runs(), b"x");
    for id in 2..10 {
        assert_eq!(f.requests.lock().unwrap()[&id], 1, "token acquisition waits for the pending exchange");
    }
    let stored = CredentialStore::load(&f.store.for_server("fixture", &f.url).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.token_response.unwrap().access_token().secret(),
        "fresh-1"
    );
}

#[tokio::test]
async fn failed_refresh_is_shared_redacted_and_preserves_the_store_then_can_retry() {
    let f = Fixture::new().await;
    f.reject_current();
    f.fail.store(true, Ordering::SeqCst);
    f.hold.store(true, Ordering::SeqCst);
    let before = std::fs::read(f.dir.path().join("auth.json")).unwrap();
    f.hold_rejections.store(true, Ordering::SeqCst);
    let calls = (1..9).map(|id| f.call(id)).collect::<Vec<_>>();
    permits(&f.rejected, 8).await;
    f.reject_release.add_permits(8);
    permits(&f.entered, 1).await;
    f.release.add_permits(1);
    for task in calls {
        let error = task.await.unwrap().err().unwrap().to_string();
        assert!(error.contains("refresh failed"), "{error}");
        assert!(!error.contains("fixture-private") && !error.contains("initial-secret"));
    }
    assert_eq!(f.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(f.dir.path().join("auth.json")).unwrap(),
        before
    );
    f.fail.store(false, Ordering::SeqCst);
    f.hold.store(false, Ordering::SeqCst);
    f.hold_rejections.store(false, Ordering::SeqCst);
    assert!(f.call(9).await.unwrap().is_ok());
    assert_eq!(f.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.helper_runs(), b"xx");
}

#[tokio::test]
async fn helper_failure_is_redacted_without_a_token_request_or_store_mutation() {
    let f = Fixture::new().await;
    f.reject_current();
    std::fs::remove_file(f.dir.path().join("secret")).unwrap();
    let before = std::fs::read(f.dir.path().join("auth.json")).unwrap();
    let error = f.call(1).await.unwrap().err().unwrap().to_string();
    assert!(error.contains("empty"));
    assert!(!error.contains("read value") && !error.contains("helper-runs"));
    assert_eq!(f.token_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(f.dir.path().join("auth.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn detached_refresh_cannot_resurrect_logout_or_overwrite_a_new_login() {
    for logout in [true, false] {
        let f = Fixture::new().await;
        f.reject_current();
        f.hold.store(true, Ordering::SeqCst);
        let call = f.call(1);
        permits(&f.entered, 1).await;
        if logout {
            f.store.sign_out("fixture", &f.url).unwrap();
        } else {
            f.store
                .save_tokens(
                    "fixture",
                    &f.url,
                    "fixture-client",
                    McpOAuthTokens {
                        access_token: "new-login".into(),
                        token_type: "Bearer".into(),
                        refresh_token: Some("new-login-refresh".into()),
                        expires_in: Some(7200),
                        scope: None,
                    },
                )
                .unwrap();
        }
        let externally_changed = std::fs::read(f.dir.path().join("auth.json")).unwrap();
        f.release.add_permits(1);
        assert!(call.await.unwrap().is_err());
        assert_eq!(
            std::fs::read(f.dir.path().join("auth.json")).unwrap(),
            externally_changed
        );
    }
}

#[tokio::test]
async fn second_401_is_returned_without_a_refresh_loop() {
    let f = Fixture::new().await;
    f.reject.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.call(1).await.unwrap(),
        Err(StreamableHttpError::Client(error)) if error.auth_rejection().is_some()
    ));
    assert_eq!(f.requests.lock().unwrap()[&1], 2);
    assert_eq!(f.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.helper_runs(), b"x");
}
