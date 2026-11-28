//! LOCAL-BOUND and STOCK-CHARACTERIZATION only. The separately owned runtime tests
//! retain ORACLE-DESIRED terminal/final-EOF assertions and RUST-OWNERSHIP requirements.
//! No SDK retry override. No callback/token-lookup counts are inferred from wire traffic.
use super::sse_fixture::*;
use super::*;
use crate::client::operation::{DispatchGate, OperationGuard};
use rmcp::model::ClientRequest;
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::transport::{
    StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
};
use serde_json::json;
use std::time::Duration;

async fn connect(
    f: &OAuthSseFixture,
    mode: ConnectionMode,
) -> RunningService<rmcp::RoleClient, ()> {
    let transport = StreamableHttpClientTransport::with_client(
        f.observed_client(mode).await,
        StreamableHttpClientTransportConfig::with_uri(f.url().to_owned())
            .reinit_on_expired_session(mode == ConnectionMode::Explicit),
    );
    let service = tokio::time::timeout(
        Duration::from_secs(5),
        ().serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
    )
    .await
    .unwrap()
    .unwrap();
    // No application request exists yet: this returned GET is the real initial common stream.
    f.wait_for(|snapshot| {
        snapshot
            .events
            .iter()
            .any(|event| matches!(event, Event::ClientGetReturned { ok: true, .. }))
    })
    .await;
    assert_eq!(f.snapshot().gets.len(), 1);
    assert_eq!(f.snapshot().gets[0].kind, GetKind::CommonInitial);
    service
}

async fn close(service: &mut RunningService<rmcp::RoleClient, ()>) {
    service
        .close_with_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("SDK cleanup timed out; Ok(None) is not a completed teardown");
}

fn request() -> (
    OperationGuard,
    Arc<DispatchGate>,
    RequestControl,
    ClientRequest,
) {
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    let mut request: ClientRequest =
        serde_json::from_value(json!({"method":"tools/call","params":{
            "name":"echo","arguments":{"lane":"main"},"_meta":{"progressToken":"fixture-main"}
        }}))
        .unwrap();
    control.attach(&mut request);
    (guard, gate, control, request)
}

fn assert_echo_and_common(snapshot: &Snapshot) {
    let posts: Vec<_> = snapshot
        .posts
        .iter()
        .filter(|post| post.method == "tools/call" && post.body["params"]["name"] == "echo")
        .collect();
    assert_eq!(
        posts.len(),
        2,
        "GET resumption must never reexecute the echo POST"
    );
    assert_eq!(posts[0].id, posts[1].id);
    assert_eq!(
        posts[0].body, posts[1].body,
        "including genuine progress metadata"
    );
    assert_eq!(posts[0].session, posts[1].session);
    assert!(
        !snapshot
            .gets
            .iter()
            .any(|get| get.kind == GetKind::Rejected),
        "unexpected MCP GET was dispatched"
    );
    assert_eq!(snapshot.executions, ["main"]);
    assert_eq!(snapshot.initializations, 1);
    assert_eq!(
        snapshot
            .gets
            .iter()
            .filter(|get| get.kind == GetKind::CommonInitial)
            .count(),
        1
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
        snapshot.max_exchange_overlap, 1,
        "isolated, sequential refresh precondition"
    );
}

fn failed_get_ranges(snapshot: &Snapshot, status: u16) -> Vec<(usize, usize)> {
    snapshot
        .events
        .iter()
        .filter_map(|event| match event {
            Event::ClientGetReturned {
                ok: false,
                status: Some(observed),
                wire_start,
                wire_end,
                ..
            } if *observed == status => Some((*wire_start, *wire_end)),
            _ => None,
        })
        .collect()
}

