//! Actual-slot consumers of the shared real HTTP/OAuth fixture.
//! RUST-OWNERSHIP is deliberately stronger than frozen Pi's detached GET cancellation.
//! ORACLE-DESIRED assertions exercise the production private HTTP transport policy.
mod provider_wait;
use super::*;
use crate::auth::sse_fixture::{
    Event, FinalBody, Gate, GatePoint, GetKind, OAuthSseFixture, ResumeReply, ServerStream,
    Snapshot, SseOptions,
};
use crate::supervisor::{
    ConnectionInput, ConnectionReadiness, McpGenerationReadiness, McpSupervisor,
};

const WAIT: Duration = Duration::from_secs(5);
// A bounded forbidden-event observation, not an unbounded proof of stream quiescence.
const EXTRA_GET_WINDOW: Duration = Duration::from_millis(250);

struct Runtime {
    owner: McpSupervisor,
    _source: McpGenerationReadiness,
    pool: McpToolSet,
}

async fn publish(
    fixture: &OAuthSseFixture,
    owner: &McpSupervisor,
) -> (McpGenerationReadiness, McpToolSet) {
    let snapshot = owner.wait_for(&[fixture.name().into()], WAIT).await;
    let source = McpGenerationReadiness::new(owner.clone(), &snapshot);
    let ConnectionReadiness::Ready(inventory) = &snapshot.servers[fixture.name()] else {
        panic!("SSE fixture setup did not become ready");
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

async fn prepare(fixture: &OAuthSseFixture) -> Runtime {
    let policy = McpServerPolicy::direct();
    assert!(
        policy.timeout > WAIT,
        "the outer test bound must not be a request timeout"
    );
    let owner = McpSupervisor::new();
    let input = ConnectionInput::new(
        fixture.config(),
        (&policy).into(),
        fixture.client_auth(),
        [0; 32],
    )
    .unwrap();
    owner
        .reconcile(owner.begin_candidate(), vec![input])
        .unwrap();
    let (source, pool) = publish(fixture, &owner).await;
    observe(fixture, |snapshot| common_started(snapshot, "session-1")).await;
    Runtime {
        owner,
        _source: source,
        pool,
    }
}

struct Call {
    abort: pi_core::AbortHandle,
    task: tokio::task::JoinHandle<Result<ToolResult, ToolError>>,
    updates: tokio::sync::mpsc::UnboundedReceiver<pi_plugin::ToolUpdate>,
}

impl Drop for Call {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Call {
    async fn finish(mut self) -> Result<ToolResult, ToolError> {
        tokio::time::timeout(WAIT, &mut self.task)
            .await
            .expect("logical result missing; an outer timeout is NOT a terminal MCP error")
            .expect("consumer task panicked")
    }

    async fn progress(&mut self, number: u64) {
        let expected = format!("Progress {number}");
        tokio::time::timeout(WAIT, async {
            loop {
                let update = self
                    .updates
                    .recv()
                    .await
                    .expect("request ended before progress");
                if update.content.iter().any(
                    |content| matches!(content, ContentBlock::Text(text) if text.text == expected),
                ) {
                    break;
                }
            }
        })
        .await
        .expect("real progress notification was not consumed by the runtime");
    }
}

fn call(pool: &McpToolSet, remote: &str, lane: &str) -> Call {
    let pool = pool.clone();
    let remote = remote.to_owned();
    let lane = lane.to_owned();
    let (abort, signal) = pi_core::AbortHandle::new();
    let context = ToolContext::standalone(PathBuf::from("/tmp"), signal);
    let (updates, received) = ToolUpdateSink::channel();
    let task = tokio::spawn(async move {
        let tool = pool
            .inner
            .tools
            .iter()
            .find(|tool| tool.remote_name == remote)
            .unwrap();
        tool.execute(
            context,
            format!("call-{lane}").into(),
            json!({"lane":lane}),
            updates,
        )
        .await
    });
    Call {
        abort,
        task,
        updates: received,
    }
}

async fn entered(gate: &Gate) {
    tokio::time::timeout(WAIT, gate.wait_entered())
        .await
        .expect("gate deadline");
}

async fn observe(fixture: &OAuthSseFixture, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    fixture
        .wait_for_with_timeout(WAIT, predicate)
        .await
        .expect("wire observation deadline")
}

fn common_started(snapshot: &Snapshot, session: &str) -> bool {
    snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerBodyStarted(ServerStream::Common { session: actual, .. }) if actual == session)
    })
}

