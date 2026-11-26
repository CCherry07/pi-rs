//! Test server + passive client observer. No production retry/control policy is changed.
//! A wire attempt, an SDK GET method return, server EOF, and client-consumed EOF are distinct.
use super::*;
use axum::{extract::State, response::{IntoResponse, Response}};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, watch};

pub(crate) const SERVER_NAME: &str = "sse-fixture";
const CLIENT_ID: &str = "sse-fixture-client";
const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeReply {
    Success,
    AuthenticateThenSuccess,
    Unauthorized,
    InsufficientScope,
    Forbidden,
    TransientThenSuccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinalBody { Held, Eof }

#[derive(Clone, Debug)]
pub(crate) struct SseOptions {
    pub post_unauthorized_once: bool,
    pub resume_reply: ResumeReply,
    pub final_body: FinalBody,
    pub unchanged_access_token: bool,
    /// Server-supplied SSE retry hint, not an SDK policy override.
    pub retry_ms: u64,
    pub common_initial_reply: ResumeReply,
    pub common_reopen_reply: ResumeReply,
}

impl Default for SseOptions {
    fn default() -> Self {
        Self {
            post_unauthorized_once: true,
            resume_reply: ResumeReply::AuthenticateThenSuccess,
            final_body: FinalBody::Held,
            unchanged_access_token: false,
            retry_ms: 0,
            common_initial_reply: ResumeReply::Success,
            common_reopen_reply: ResumeReply::Success,
        }
    }
}

/// Configure holds BEFORE invocation. Lane labels must be unique per logical echo/probe call.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GatePoint {
    Exchange(usize),
    FirstPost401(String),
    /// After priming progress, before the POST response body's EOF. Open by default.
    PrimingEof(String),
    /// Before final response data in a successful resumed GET. Open by default.
    ResumeBody(String),
    /// After final response data; blocked iff FinalBody::Held, unless explicitly released.
    FinalEof(String),
    /// After common-stream prefix; blocked by default. Ordinal is global across sessions.
    CommonEof(usize),
    /// Before the probe JSON response. Open by default; never scripts OAuth/SSE for probe.
    Probe(String),
}

#[derive(Clone)]
pub(crate) struct Gate {
    open: watch::Sender<bool>,
    entered: watch::Sender<usize>,
}