fn failed_exchange_ranges(snapshot: &Snapshot, status: u16) -> Vec<(usize, usize)> {
    snapshot
        .events
        .iter()
        .filter_map(|event| match event {
            Event::ClientGetReturned {
                ok: false,
                status: Some(observed),
                exchange_start,
                exchange_end,
                ..
            } if *observed == status => Some((*exchange_start, *exchange_end)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn local_bound_real_sdk_post_and_resumed_get_recover_independently() {
    for mode in [ConnectionMode::Explicit, ConnectionMode::Supervised] {
        for unchanged_access_token in [false, true] {
            // Unexpired credentials, one echo, no overlap/login/logout; real common stream held.
            // Held final body isolates this bound; it proves nothing about stock's EOF drain.
            let f = OAuthSseFixture::start(SseOptions {
                unchanged_access_token,
                ..Default::default()
            })
            .await;
            let mut service = connect(&f, mode).await;
            let (_guard, _gate, control, request) = request();
            let handle = service
                .peer()
                .send_cancellable_request(request, Default::default())
                .await
                .unwrap();
            let rpc_id = serde_json::to_value(&handle.id).unwrap();
            let result = tokio::time::timeout(Duration::from_secs(5), handle.await_response())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap()["content"][0]["text"],
                "echo:main"
            );
            if mode == ConnectionMode::Supervised {
                assert!(!control.try_recover().unwrap());
            }
            let snapshot = f.snapshot();
            assert_echo_and_common(&snapshot);
            let gets: Vec<_> = snapshot
                .gets
                .iter()
                .filter(|get| get.kind == GetKind::Response)
                .collect();
            assert_eq!(gets.len(), 2);
            assert_eq!(gets[0].cursor, gets[1].cursor);
            assert_eq!(gets[0].session, gets[1].session);
            assert_eq!(gets[0].session.as_deref(), Some("session-1"));
            assert_eq!(
                gets[0].authorization.as_deref(),
                Some(if unchanged_access_token {
                    "Bearer initial"
                } else {
                    "Bearer access-1"
                })
            );
            assert_eq!(
                gets[1].authorization.as_deref(),
                Some(if unchanged_access_token {
                    "Bearer initial"
                } else {
                    "Bearer access-2"
                })
            );
            assert_eq!(
                snapshot
                    .posts
                    .iter()
                    .find(|post| post.lane.as_deref() == Some("main"))
                    .unwrap()
                    .id
                    .as_ref(),
                Some(&rpc_id)
            );
            assert_eq!(snapshot.exchanges.len(), 2);
            assert_eq!(snapshot.exchanges[0].grant, "refresh-0");
            assert_eq!(snapshot.exchanges[1].grant, "refresh-1");
            assert_eq!(
                f.saved_tokens().await.unwrap(),
                SavedTokens {
                    access: if unchanged_access_token {
                        "initial".into()
                    } else {
                        "access-2".into()
                    },
                    refresh: Some("refresh-2".into()),
                }
            );
            assert!(!snapshot.events.iter().any(|event| matches!(
                event,
                Event::ServerBodyDropped {
                    stream: ServerStream::Common { .. },
                    ..
                }
            )));
            close(&mut service).await;
            assert_echo_and_common(&f.snapshot());
        }
    }
}

#[tokio::test]
async fn local_bound_failed_get_invocation_stops_its_own_auth_reaction_only() {
    for (resume_reply, status, attempts, resume_exchanges) in [
        (ResumeReply::Unauthorized, 401, 2, 1),
        (ResumeReply::InsufficientScope, 403, 1, 0),
        (ResumeReply::Forbidden, 403, 1, 0),
    ] {
        let f = OAuthSseFixture::start(SseOptions {
            resume_reply,
            ..Default::default()
        })
        .await;
        let mut service = connect(&f, ConnectionMode::Supervised).await;
        let (_guard, _gate, control, request) = request();
        let handle = service
            .peer()
            .send_cancellable_request(request, Default::default())
            .await
            .unwrap();
        let snapshot = f
            .wait_for(|snapshot| !failed_get_ranges(snapshot, status).is_empty())
            .await;
        let (start, end) = failed_get_ranges(&snapshot, status)[0];
        assert_eq!(end - start, attempts);
        assert!(
            snapshot.gets[start..end]
                .iter()
                .all(|get| get.kind == GetKind::Response)
        );
        assert_echo_and_common(&snapshot);
        let (exchange_start, exchange_end) = failed_exchange_ranges(&snapshot, status)[0];
        assert_eq!(exchange_start, 1, "POST already spent exchange1");
        assert_eq!(
            exchange_end - exchange_start,
            resume_exchanges,
            "actual AS attempts during this invocation, not later outer reopens or callback counts"
        );
        assert!(!control.try_recover().unwrap());
        // METHOD RETURN is headers/auth completion, NOT stream/RPC/worker completion.
        // Explicit teardown below isolates LOCAL-BOUND; it MUST NOT establish outer eligibility.
        close(&mut service).await;
        assert_echo_and_common(&f.snapshot());
        drop(handle);
    }
}

#[tokio::test]
async fn local_bound_transient_get_allows_another_open_without_reexecuting_echo() {
    let f = OAuthSseFixture::start(SseOptions {
        resume_reply: ResumeReply::TransientThenSuccess,
        ..Default::default()
    })
    .await;
    let mut service = connect(&f, ConnectionMode::Supervised).await;
    let (_guard, _gate, control, request) = request();
    let handle = service
        .peer()
        .send_cancellable_request(request, Default::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle.await_response())
        .await
        .unwrap()
        .unwrap();
    let snapshot = f.snapshot();
    assert_echo_and_common(&snapshot);
    assert_eq!(
        snapshot
            .gets
            .iter()
            .filter(|get| get.kind == GetKind::Response)
            .count(),
        2
    );
    assert_eq!(failed_get_ranges(&snapshot, 503).len(), 1);
    let (start, end) = failed_get_ranges(&snapshot, 503)[0];
    assert_eq!(end - start, 1, "503 is not an auth replay");
    assert_eq!(
        snapshot.exchanges.len(),
        1,
        "only the original POST's reaction"
    );
    assert!(!control.try_recover().unwrap());
    close(&mut service).await;
    assert_echo_and_common(&f.snapshot());
}

#[tokio::test]
async fn stock_characterization_terminal_get_can_reopen_with_a_fresh_local_bound() {
    // STOCK-CHARACTERIZATION, deliberately NOT the desired oracle assertion. The separate
    // ORACLE-DESIRED case requires a prompt logical error and forbids this next opening.
    let f = OAuthSseFixture::start(SseOptions {
        resume_reply: ResumeReply::Unauthorized,
        ..Default::default()
    })
    .await;
    let mut service = connect(&f, ConnectionMode::Supervised).await;
    let (_guard, _gate, _control, request) = request();
    let handle = service
        .peer()
        .send_cancellable_request(request, Default::default())
        .await
        .unwrap();
    let snapshot = f
        .wait_for(|snapshot| failed_get_ranges(snapshot, 401).len() >= 2)
        .await;
    for (start, end) in failed_get_ranges(&snapshot, 401).into_iter().take(2) {
        assert_eq!(end - start, 2);
        assert!(
            snapshot.gets[start..end]
                .iter()
                .all(|get| get.kind == GetKind::Response)
        );
    }
    assert_echo_and_common(&snapshot);
    assert_eq!(
        &failed_exchange_ranges(&snapshot, 401)[..2],
        &[(1, 2), (2, 3)],
        "POST + one actual AS attempt in each of the first two GET invocations"
    );
    close(&mut service).await;
    assert_echo_and_common(&f.snapshot());
    drop(handle);
}