fn posts<'a>(snapshot: &'a Snapshot, lane: &str) -> Vec<&'a crate::auth::sse_fixture::PostRecord> {
    snapshot
        .posts
        .iter()
        .filter(|post| post.method == "tools/call" && post.lane.as_deref() == Some(lane))
        .collect()
}

fn gets<'a>(snapshot: &'a Snapshot, lane: &str) -> Vec<&'a crate::auth::sse_fixture::GetRecord> {
    snapshot
        .gets
        .iter()
        .filter(|get| get.kind == GetKind::Response && get.lane.as_deref() == Some(lane))
        .collect()
}

fn echo_posts(snapshot: &Snapshot, lane: &str, expected: usize) {
    let records = posts(snapshot, lane);
    assert_eq!(records.len(), expected, "original echo POST count");
    assert!(
        records
            .iter()
            .all(|record| record.body["params"]["name"] == "echo")
    );
    // These runtime cases use rotating access text; same-text cases live at LOCAL-BOUND.
    assert_eq!(records[0].authorization.as_deref(), Some("Bearer initial"));
    for replay in records.iter().skip(1) {
        assert_eq!(replay.authorization.as_deref(), Some("Bearer access-1"));
        assert_eq!(replay.id, records[0].id, "SDK request id changed on replay");
        assert_eq!(
            replay.body, records[0].body,
            "wire request changed on replay"
        );
        assert_eq!(replay.session, records[0].session);
    }
}

fn unexpected_get(snapshot: &Snapshot) -> bool {
    snapshot.gets.iter().any(|get| {
        !matches!(
            get.kind,
            GetKind::Response | GetKind::CommonInitial | GetKind::CommonReopen
        )
    })
}

fn assert_complete_wire(snapshot: &Snapshot) {
    assert!(
        !unexpected_get(snapshot),
        "unclassified/wrong-session GET reached the server"
    );
    assert_eq!(
        snapshot
            .gets
            .iter()
            .filter(|get| get.kind == GetKind::CommonReopen)
            .count(),
        0
    );
    assert_eq!(
        snapshot
            .gets
            .iter()
            .filter(|get| get.kind == GetKind::CommonInitial)
            .count(),
        snapshot.initializations
    );
}

fn response_attempts(snapshot: &Snapshot, lane: &str, token_generations: &[usize]) {
    let records = gets(snapshot, lane);
    assert_eq!(records.len(), token_generations.len());
    let original_posts = posts(snapshot, lane);
    let original_session = &original_posts[0].session;
    for (record, generation) in records.iter().zip(token_generations) {
        assert_eq!(&record.session, original_session);
        assert_eq!(
            record.cursor, records[0].cursor,
            "resumption changed its server cursor"
        );
        let bearer = if *generation == 0 {
            "Bearer initial".to_owned()
        } else {
            format!("Bearer access-{generation}")
        };
        assert_eq!(record.authorization.as_deref(), Some(bearer.as_str()));
    }
}

fn success(result: Result<ToolResult, ToolError>, text: &str) {
    let result = result.unwrap();
    assert!(!result.is_error, "server rejected the actual bearer");
    assert!(
        serde_json::to_string(&result.content)
            .unwrap()
            .contains(text)
    );
}

