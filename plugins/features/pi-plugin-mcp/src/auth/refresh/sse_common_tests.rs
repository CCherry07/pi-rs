//! Common INITIAL and established-common REOPEN are separate scenarios.
//! LOCAL-BOUND checks stored method intervals; ORACLE-DESIRED checks the entire ledger.
//! Default SDK policy is untouched. Five quiet seconds are bounded negative evidence,
//! not common-worker termination, transport-error delivery, or lifecycle quiescence.
use super::ConnectionMode;
use super::sse_fixture::*;
use rmcp::{ClientHandler, RoleClient};
use rmcp::model::{ClientRequest, ProgressNotificationParam};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, NotificationContext, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig};
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;

const BOUND: Duration = Duration::from_secs(5);

struct ProgressClient { updates: mpsc::UnboundedSender<ProgressNotificationParam> }

impl ClientHandler for ProgressClient {
    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let _ = self.updates.send(params);
    }
}

struct CommonCase {
    fixture: OAuthSseFixture,
    service: RunningService<RoleClient, ProgressClient>,
    progress: mpsc::UnboundedReceiver<ProgressNotificationParam>,
}

impl CommonCase {
    async fn start(options: SseOptions) -> Self {
        let fixture = OAuthSseFixture::start(options).await;
        let transport = StreamableHttpClientTransport::with_client(
            fixture.observed_client(ConnectionMode::Supervised).await,
            StreamableHttpClientTransportConfig::with_uri(fixture.url().to_owned())
                .reinit_on_expired_session(false),
        );
        let (updates, progress) = mpsc::unbounded_channel();
        // Only initialize the real SDK service here. Waiting for a successful common GET
        // would make INITIAL rejection/503 setup impossible and hide the distinction.
        let service = tokio::time::timeout(BOUND,
            ProgressClient { updates }.serve_with_lifecycle(transport, ClientLifecycleMode::Initialize))
            .await.unwrap().unwrap();
        Self { fixture, service, progress }
    }

    async fn successful_open(&self, expected_successes: usize) -> usize {
        let snapshot = self.fixture.wait_for(|snapshot| snapshot.events.iter().filter(|event|
            matches!(event, Event::ClientGetReturned { ok: true, .. })).count() >= expected_successes).await;
        // In these isolated cases wire_end is the ordinal of that successful wire GET.
        snapshot.events.iter().filter_map(|event| match event {
            Event::ClientGetReturned { ok: true, wire_end, .. } => Some(*wire_end),
            _ => None,
        }).nth(expected_successes - 1).unwrap()
    }

    async fn probe(&mut self, lane: &str, common_ordinal: Option<usize>) {
        let gate = self.fixture.hold(GatePoint::Probe(lane.into()));
        let request: ClientRequest = serde_json::from_value(json!({"method":"tools/call","params":{
            "name":"probe","arguments":{"lane":lane}
        }})).unwrap();
        let handle = self.service.peer().send_cancellable_request(request, Default::default()).await.unwrap();
        let progress_token = handle.progress_token.clone();
        gate.wait_entered().await;
        let snapshot = self.fixture.snapshot();
        let posts: Vec<_> = snapshot.posts.iter().filter(|post| post.lane.as_deref() == Some(lane)).collect();
        assert_eq!(posts.len(), 1, "probe must not be reexecuted");
        assert!(posts[0].authorized, "server validates the actual bearer");
        assert_eq!(posts[0].progress_token, Some(serde_json::to_value(&progress_token).unwrap()));
        assert_eq!(posts[0].id, Some(serde_json::to_value(&handle.id).unwrap()));
        assert_eq!(posts[0].session.as_deref(), Some("session-1"));
        let saved = self.fixture.saved_tokens().await.unwrap();
        assert_eq!(posts[0].authorization, Some(format!("Bearer {}", saved.access)));
        if let Some(ordinal) = common_ordinal {
            self.fixture.emit_common_progress(ordinal, serde_json::to_value(&progress_token).unwrap(), 777).await;
            tokio::time::timeout(BOUND, async {
                loop {
                    let update = self.progress.recv().await.expect("progress receiver closed");
                    if update.progress_token == progress_token {
                        assert_eq!(update.progress, 777.0);
                        break;
                    }
                }
            }).await.expect("SDK must consume actual common progress for the pending probe");
        }
        gate.release();
        let result = tokio::time::timeout(BOUND, handle.await_response()).await.unwrap().unwrap();
        let result = serde_json::to_value(result).unwrap();
        assert_eq!(result["content"][0]["text"], format!("probe:{lane}"));
        assert_eq!(result["isError"], false);
    }