impl Gate {
    fn new(open: bool) -> Self {
        Self { open: watch::channel(open).0, entered: watch::channel(0).0 }
    }
    pub fn release(&self) { self.open.send_replace(true); }
    pub async fn wait_entered(&self) {
        let mut seen = self.entered.subscribe();
        tokio::time::timeout(WAIT, async {
            while *seen.borrow_and_update() == 0 { seen.changed().await.unwrap(); }
        }).await.expect("fixture gate was not entered");
    }
    async fn pass(&self) {
        self.entered.send_modify(|count| *count += 1);
        let mut open = self.open.subscribe();
        while !*open.borrow_and_update() { open.changed().await.unwrap(); }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PostRecord {
    pub method: String,
    pub lane: Option<String>,
    pub id: Option<Value>,
    pub body: Value,
    pub session: Option<String>,
    pub authorization: Option<String>,
    pub progress_token: Option<Value>,
    pub authorized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GetKind { Response, CommonInitial, CommonReopen, Rejected }

#[derive(Clone, Debug)]
pub(crate) struct GetRecord {
    pub ordinal: usize,
    pub kind: GetKind,
    pub lane: Option<String>,
    pub session: Option<String>,
    pub cursor: Option<String>,
    pub authorization: Option<String>,
    /// Script choice at request arrival, not proof that the client received this status.
    pub planned_status: u16,
}

#[derive(Clone, Debug)]
pub(crate) struct ExchangeRecord {
    #[expect(
        dead_code,
        reason = "Retained passive diagnostic payload: AS-attempt identity; currently unasserted."
    )]
    pub ordinal: usize,
    pub grant: String,
    pub issued_access: Option<String>,
    pub issued_refresh: Option<String>,
    /// Endpoint handler prepared its response; NOT client credential-save completion.
    pub response_prepared: bool,
    /// Handler future dropped before preparing a response; the rotating grant stays consumed.
    pub handler_dropped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServerStream {
    Priming { lane: String, session: String },
    Response { lane: String, session: String },
    Common { ordinal: usize, session: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClientStream { Post(Option<Value>), Get(usize) }

#[derive(Clone, Debug)]
pub(crate) enum Event {
    ServerBodyStarted(ServerStream),
    ServerEof(ServerStream),
    ServerBodyDropped {
        stream: ServerStream,
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: server EOF/drop distinction; currently unasserted."
        )]
        eof_sent: bool,
    },
    ClientGetStarted {
        open: usize,
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: GET session input before dispatch; currently unasserted."
        )]
        session: Option<String>,
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: GET cursor input before dispatch; currently unasserted."
        )]
        cursor: Option<String>,
    },
    /// Wire/AS attempt vector ranges during this method; attributable wholly only in isolated cases.
    /// AS attempts are not callback/token-lookup counts or direct evidence of joining a flight.
    ClientGetReturned { open: usize, ok: bool, status: Option<u16>, wire_start: usize, wire_end: usize, exchange_start: usize, exchange_end: usize },
    ClientGetDropped {
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: dropped GET correlation; currently unasserted."
        )]
        open: usize,
    },
    ClientSseEof(
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: client EOF source identity; currently unasserted."
        )]
        ClientStream,
    ),
    ClientSseDropped {
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: dropped body source identity; currently unasserted."
        )]
        stream: ClientStream,
        #[expect(
            dead_code,
            reason = "Retained passive diagnostic payload: EOF-before-body-drop distinction; currently unasserted."
        )]
        eof_observed: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub posts: Vec<PostRecord>,
    pub gets: Vec<GetRecord>,
    pub exchanges: Vec<ExchangeRecord>,
    pub events: Vec<Event>,
    pub initializations: usize,
    pub executions: Vec<String>,
    pub cancellations: Vec<Value>,
    pub deletes: Vec<Option<String>>,
    pub max_exchange_overlap: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedTokens {
    pub access: String,
    pub refresh: Option<String>,
}

#[derive(Clone)]
struct Lane {
    id: Value,
    session: String,
    cursor: String,
    posts: usize,
    gets: usize,
}

#[derive(Default)]
struct CommonPhase { opened: bool, initial_attempts: usize, reopen_attempts: usize }

struct Data {
    snapshot: Snapshot,
    lanes: BTreeMap<String, Lane>,
    lane_replies: BTreeMap<String, ResumeReply>,
    common: BTreeMap<String, CommonPhase>,
    common_senders: BTreeMap<usize, (String, mpsc::Sender<String>)>,
    current_access: String,
    current_refresh: String,
    active_exchanges: usize,
}

struct Shared {
    origin: String,
    options: SseOptions,
    data: Mutex<Data>,
    gates: Mutex<BTreeMap<GatePoint, Gate>>,
    changed: watch::Sender<usize>,
    next_open: AtomicUsize,
}

impl Shared {
    fn change<T>(&self, f: impl FnOnce(&mut Data) -> T) -> T {
        let result = f(&mut self.data.lock().unwrap());
        self.changed.send_modify(|revision| *revision += 1);
        result
    }
    fn event(&self, event: Event) { self.change(|data| data.snapshot.events.push(event)); }
    fn gate(&self, point: GatePoint) -> Gate {
        let open = !matches!(point, GatePoint::CommonEof(_))
            && !(matches!(point, GatePoint::FinalEof(_)) && self.options.final_body == FinalBody::Held);
        self.gates.lock().unwrap().entry(point).or_insert_with(|| Gate::new(open)).clone()
    }
}