/// Positive fence: a probe waits for local refresh, sends the saved bearer, and stays pending
/// while a genuine progress notification arrives on the still-live common stream.
async fn probe_common(
    fixture: &OAuthSseFixture,
    pool: &McpToolSet,
    session: &str,
    lane: &str,
    expected_exchanges: usize,
) -> Snapshot {
    let gate = fixture.hold(GatePoint::Probe(lane.into()));
    let mut probe = call(pool, "probe", lane);
    entered(&gate).await;
    let snapshot = fixture.snapshot();
    assert_complete_wire(&snapshot);
    assert_eq!(snapshot.exchanges.len(), expected_exchanges);
    let record = posts(&snapshot, lane).into_iter().next().unwrap();
    assert!(record.authorized);
    assert_eq!(record.session.as_deref(), Some(session));
    let expected_access = snapshot
        .exchanges
        .last()
        .and_then(|exchange| exchange.issued_access.as_deref())
        .unwrap_or("initial");
    assert_eq!(
        record.authorization.as_deref(),
        Some(format!("Bearer {expected_access}").as_str())
    );
    let saved = fixture.saved_tokens().await.unwrap();
    assert_eq!(saved.access, expected_access);
    let expected_refresh = snapshot
        .exchanges
        .last()
        .and_then(|exchange| exchange.issued_refresh.as_deref())
        .unwrap_or("refresh-0");
    assert_eq!(saved.refresh.as_deref(), Some(expected_refresh));
    let common = snapshot
        .gets
        .iter()
        .find(|get| {
            get.kind == GetKind::CommonInitial
                && get.session.as_deref() == Some(session)
                && get.planned_status == 200
        })
        .unwrap();
    assert!(!snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerBodyDropped { stream: ServerStream::Common { ordinal, .. }, .. } if *ordinal == common.ordinal)
    }));
    fixture
        .emit_common_progress(common.ordinal, record.progress_token.clone().unwrap(), 777)
        .await;
    probe.progress(777).await;
    gate.release();
    success(probe.finish().await, &format!("probe:{lane}"));
    let final_snapshot = fixture.snapshot();
    assert_complete_wire(&final_snapshot);
    assert_eq!(final_snapshot.exchanges.len(), expected_exchanges);
    final_snapshot
}

#[tokio::test]
async fn rust_ownership_original_auth_abort_preserves_rotation_and_common_stream() {
    let fixture = OAuthSseFixture::start(SseOptions {
        resume_reply: ResumeReply::Success,
        ..Default::default()
    })
    .await;
    let exchange = fixture.hold_exchange(1);
    let runtime = prepare(&fixture).await;
    let original = call(&runtime.pool, "echo", "original");
    entered(&exchange).await;
    original.abort.abort();
    assert!(matches!(original.finish().await, Err(ToolError::Aborted)));
    exchange.release();
    let snapshot = probe_common(&fixture, &runtime.pool, "session-1", "after-abort", 1).await;
    echo_posts(&snapshot, "original", 1);
    assert!(gets(&snapshot, "original").is_empty());
    assert!(!snapshot.executions.iter().any(|lane| lane == "original"));
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn rust_ownership_resumed_auth_abort_keeps_shared_refresh_and_common_stream() {
    let fixture = OAuthSseFixture::start(SseOptions::default()).await;
    let exchange = fixture.hold_exchange(2);
    let runtime = prepare(&fixture).await;
    let original = call(&runtime.pool, "echo", "resumed");
    entered(&exchange).await;
    echo_posts(&fixture.snapshot(), "resumed", 2);
    assert_eq!(gets(&fixture.snapshot(), "resumed").len(), 1);
    original.abort.abort();
    assert!(matches!(original.finish().await, Err(ToolError::Aborted)));
    exchange.release();
    let snapshot = probe_common(
        &fixture,
        &runtime.pool,
        "session-1",
        "after-resume-abort",
        2,
    )
    .await;
    echo_posts(&snapshot, "resumed", 2);
    response_attempts(&snapshot, "resumed", &[1]); // No late GET auth replay after cancellation.
    assert_eq!(snapshot.executions, ["resumed"]);
    let id = posts(&snapshot, "resumed")[0].id.as_ref().unwrap();
    assert!(snapshot.cancellations.contains(id));
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn rust_ownership_two_rejected_calls_cancel_one_keep_single_exchange_sibling_and_common() {
    let fixture = OAuthSseFixture::start(SseOptions {
        // Do not accidentally add a second, resume-phase OAuth challenge to this case.
        resume_reply: ResumeReply::Success,
        ..Default::default()
    })
    .await;
    let left_401 = fixture.hold(GatePoint::FirstPost401("left".into()));
    let right_401 = fixture.hold(GatePoint::FirstPost401("right".into()));
    let exchange = fixture.hold_exchange(1);
    let runtime = prepare(&fixture).await;
    let left = call(&runtime.pool, "echo", "left");
    let right = call(&runtime.pool, "echo", "right");
    entered(&left_401).await;
    entered(&right_401).await;
    left_401.release();
    right_401.release();
    entered(&exchange).await;
    left.abort.abort();
    assert!(matches!(left.finish().await, Err(ToolError::Aborted)));
    exchange.release();
    success(right.finish().await, "echo:right");
    let snapshot = probe_common(&fixture, &runtime.pool, "session-1", "after-sibling", 1).await;
    echo_posts(&snapshot, "left", 1);
    echo_posts(&snapshot, "right", 2);
    assert_eq!(snapshot.max_exchange_overlap, 1);
    assert_eq!(snapshot.executions, ["right"]);
    assert!(gets(&snapshot, "left").is_empty());
    response_attempts(&snapshot, "right", &[1]);
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn rust_ownership_close_after_consumed_priming_with_eof_in_flight() {
    let fixture = OAuthSseFixture::start(SseOptions {
        post_unauthorized_once: false,
        resume_reply: ResumeReply::Success,
        retry_ms: 30_000,
        ..Default::default()
    })
    .await;
    let priming = fixture.hold(GatePoint::PrimingEof("closing".into()));
    let runtime = prepare(&fixture).await;
    let mut original = call(&runtime.pool, "echo", "closing");
    original.progress(1).await;
    entered(&priming).await;
    priming.release();
    observe(&fixture, |snapshot| {
        snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerEof(ServerStream::Priming { lane, .. }) if lane == "closing")
    })
    })
    .await;
    // This is NOT an internal backoff-entered fence. It proves shutdown after consumed
    // priming and server EOF, while a long retry hint makes resumption eligible later.
    tokio::time::timeout(WAIT, runtime.owner.close())
        .await
        .unwrap()
        .unwrap();
    assert!(original.finish().await.is_err());
    let snapshot = observe(&fixture, |snapshot| snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerBodyDropped { stream: ServerStream::Common { session, .. }, .. } if session == "session-1")
    })).await;
    assert_complete_wire(&snapshot);
    echo_posts(&snapshot, "closing", 1);
    assert!(gets(&snapshot, "closing").is_empty());
    assert_eq!(
        snapshot.gets.len(),
        1,
        "shutdown must not open any response/common/invalid GET"
    );
    assert_eq!(snapshot.gets[0].kind, GetKind::CommonInitial);
    assert!(snapshot.exchanges.is_empty());
    assert!(
        snapshot
            .deletes
            .iter()
            .any(|session| session.as_deref() == Some("session-1"))
    );
}