    async fn establish_then_end_common(&mut self) {
        let ordinal = self.successful_open(1).await;
        assert_eq!(ordinal, 1, "this phase starts with an unchallenged common GET");
        // Public SDK progress delivery, not enqueue/server EOF, proves this stream usable
        // and its issued common/session-1/1 cursor processed before EOF is released.
        self.probe("before-common-eof", Some(ordinal)).await;
        self.fixture.gate(GatePoint::CommonEof(ordinal)).release();
    }

    async fn close(&mut self) {
        self.service.close_with_timeout(Duration::from_secs(2)).await.unwrap()
            .expect("SDK cleanup timed out; Ok(None) is not a completed teardown");
    }
}

#[derive(Clone, Copy)]
struct FailedOpening {
    open: usize,
    wire_start: usize,
    wire_end: usize,
    exchange_start: usize,
    exchange_end: usize,
}

fn failed_opening(snapshot: &Snapshot, status: u16) -> Option<FailedOpening> {
    snapshot.events.iter().find_map(|event| match event {
        Event::ClientGetReturned { open, ok: false, status: Some(observed), wire_start, wire_end,
            exchange_start, exchange_end } if *observed == status => Some(FailedOpening {
                open: *open, wire_start: *wire_start, wire_end: *wire_end,
                exchange_start: *exchange_start, exchange_end: *exchange_end,
            }),
        _ => None,
    })
}

fn rejection_bounds(reply: ResumeReply) -> (u16, usize, usize) {
    match reply {
        ResumeReply::Unauthorized => (401, 2, 1),
        ResumeReply::InsufficientScope | ResumeReply::Forbidden => (403, 1, 0),
        _ => panic!("not a terminal common GET script"),
    }
}

fn assert_common_only(snapshot: &Snapshot) {
    assert_eq!(snapshot.initializations, 1);
    assert!(snapshot.executions.is_empty(), "no echo RPC exists in these common-only cases");
    assert!(snapshot.cancellations.is_empty());
    assert!(snapshot.posts.iter().all(|post| post.method != "tools/call" || post.body["params"]["name"] == "probe"));
    assert!(snapshot.gets.iter().all(|get| matches!(get.kind, GetKind::CommonInitial | GetKind::CommonReopen)),
        "including rejected/unclassified wire GETs in the ledger");
    for get in &snapshot.gets {
        assert_eq!(get.session.as_deref(), Some("session-1"));
        assert_eq!(get.cursor.as_deref(), if get.kind == GetKind::CommonInitial { None } else { Some("common/session-1/1") });
    }
}