pub(crate) struct OAuthSseFixture {
    shared: Arc<Shared>,
    url: String,
    auth: McpClientAuth,
    settings: McpOAuthConfig,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for OAuthSseFixture { fn drop(&mut self) { self.task.abort(); } }

impl OAuthSseFixture {
    pub async fn start(options: SseOptions) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{origin}/mcp");
        let shared = Arc::new(Shared {
            origin: origin.clone(), options,
            data: Mutex::new(Data {
                snapshot: Snapshot::default(), lanes: BTreeMap::new(), lane_replies: BTreeMap::new(),
                common: BTreeMap::new(), common_senders: BTreeMap::new(),
                current_access: "initial".into(), current_refresh: "refresh-0".into(), active_exchanges: 0,
            }),
            gates: Mutex::new(BTreeMap::new()), changed: watch::channel(0).0,
            next_open: AtomicUsize::new(0),
        });
        let router = axum::Router::new()
            .route("/metadata", axum::routing::get(metadata))
            .route("/.well-known/oauth-authorization-server", axum::routing::get(metadata))
            .route("/token", axum::routing::post(exchange))
            .route("/mcp", axum::routing::post(post).get(get).delete(delete))
            .with_state(shared.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let store = McpOAuthCredentialStore::memory();
        store.save_tokens(SERVER_NAME, &url, CLIENT_ID, McpOAuthTokens {
            access_token: "initial".into(), token_type: "Bearer".into(),
            refresh_token: Some("refresh-0".into()), expires_in: Some(3600), scope: None,
        }).unwrap();
        Self {
            shared, url, auth: McpClientAuth::default().with_oauth_store(store), task,
            settings: McpOAuthConfig {
                client_id: Some(CLIENT_ID.into()), auth_server_metadata_url: Some(format!("{origin}/metadata")),
                ..Default::default()
            },
        }
    }
    pub fn name(&self) -> &'static str { SERVER_NAME }
    pub fn url(&self) -> &str { &self.url }
    pub fn config(&self) -> crate::McpServerConfig {
        crate::McpServerConfig::http(SERVER_NAME, &self.url).oauth(self.settings.clone())
    }
    pub fn client_auth(&self) -> McpClientAuth { self.auth.clone() }
    pub fn snapshot(&self) -> Snapshot { self.shared.data.lock().unwrap().snapshot.clone() }
    pub fn gate(&self, point: GatePoint) -> Gate { self.shared.gate(point) }
    pub fn hold(&self, point: GatePoint) -> Gate {
        let gate = self.gate(point);
        assert_eq!(*gate.entered.borrow(), 0, "configure holds before invocation");
        gate.open.send_replace(false);
        gate
    }
    pub fn hold_exchange(&self, ordinal: usize) -> Gate { self.hold(GatePoint::Exchange(ordinal)) }
    pub async fn wait_for(&self, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
        self.wait_for_with_timeout(WAIT, predicate).await.expect("fixture observation timed out")
    }
    pub async fn wait_for_with_timeout(&self, timeout: std::time::Duration, predicate: impl Fn(&Snapshot) -> bool)
        -> Result<Snapshot, tokio::time::error::Elapsed>
    {
        let mut changed = self.shared.changed.subscribe();
        tokio::time::timeout(timeout, async {
            loop {
                let snapshot = self.snapshot();
                if predicate(&snapshot) { return snapshot; }
                changed.changed().await.unwrap();
            }
        }).await
    }
    pub async fn saved_tokens(&self) -> Option<SavedTokens> {
        CredentialStore::load(&self.auth.oauth_store.for_server(SERVER_NAME, &self.url).unwrap())
            .await.unwrap().and_then(|stored| stored.token_response).map(|token| SavedTokens {
                access: token.access_token().secret().clone(),
                refresh: token.refresh_token().map(|token| token.secret().clone()),
            })
    }
    /// Enqueue a real common-stream notification. Client consumption must be observed separately.
    pub async fn emit_common_progress(&self, ordinal: usize, progress_token: Value, progress: u64) {
        let (cursor, sender) = self.shared.data.lock().unwrap().common_senders.get(&ordinal).unwrap().clone();
        sender.send(frame(&cursor, None, progress_message(progress_token, progress))).await.unwrap();
    }
    pub async fn observed_client(&self, mode: ConnectionMode) -> ObservedOAuthClient {
        ObservedOAuthClient {
            inner: oauth_http_client(SERVER_NAME, &self.url, Some(&self.settings), &self.auth, mode).await.unwrap(),
            shared: self.shared.clone(),
        }
    }
}

fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    // Keep an invalid text header observable rather than panicking before the GET trace.
    headers.get(name).map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
}
fn progress_message(token: Value, progress: u64) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":token,"progress":progress}})
}
fn frame(cursor: &str, retry: Option<u64>, message: Value) -> String {
    let retry = retry.map(|ms| format!("retry: {ms}\n")).unwrap_or_default();
    format!("id: {cursor}\n{retry}data: {message}\n\n")
}
fn rejection(reply: ResumeReply, attempt: usize) -> Option<(u16, Option<&'static str>)> {
    match reply {
        ResumeReply::Success => None,
        ResumeReply::AuthenticateThenSuccess if attempt > 1 => None,
        ResumeReply::AuthenticateThenSuccess | ResumeReply::Unauthorized => Some((401, None)),
        ResumeReply::InsufficientScope => Some((403, Some("Bearer error=insufficient_scope, scope=fixture-private"))),
        ResumeReply::Forbidden => Some((403, None)),
        ResumeReply::TransientThenSuccess if attempt == 1 => Some((503, None)),
        ResumeReply::TransientThenSuccess => None,
    }
}
fn rejected(status: u16, challenge: Option<&'static str>) -> Response {
    let mut response = (http::StatusCode::from_u16(status).unwrap(), "fixture-rejection").into_response();
    if let Some(challenge) = challenge {
        response.headers_mut().insert("www-authenticate", HeaderValue::from_static(challenge));
    }
    response
}
async fn metadata(State(shared): State<Arc<Shared>>) -> axum::Json<Value> {
    axum::Json(json!({"issuer":shared.origin,"authorization_endpoint":format!("{}/authorize", shared.origin),
        "token_endpoint":format!("{}/token", shared.origin),"token_endpoint_auth_methods_supported":["none"]}))
}
async fn exchange(State(shared): State<Arc<Shared>>, body: String) -> Response {
    let form: BTreeMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["client_id"], CLIENT_ID);
    let grant = form["refresh_token"].clone();
    let (ordinal, issued) = shared.change(|data| {
        let ordinal = data.snapshot.exchanges.len() + 1;
        let issued = if grant == data.current_refresh {
            let access = if shared.options.unchanged_access_token { "initial".into() } else { format!("access-{ordinal}") };
            let refresh = format!("refresh-{ordinal}");
            // Reserve this rotating grant now; a second simultaneous consumer is rejected.
            data.current_refresh = refresh.clone();
            Some((access, refresh))
        } else { None };
        data.active_exchanges += 1;
        data.snapshot.max_exchange_overlap = data.snapshot.max_exchange_overlap.max(data.active_exchanges);
        data.snapshot.exchanges.push(ExchangeRecord { ordinal, grant, issued_access: None, issued_refresh: None,
            response_prepared: false, handler_dropped: false });
        (ordinal, issued)
    });
    let mut observation = ExchangeObservation { shared: shared.clone(), ordinal, finished: false };
    shared.gate(GatePoint::Exchange(ordinal)).pass().await;
    shared.change(|data| {
        data.active_exchanges -= 1;
        let record = &mut data.snapshot.exchanges[ordinal - 1];
        record.response_prepared = true;
        if let Some((access, refresh)) = &issued {
            data.current_access = access.clone();
            record.issued_access = Some(access.clone());
            record.issued_refresh = Some(refresh.clone());
        }
    });
    observation.finished = true;
    match issued {
        Some((access, refresh)) => axum::Json(json!({"access_token":access,"refresh_token":refresh,"token_type":"Bearer","expires_in":3600})).into_response(),
        None => (http::StatusCode::BAD_REQUEST, axum::Json(json!({"error":"invalid_grant"}))).into_response(),
    }
}