#[tokio::test]
async fn rust_ownership_exact_slot_retirement_cannot_rebind_resumed_request() {
    let fixture = OAuthSseFixture::start(SseOptions::default()).await;
    let exchange = fixture.hold_exchange(2);
    let runtime = prepare(&fixture).await;
    let original = call(&runtime.pool, "echo", "retiring");
    entered(&exchange).await;
    runtime.owner.reconnect(fixture.name()).unwrap();
    assert!(original.finish().await.is_err());
    let (_new_source, new_pool) = publish(&fixture, &runtime.owner).await;
    observe(&fixture, |snapshot| common_started(snapshot, "session-2")).await;
    let before = fixture.snapshot();
    let error = call(&runtime.pool, "probe", "old-pool")
        .finish()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("retired"));
    let after = fixture.snapshot();
    assert_eq!(
        after.posts.len(),
        before.posts.len(),
        "old view dispatched onto a new slot"
    );
    assert_eq!(
        after.gets.len(),
        before.gets.len(),
        "old view emitted a GET after slot retirement"
    );
    assert_complete_wire(&after);
    exchange.release();
    // The old manager is no longer reachable through a live connection. Wait on actual
    // saved state, not the AS response-prepared event; yield rather than sleep blindly.
    tokio::time::timeout(WAIT, async {
        loop {
            if fixture
                .saved_tokens()
                .await
                .is_some_and(|tokens| tokens.refresh.as_deref() == Some("refresh-2"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached rotation did not reach the credential store");
    observe(&fixture, |snapshot| snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerBodyDropped { stream: ServerStream::Common { session, .. }, .. } if session == "session-1")
    })).await;
    let snapshot = probe_common(&fixture, &new_pool, "session-2", "new-pool", 2).await;
    echo_posts(&snapshot, "retiring", 2);
    response_attempts(&snapshot, "retiring", &[1]);
    assert_eq!(snapshot.initializations, 2);
    assert!(snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerBodyDropped { stream: ServerStream::Common { session, .. }, .. } if session == "session-1")
    }));
    assert_eq!(
        snapshot
            .gets
            .iter()
            .filter(|get| get.kind == GetKind::CommonInitial && get.planned_status == 200)
            .count(),
        2
    );
    runtime.owner.close().await.unwrap();
}