fn assert_local_bound(snapshot: &Snapshot, failure: FailedOpening, reopened: bool, reply: ResumeReply) {
    let (status, attempts, exchanges) = rejection_bounds(reply);
    assert_eq!(failure.open, if reopened { 2 } else { 1 });
    assert_eq!(failure.wire_start, usize::from(reopened));
    assert_eq!(failure.wire_end - failure.wire_start, attempts);
    assert_eq!(failure.exchange_start, 0, "no echo/auth POST baseline and no other refresh actor");
    assert_eq!(failure.exchange_end - failure.exchange_start, exchanges,
        "AS attempts in this isolated method interval, NOT callback/token lookup counts");
    for (attempt, get) in snapshot.gets[failure.wire_start..failure.wire_end].iter().enumerate() {
        assert_eq!(get.kind, if reopened { GetKind::CommonReopen } else { GetKind::CommonInitial });
        assert_eq!(get.planned_status, status);
        assert_eq!(get.session.as_deref(), Some("session-1"));
        assert_eq!(get.cursor.as_deref(), if reopened { Some("common/session-1/1") } else { None });
        assert_eq!(get.authorization.as_deref(), Some(if attempt == 0 { "Bearer initial" } else { "Bearer access-1" }));
    }
    if exchanges == 1 {
        let exchange = &snapshot.exchanges[failure.exchange_start];
        assert_eq!(exchange.grant, "refresh-0");
        assert_eq!(exchange.issued_access.as_deref(), Some("access-1"));
        assert_eq!(exchange.issued_refresh.as_deref(), Some("refresh-1"));
        assert!(exchange.response_prepared);
        assert!(!exchange.handler_dropped);
    }
    // Do not compare whole-ledger totals here: a late observer can see an additional
    // default-policy outer opening. That is a separate ORACLE-DESIRED violation below.
}

async fn terminal_case(reopened: bool, reply: ResumeReply) -> (CommonCase, FailedOpening) {
    let mut case = CommonCase::start(SseOptions {
        post_unauthorized_once: false,
        common_initial_reply: if reopened { ResumeReply::Success } else { reply },
        common_reopen_reply: if reopened { reply } else { ResumeReply::Success },
        ..Default::default()
    }).await;
    if reopened { case.establish_then_end_common().await; }
    let status = rejection_bounds(reply).0;
    let snapshot = case.fixture.wait_for(|snapshot| failed_opening(snapshot, status).is_some()).await;
    let failure = failed_opening(&snapshot, status).unwrap();
    assert_local_bound(&snapshot, failure, reopened, reply);
    // This immutable boundary is recorded BEFORE any subsequent JSON liveness probe.
    (case, failure)
}

fn next_opening(snapshot: &Snapshot, failure: FailedOpening) -> bool {
    snapshot.gets.len() > failure.wire_end || snapshot.events.iter().any(|event|
        matches!(event, Event::ClientGetStarted { open, .. } if *open > failure.open))
}

async fn oracle_terminal(reopened: bool, reply: ResumeReply) {
    let (mut case, failure) = terminal_case(reopened, reply).await;
    case.probe("alive-after-common-failure", None).await;
    // Inspect the COMPLETE ledger against the pre-probe boundary, including any forbidden
    // SDK invocation or wire attempt during probe completion. Keep the service alive here.
    // Missing another opening for five seconds is bounded absence, not worker/error delivery.
    let extra = case.fixture.wait_for_with_timeout(BOUND, |snapshot| next_opening(snapshot, failure)).await;
    assert!(extra.is_err(), "terminal common GET failure must not begin another SDK opening or wire attempt");
    let snapshot = case.fixture.snapshot();
    assert_common_only(&snapshot);
    assert!(!next_opening(&snapshot, failure));
    assert_eq!(snapshot.gets.len(), failure.wire_end);
    assert_eq!(snapshot.exchanges.len(), failure.exchange_end);
    assert_eq!(snapshot.events.iter().filter(|event| matches!(event, Event::ClientGetStarted { .. })).count(), failure.open);
    let expected_refreshes = rejection_bounds(reply).2;
    assert_eq!(case.fixture.saved_tokens().await.unwrap(), SavedTokens {
        access: if expected_refreshes == 1 { "access-1".into() } else { "initial".into() },
        refresh: Some(format!("refresh-{expected_refreshes}")),
    });
    case.close().await;
    let final_snapshot = case.fixture.snapshot();
    assert_common_only(&final_snapshot);
    assert!(!next_opening(&final_snapshot, failure));
    assert_eq!(final_snapshot.exchanges.len(), failure.exchange_end);
}

#[tokio::test]
async fn local_bound_common_initial_and_reopened_rejections_have_separate_method_intervals() {
    for reopened in [false, true] {
        for reply in [ResumeReply::Unauthorized, ResumeReply::InsufficientScope, ResumeReply::Forbidden] {
            let (mut case, failure) = terminal_case(reopened, reply).await;
            // Explicit close isolates LOCAL-BOUND. It must not establish outer termination.
            case.close().await;
            assert_local_bound(&case.fixture.snapshot(), failure, reopened, reply);
        }
    }
}