struct ExchangeObservation { shared: Arc<Shared>, ordinal: usize, finished: bool }
impl Drop for ExchangeObservation {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.change(|data| {
                data.active_exchanges -= 1;
                data.snapshot.exchanges[self.ordinal - 1].handler_dropped = true;
            });
        }
    }
}

async fn post(State(shared): State<Arc<Shared>>, headers: http::HeaderMap, axum::Json(body): axum::Json<Value>) -> Response {
    let method = body["method"].as_str().unwrap().to_owned();
    let id = body.get("id").cloned();
    let lane = body.pointer("/params/arguments/lane").and_then(Value::as_str).map(str::to_owned);
    let progress_token = body.pointer("/params/_meta/progressToken").cloned();
    let session = header(&headers, "mcp-session-id");
    let authorization = header(&headers, "authorization");
    let authorized = shared.change(|data| {
        let authorized = authorization.as_deref() == Some(format!("Bearer {}", data.current_access).as_str());
        data.snapshot.posts.push(PostRecord {
            method: method.clone(), lane: lane.clone(), id: id.clone(), body: body.clone(), session: session.clone(),
            authorization, progress_token: progress_token.clone(), authorized,
        });
        authorized
    });
    if method == "server/discover" { return rejected(400, None); }
    if method == "initialize" {
        let session = shared.change(|data| {
            data.snapshot.initializations += 1;
            let session = format!("session-{}", data.snapshot.initializations);
            data.common.insert(session.clone(), CommonPhase::default());
            session
        });
        return ([("mcp-session-id", session)], axum::Json(json!({"jsonrpc":"2.0","id":id,
            "result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"sse-fixture","version":"1"}}}))).into_response();
    }
    if method == "notifications/cancelled" {
        shared.change(|data| data.snapshot.cancellations.push(body["params"]["requestId"].clone()));
    }
    let Some(id) = id else { return http::StatusCode::ACCEPTED.into_response(); };
    if method == "tools/list" {
        return axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
            {"name":"echo","description":"SSE echo","inputSchema":{"type":"object","properties":{"lane":{"type":"string"}},"required":["lane"]}},
            {"name":"probe","description":"JSON credential/progress probe","inputSchema":{"type":"object","properties":{"lane":{"type":"string"}},"required":["lane"]}}
        ]}})).into_response();
    }
    if method != "tools/call" { return axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{}})).into_response(); }
    let lane = lane.expect("fixture calls need a lane label");
    if body["params"]["name"] == "probe" {
        shared.gate(GatePoint::Probe(lane.clone())).pass().await;
        return axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":format!("probe:{lane}")}],"isError":!authorized}})).into_response();
    }
    assert_eq!(body["params"]["name"], "echo");
    // Progress-based priming requires genuine request metadata, never a fabricated token.
    let Some(progress_token) = progress_token else {
        return (http::StatusCode::BAD_REQUEST, "fixture echo requires an actual progressToken").into_response();
    };
    let session = session.expect("sessionful fixture");
    let (attempt, cursor) = shared.change(|data| {
        let next = data.lanes.len() + 1;
        let state = data.lanes.entry(lane.clone()).or_insert_with(|| Lane {
            id: id.clone(), session: session.clone(), cursor: format!("response/{session}/{next}"), posts: 0, gets: 0,
        });
        assert_eq!(state.id, id, "POST replay changed the SDK-generated id");
        state.posts += 1;
        (state.posts, state.cursor.clone())
    });
    if shared.options.post_unauthorized_once && attempt == 1 {
        shared.gate(GatePoint::FirstPost401(lane)).pass().await;
        return rejected(401, None);
    }
    if !authorized { return rejected(401, None); }
    shared.change(|data| data.snapshot.executions.push(lane.clone()));
    let priming = frame(&cursor, Some(shared.options.retry_ms), progress_message(progress_token, 1));
    sse_response(shared.clone(), ServerStream::Priming { lane: lane.clone(), session }, priming, None,
        shared.gate(GatePoint::PrimingEof(lane)), None)
}