struct TerminalDiagnostic {
    scenario: &'static str,
    started: tokio::time::Instant,
    expected_gets: usize,
    expected_exchanges: usize,
}

struct TerminalTaskEvidence {
    source: &'static str,
    state: &'static str,
    tool_is_error: Option<bool>,
}

fn terminal_join_error_kind(error: &tokio::task::JoinError) -> &'static str {
    if error.is_cancelled() {
        "task-cancelled"
    } else if error.is_panic() {
        "task-panicked"
    } else {
        "task-join-error"
    }
}

fn terminal_poll_task_once(original: &mut Call) -> TerminalTaskEvidence {
    // Poll the existing handle once, without awaiting, restarting or extending its wait.
    // Pending may reflect cooperative budget; neither outcome dates result completion.
    let (state, tool_is_error) = match futures::FutureExt::now_or_never(&mut original.task) {
        None => ("pending", None),
        Some(Ok(Ok(result))) => ("tool-result", Some(result.is_error)),
        Some(Ok(Err(_))) => ("tool-error", None),
        Some(Err(error)) => (terminal_join_error_kind(&error), None),
    };
    TerminalTaskEvidence {
        source: "diagnostic-poll",
        state,
        tool_is_error,
    }
}

fn terminal_diagnostic_failure(
    diagnostic: &TerminalDiagnostic,
    branch: &'static str,
    snapshot: Snapshot,
    task: TerminalTaskEvidence,
) -> ! {
    // Allowlisted scalars and at most eight rows, never Debug of raw records/results.
    // Status is the server's planned response, not evidence of client receipt.
    let target_posts = posts(&snapshot, "terminal");
    let target_gets = gets(&snapshot, "terminal");
    let original_session = target_posts.first().and_then(|post| post.session.as_ref());
    let original_cursor = target_gets.first().and_then(|get| get.cursor.as_ref());
    let omitted = snapshot.gets.len().saturating_sub(8);
    let recent_gets: Vec<Value> = snapshot.gets.iter().skip(omitted).map(|get| {
        let kind = match get.kind {
            GetKind::Response => "response",
            GetKind::CommonInitial => "common-initial",
            GetKind::CommonReopen => "common-reopen",
            GetKind::Rejected => "rejected",
        };
        json!({
            "ordinal": get.ordinal,
            "kind": kind,
            "planned_status": get.planned_status,
            "target_lane": get.lane.as_deref() == Some("terminal"),
            "session_present": get.session.is_some(),
            "session_equals_original_post": original_session.map(|value| get.session.as_ref() == Some(value)),
            "cursor_present": get.cursor.is_some(),
            "cursor_equals_first_response": original_cursor.map(|value| get.cursor.as_ref() == Some(value)),
        })
    }).collect();
    let evidence = json!({
        "schema": 1,
        "scenario": diagnostic.scenario,
        "branch": branch,
        "elapsed_ms_at_diagnostic": diagnostic.started.elapsed().as_millis(),
        "task": {
            "source": task.source,
            "state": task.state,
            "tool_is_error": task.tool_is_error,
            "completion_time": "not-observed",
        },
        "expected": {
            "target_response_gets": diagnostic.expected_gets,
            "as_attempts": diagnostic.expected_exchanges,
        },
        "counts": {
            "posts": snapshot.posts.len(),
            "target_posts": target_posts.len(),
            "gets": snapshot.gets.len(),
            "target_response_gets": target_gets.len(),
            "response_gets": snapshot.gets.iter().filter(|get| get.kind == GetKind::Response).count(),
            "common_initial_gets": snapshot.gets.iter().filter(|get| get.kind == GetKind::CommonInitial).count(),
            "common_reopen_gets": snapshot.gets.iter().filter(|get| get.kind == GetKind::CommonReopen).count(),
            "rejected_gets": snapshot.gets.iter().filter(|get| get.kind == GetKind::Rejected).count(),
            "target_executions": snapshot.executions.iter().filter(|lane| lane.as_str() == "terminal").count(),
            "as_attempts": snapshot.exchanges.len(),
            "as_responses_prepared": snapshot.exchanges.iter().filter(|exchange| exchange.response_prepared).count(),
            "as_handlers_dropped": snapshot.exchanges.iter().filter(|exchange| exchange.handler_dropped).count(),
        },
        "predicates_at_snapshot": {
            "unexpected_get": unexpected_get(&snapshot),
            "too_many_target_response_gets": target_gets.len() > diagnostic.expected_gets,
        },
        "server_phase": {
            "priming_body_started": snapshot.events.iter().any(|event| matches!(event,
                Event::ServerBodyStarted(ServerStream::Priming { lane, .. }) if lane == "terminal")),
            "priming_eof": snapshot.events.iter().any(|event| matches!(event,
                Event::ServerEof(ServerStream::Priming { lane, .. }) if lane == "terminal")),
            "response_body_started": snapshot.events.iter().any(|event| matches!(event,
                Event::ServerBodyStarted(ServerStream::Response { lane, .. }) if lane == "terminal")),
            "response_eof": snapshot.events.iter().any(|event| matches!(event,
                Event::ServerEof(ServerStream::Response { lane, .. }) if lane == "terminal")),
        },
        "recent_gets": recent_gets,
        "omitted_get_rows": omitted,
        "post_terminal_probe": "not-run",
        "cleanup": "not-observed",
    });
    panic!("terminal resume diagnostic: {evidence}");
}