// Separately named: an observed desired red cannot prevent another status/phase from running.
#[tokio::test]
async fn oracle_desired_common_initial_exhausted_401_does_not_open_again() {
    oracle_terminal(false, ResumeReply::Unauthorized).await;
}
#[tokio::test]
async fn oracle_desired_common_initial_scope_403_does_not_open_again() {
    oracle_terminal(false, ResumeReply::InsufficientScope).await;
}
#[tokio::test]
async fn oracle_desired_common_initial_plain_403_does_not_open_again() {
    oracle_terminal(false, ResumeReply::Forbidden).await;
}
#[tokio::test]
async fn oracle_desired_common_reopened_exhausted_401_does_not_open_again() {
    oracle_terminal(true, ResumeReply::Unauthorized).await;
}
#[tokio::test]
async fn oracle_desired_common_reopened_scope_403_does_not_open_again() {
    oracle_terminal(true, ResumeReply::InsufficientScope).await;
}
#[tokio::test]
async fn oracle_desired_common_reopened_plain_403_does_not_open_again() {
    oracle_terminal(true, ResumeReply::Forbidden).await;
}

async fn oracle_transient(reopened: bool) {
    let mut case = CommonCase::start(SseOptions {
        post_unauthorized_once: false,
        common_initial_reply: if reopened { ResumeReply::Success } else { ResumeReply::TransientThenSuccess },
        common_reopen_reply: if reopened { ResumeReply::TransientThenSuccess } else { ResumeReply::Success },
        ..Default::default()
    }).await;
    if reopened { case.establish_then_end_common().await; }
    // Positive evidence required. A timeout FAILS; it does not become terminal success.
    // In frozen H488–512 initial503 and later503 both reenter runGetStream. Stock currently
    // loses initial503 before its auto-reconnector exists. No timing-parity claim is made.
    let ordinal = case.successful_open(if reopened { 2 } else { 1 }).await;
    assert_eq!(ordinal, if reopened { 3 } else { 2 });
    case.probe("after-common-transient", Some(ordinal)).await;
    let snapshot = case.fixture.snapshot();
    assert_common_only(&snapshot);
    assert_eq!(snapshot.gets.len(), ordinal);
    let expected_kinds = if reopened {
        vec![GetKind::CommonInitial, GetKind::CommonReopen, GetKind::CommonReopen]
    } else { vec![GetKind::CommonInitial, GetKind::CommonInitial] };
    assert_eq!(snapshot.gets.iter().map(|get| get.kind).collect::<Vec<_>>(), expected_kinds);
    assert!(snapshot.gets.iter().all(|get| get.authorization.as_deref() == Some("Bearer initial")));
    let failure = failed_opening(&snapshot, 503).unwrap();
    assert_eq!(failure.wire_start, usize::from(reopened));
    assert_eq!(failure.wire_end - failure.wire_start, 1, "503 never causes local auth replay");
    assert_eq!(failure.exchange_end - failure.exchange_start, 0);
    assert_eq!(snapshot.events.iter().filter(|event| matches!(event, Event::ClientGetStarted { .. })).count(), ordinal);
    assert!(snapshot.exchanges.is_empty());
    assert_eq!(case.fixture.saved_tokens().await.unwrap(), SavedTokens {
        access: "initial".into(), refresh: Some("refresh-0".into()),
    });
    case.close().await;
    let final_snapshot = case.fixture.snapshot();
    assert_common_only(&final_snapshot);
    assert_eq!(final_snapshot.gets.len(), ordinal);
    assert!(final_snapshot.exchanges.is_empty());
}

#[tokio::test]
async fn oracle_desired_common_initial_503_retries_and_becomes_usable() {
    oracle_transient(false).await;
}
#[tokio::test]
async fn oracle_desired_common_reopened_503_retries_and_becomes_usable() {
    oracle_transient(true).await;
}