enum GetRoute {
    Reject(u16, Option<&'static str>),
    Response { name: String, lane: Lane, extra: bool },
    Common(String),
}

async fn get(State(shared): State<Arc<Shared>>, headers: http::HeaderMap) -> Response {
    let session = header(&headers, "mcp-session-id");
    let cursor = header(&headers, "last-event-id");
    let authorization = header(&headers, "authorization");
    // Append EVERY wire GET, including missing-session and unknown/wrong-session cursors.
    // Classification and append share one lock, so observers never see a provisional kind.
    let (ordinal, route) = shared.change(|data| {
        let ordinal = data.snapshot.gets.len() + 1;
        let mut record = GetRecord { ordinal, kind: GetKind::Rejected, lane: None,
            session: session.clone(), cursor: cursor.clone(), authorization: authorization.clone(), planned_status: 400 };
        // Only this SERVER resolves its own issued cursor, never client RequestControl.
        let owner = data.lanes.iter().find_map(|(name, lane)| {
            cursor.as_deref().is_some_and(|cursor| cursor == lane.cursor || cursor == format!("{}/final", lane.cursor))
                .then(|| name.clone())
        });
        record.lane = owner.clone();
        let authorized = authorization.as_deref() == Some(format!("Bearer {}", data.current_access).as_str());
        let route = if let Some(session) = session.as_deref() {
            if !data.common.contains_key(session) {
                GetRoute::Reject(400, None)
            } else if let Some(name) = owner {
                if data.lanes[&name].session != session {
                    GetRoute::Reject(400, None)
                } else {
                    record.kind = GetKind::Response;
                    let script = data.lane_replies.get(&name).copied().unwrap_or(shared.options.resume_reply);
                    let lane = data.lanes.get_mut(&name).unwrap();
                    lane.gets += 1;
                    let reject = rejection(script, lane.gets).or_else(|| (!authorized).then_some((401, None)));
                    record.planned_status = reject.map_or(200, |value| value.0);
                    match reject {
                        Some((status, challenge)) => GetRoute::Reject(status, challenge),
                        None => GetRoute::Response { name, lane: lane.clone(), extra: cursor.as_deref().unwrap().ends_with("/final") },
                    }
                }
            } else if session.is_empty() || cursor.as_deref().is_some_and(|cursor|
                !data.common_senders.values().any(|(issued, _)| issued == cursor && issued.starts_with(&format!("common/{session}/")))) {
                GetRoute::Reject(400, None)
            } else {
                let phase = data.common.entry(session.to_owned()).or_default();
                let (kind, script, attempt) = if phase.opened {
                    phase.reopen_attempts += 1;
                    (GetKind::CommonReopen, shared.options.common_reopen_reply, phase.reopen_attempts)
                } else {
                    phase.initial_attempts += 1;
                    (GetKind::CommonInitial, shared.options.common_initial_reply, phase.initial_attempts)
                };
                let reject = rejection(script, attempt).or_else(|| (!authorized).then_some((401, None)));
                if reject.is_none() { phase.opened = true; }
                record.kind = kind;
                record.planned_status = reject.map_or(200, |value| value.0);
                match reject {
                    Some((status, challenge)) => GetRoute::Reject(status, challenge),
                    None => GetRoute::Common(session.to_owned()),
                }
            }
        } else { GetRoute::Reject(400, None) };
        data.snapshot.gets.push(record);
        (ordinal, route)
    });
    match route {
        GetRoute::Reject(status, challenge) => rejected(status, challenge),
        GetRoute::Response { name, lane, extra } => {
            let kind = ServerStream::Response { lane: name.clone(), session: lane.session.clone() };
            // Forbidden post-answer GET is recorded and held without another RPC result/EOF.
            let final_frame = if extra { ": forbidden post-answer GET held\n\n".into() } else {
                let result = json!({"jsonrpc":"2.0","id":lane.id,"result":{"content":[{"type":"text","text":format!("echo:{name}")}]}});
                frame(&format!("{}/final", lane.cursor), Some(shared.options.retry_ms), result)
            };
            let eof = if extra { Gate::new(false) } else { shared.gate(GatePoint::FinalEof(name.clone())) };
            sse_response(shared.clone(), kind, final_frame, Some(shared.gate(GatePoint::ResumeBody(name))), eof, None)
        }
        GetRoute::Common(session) => {
            let cursor = format!("common/{session}/{ordinal}");
            let (send, receive) = mpsc::channel(8);
            shared.change(|data| { data.common_senders.insert(ordinal, (cursor.clone(), send)); });
            sse_response(shared.clone(), ServerStream::Common { ordinal, session },
                format!("id: {cursor}\n: common fixture stream\n\n"), None, shared.gate(GatePoint::CommonEof(ordinal)), Some(receive))
        }
    }
}
async fn delete(State(shared): State<Arc<Shared>>, headers: http::HeaderMap) -> http::StatusCode {
    shared.change(|data| data.snapshot.deletes.push(header(&headers, "mcp-session-id")));
    http::StatusCode::NO_CONTENT
}

struct BodyState {
    shared: Arc<Shared>, kind: ServerStream, frame: Option<String>, before: Option<Gate>, eof: Gate,
    messages: Option<mpsc::Receiver<String>>, eof_sent: bool, prefixed: bool,
}
impl Drop for BodyState {
    fn drop(&mut self) { self.shared.event(Event::ServerBodyDropped { stream: self.kind.clone(), eof_sent: self.eof_sent }); }
}
fn sse_response(shared: Arc<Shared>, kind: ServerStream, frame: String, before: Option<Gate>, eof: Gate,
    messages: Option<mpsc::Receiver<String>>) -> Response
{
    shared.event(Event::ServerBodyStarted(kind.clone()));
    let state = BodyState { shared, kind, frame: Some(frame), before, eof, messages, eof_sent: false, prefixed: false };
    let body = stream::unfold(state, |mut state| async move {
        if !state.prefixed {
            state.prefixed = true;
            return Some((Ok::<_, std::convert::Infallible>(": open\n\n".to_owned()), state));
        }
        if let Some(frame) = state.frame.take() {
            if let Some(gate) = state.before.take() { gate.pass().await; }
            return Some((Ok(frame), state));
        }
        if let Some(messages) = state.messages.as_mut() {
            tokio::select! {
                biased;
                _ = state.eof.pass() => {},
                message = messages.recv() => {
                    if let Some(message) = message { return Some((Ok(message), state)); }
                }
            }
        } else { state.eof.pass().await; }
        state.eof_sent = true;
        state.shared.event(Event::ServerEof(state.kind.clone()));
        None
    });
    ([("content-type", "text/event-stream")], axum::body::Body::from_stream(body)).into_response()
}

#[derive(Clone)]
pub(crate) struct ObservedOAuthClient { inner: OAuthHttpClient, shared: Arc<Shared> }

struct GetObservation { shared: Arc<Shared>, open: usize, returned: bool, wire_start: usize, exchange_start: usize }
impl Drop for GetObservation {
    fn drop(&mut self) { if !self.returned { self.shared.event(Event::ClientGetDropped { open: self.open }); } }
}
struct StreamObservation { shared: Arc<Shared>, source: ClientStream, eof: bool }
impl Drop for StreamObservation {
    fn drop(&mut self) { self.shared.event(Event::ClientSseDropped { stream: self.source.clone(), eof_observed: self.eof }); }
}
fn observe_stream(stream: BoxStream<'static, Result<Sse, SseError>>, shared: Arc<Shared>, source: ClientStream)
    -> BoxStream<'static, Result<Sse, SseError>>
{
    stream::unfold((stream, StreamObservation { shared, source, eof: false }), |(mut stream, mut observation)| async move {
        match stream.next().await {
            Some(event) => Some((event, (stream, observation))),
            None => { observation.eof = true; observation.shared.event(Event::ClientSseEof(observation.source.clone())); None }
        }
    }).boxed()
}
impl ObservedOAuthClient {
    fn message_id(message: &ClientJsonRpcMessage) -> Option<Value> {
        match message {
            ClientJsonRpcMessage::Request(request) => Some(serde_json::to_value(&request.id).unwrap()),
            _ => None,
        }
    }
    fn observe_post(&self, id: Option<Value>, result: Result<StreamableHttpPostResponse, HttpError>) -> Result<StreamableHttpPostResponse, HttpError> {
        match result {
            Ok(StreamableHttpPostResponse::Sse(stream, session)) => {
                Ok(StreamableHttpPostResponse::Sse(observe_stream(stream, self.shared.clone(), ClientStream::Post(id)), session))
            }
            result => result,
        }
    }
    fn begin_get(&self, session: &Option<Arc<str>>, cursor: &Option<String>) -> GetObservation {
        let open = self.shared.next_open.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared.event(Event::ClientGetStarted { open, session: session.as_deref().map(str::to_owned), cursor: cursor.clone() });
        let (wire_start, exchange_start) = {
            let data = self.shared.data.lock().unwrap();
            (data.snapshot.gets.len(), data.snapshot.exchanges.len())
        };
        GetObservation { shared: self.shared.clone(), open, returned: false, wire_start, exchange_start }
    }
    fn end_get(&self, mut observation: GetObservation, result: Result<BoxStream<'static, Result<Sse, SseError>>, HttpError>)
        -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError>
    {
        let status = match &result { Err(StreamableHttpError::Client(error)) => error.status().map(|status| status.as_u16()), _ => None };
        observation.returned = true;
        let (wire_end, exchange_end) = {
            let data = self.shared.data.lock().unwrap();
            (data.snapshot.gets.len(), data.snapshot.exchanges.len())
        };
        self.shared.event(Event::ClientGetReturned { open: observation.open, ok: result.is_ok(), status,
            wire_start: observation.wire_start, wire_end, exchange_start: observation.exchange_start, exchange_end });
        result.map(|stream| observe_stream(stream, self.shared.clone(), ClientStream::Get(observation.open)))
    }
}
impl StreamableHttpClient for ObservedOAuthClient {
    type Error = PiHttpClientError;
    async fn post_message(&self, uri: Arc<str>, message: ClientJsonRpcMessage, session: Option<Arc<str>>, token: Option<String>, headers: HashMap<HeaderName, HeaderValue>) -> Result<StreamableHttpPostResponse, HttpError> {
        let id = Self::message_id(&message);
        let result = self.inner.post_message(uri, message, session, token, headers).await;
        self.observe_post(id, result)
    }
    async fn post_message_with_max_sse_event_size(&self, uri: Arc<str>, message: ClientJsonRpcMessage, session: Option<Arc<str>>, token: Option<String>, headers: HashMap<HeaderName, HeaderValue>, limit: usize) -> Result<StreamableHttpPostResponse, HttpError> {
        let id = Self::message_id(&message);
        let result = self.inner.post_message_with_max_sse_event_size(uri, message, session, token, headers, limit).await;
        self.observe_post(id, result)
    }
    async fn get_stream(&self, uri: Arc<str>, session: Option<Arc<str>>, cursor: Option<String>, token: Option<String>, headers: HashMap<HeaderName, HeaderValue>) -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError> {
        let observation = self.begin_get(&session, &cursor);
        let result = self.inner.get_stream(uri, session, cursor, token, headers).await;
        self.end_get(observation, result)
    }
    async fn get_stream_with_max_sse_event_size(&self, uri: Arc<str>, session: Option<Arc<str>>, cursor: Option<String>, token: Option<String>, headers: HashMap<HeaderName, HeaderValue>, limit: usize) -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError> {
        let observation = self.begin_get(&session, &cursor);
        let result = self.inner.get_stream_with_max_sse_event_size(uri, session, cursor, token, headers, limit).await;
        self.end_get(observation, result)
    }
    async fn delete_session(&self, uri: Arc<str>, session: Arc<str>, token: Option<String>, headers: HashMap<HeaderName, HeaderValue>) -> Result<(), HttpError> {
        self.inner.delete_session(uri, session, token, headers).await
    }
}