async fn terminal_resume_case(reply: ResumeReply, expected_gets: usize, expected_exchanges: usize) {
    let fixture = OAuthSseFixture::start(SseOptions {
        resume_reply: reply,
        ..Default::default()
    })
    .await;
    let runtime = prepare(&fixture).await;
    let mut original = call(&runtime.pool, "echo", "terminal");
    let diagnostic = TerminalDiagnostic {
        scenario: match reply {
            ResumeReply::Unauthorized => "unauthorized-401",
            ResumeReply::InsufficientScope => "scope-403",
            ResumeReply::Forbidden => "plain-403",
            _ => "other-terminal-scenario",
        },
        expected_gets,
        expected_exchanges,
        started: tokio::time::Instant::now(),
    };
    // Keep unbiased selection and both existing bounds. Diagnostic readiness never
    // turns a timeout into a pass or proves when a simultaneously ready result completed.
    let result = tokio::select! {
        result = tokio::time::timeout(WAIT, &mut original.task) => match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                let snapshot = fixture.snapshot();
                // The join result was consumed: do not poll this handle a second time.
                let task = TerminalTaskEvidence {
                    source: "selected-result",
                    state: terminal_join_error_kind(&error),
                    tool_is_error: None,
                };
                terminal_diagnostic_failure(&diagnostic, "result-join-error", snapshot, task)
            }
            Err(_) => {
                let snapshot = fixture.snapshot();
                let task = terminal_poll_task_once(&mut original);
                terminal_diagnostic_failure(&diagnostic, "result-deadline-selected", snapshot, task)
            }
        },
        observation = fixture.wait_for_with_timeout(WAIT, |snapshot| {
            unexpected_get(snapshot) || gets(snapshot, "terminal").len() > expected_gets
        }) => match observation {
            Ok(snapshot) => {
                let task = terminal_poll_task_once(&mut original);
                terminal_diagnostic_failure(&diagnostic, "forbidden-wire-observed", snapshot, task)
            }
            Err(_) => {
                let snapshot = fixture.snapshot();
                let task = terminal_poll_task_once(&mut original);
                terminal_diagnostic_failure(&diagnostic, "observation-deadline-selected", snapshot, task)
            }
        },
    };
    assert!(
        result.is_err(),
        "terminal HTTP rejection became a successful tool result"
    );
    let snapshot = probe_common(
        &fixture,
        &runtime.pool,
        "session-1",
        "after-terminal",
        expected_exchanges,
    )
    .await;
    echo_posts(&snapshot, "terminal", 2);
    response_attempts(
        &snapshot,
        "terminal",
        if expected_gets == 2 { &[1, 2] } else { &[1] },
    );
    assert_eq!(snapshot.executions, ["terminal"]);
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn oracle_desired_resumed_401_after_one_replay_is_terminal() {
    terminal_resume_case(ResumeReply::Unauthorized, 2, 2).await;
}

#[tokio::test]
async fn oracle_desired_resumed_scope_step_up_is_terminal_without_resume_refresh() {
    terminal_resume_case(ResumeReply::InsufficientScope, 1, 1).await;
}

#[tokio::test]
async fn oracle_desired_resumed_plain_403_is_terminal_without_resume_refresh() {
    terminal_resume_case(ResumeReply::Forbidden, 1, 1).await;
}

#[tokio::test]
async fn oracle_desired_resume_expiry_does_not_reexecute_or_expire_the_application_session() {
    let fixture = OAuthSseFixture::start(SseOptions {
        post_unauthorized_once: false,
        resume_reply: ResumeReply::SessionExpired,
        ..Default::default()
    })
    .await;
    let runtime = prepare(&fixture).await;
    assert!(
        call(&runtime.pool, "echo", "resume-expiry")
            .finish()
            .await
            .is_err()
    );
    let snapshot = fixture.snapshot();
    echo_posts(&snapshot, "resume-expiry", 1);
    assert_eq!(
        snapshot.initializations, 1,
        "a rejected GET cannot invalidate the POST session"
    );
    assert_eq!(snapshot.executions, ["resume-expiry"]);
    // No prior POST challenge: the application allowance and original bearer are unused.
    response_attempts(&snapshot, "resume-expiry", &[0]);
    let snapshot = probe_common(
        &fixture,
        &runtime.pool,
        "session-1",
        "after-resume-expiry",
        0,
    )
    .await;
    assert_eq!(snapshot.initializations, 1);
    echo_posts(&snapshot, "resume-expiry", 1);
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn oracle_desired_transient_resume_reopens_without_reexecuting_echo() {
    let fixture = OAuthSseFixture::start(SseOptions {
        resume_reply: ResumeReply::TransientThenSuccess,
        ..Default::default()
    })
    .await;
    let runtime = prepare(&fixture).await;
    success(
        call(&runtime.pool, "echo", "transient").finish().await,
        "echo:transient",
    );
    let snapshot = probe_common(&fixture, &runtime.pool, "session-1", "after-transient", 1).await;
    echo_posts(&snapshot, "transient", 2);
    response_attempts(&snapshot, "transient", &[1, 1]);
    let responses = gets(&snapshot, "transient");
    assert_eq!(responses[0].planned_status, 503);
    assert_eq!(responses[1].planned_status, 200);
    assert_eq!(responses[0].cursor, responses[1].cursor);
    assert_eq!(responses[0].session, responses[1].session);
    assert_eq!(snapshot.executions, ["transient"]);
    runtime.owner.close().await.unwrap();
}

#[tokio::test]
async fn oracle_desired_final_response_eof_retry_zero_has_no_extra_get_in_observation_window() {
    let fixture = OAuthSseFixture::start(SseOptions {
        resume_reply: ResumeReply::Success,
        final_body: FinalBody::Eof,
        retry_ms: 0,
        ..Default::default()
    })
    .await;
    let runtime = prepare(&fixture).await;
    success(
        call(&runtime.pool, "echo", "final").finish().await,
        "echo:final",
    );
    observe(&fixture, |snapshot| {
        snapshot.events.iter().any(|event| {
        matches!(event, Event::ServerEof(ServerStream::Response { lane, .. }) if lane == "final")
    })
    })
    .await;
    // Keep transport/common alive: closing at result delivery would mask the stock drain gap.
    // An extra GET is conclusive failure. Timeout here means only bounded absence, unlike
    // the logical-result timeout above; it is NOT an SDK quiescence/full-parity assertion.
    let extra = fixture
        .wait_for_with_timeout(EXTRA_GET_WINDOW, |snapshot| {
            unexpected_get(snapshot) || gets(snapshot, "final").len() > 1
        })
        .await;
    assert!(
        extra.is_err(),
        "matching final response+EOF initiated an extra GET during drain"
    );
    let snapshot = probe_common(&fixture, &runtime.pool, "session-1", "after-final", 1).await;
    echo_posts(&snapshot, "final", 2);
    response_attempts(&snapshot, "final", &[1]);
    assert_eq!(snapshot.executions, ["final"]);
    runtime.owner.close().await.unwrap();
}
