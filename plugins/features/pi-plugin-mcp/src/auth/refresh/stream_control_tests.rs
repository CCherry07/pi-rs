//! GET auth waits keep request authority without borrowing the POST's recovery allowance.
use super::sse_fixture::{OAuthSseFixture, ObservedOAuthClient, ResumeReply, SseOptions};
use super::*;
use crate::client::operation::{DispatchGate, OperationError, OperationGuard};
use crate::http_client::{HttpClient, StreamRequest};
use serde_json::json;
use std::time::Duration;

async fn initialize(fixture: &OAuthSseFixture) -> ObservedOAuthClient {
    let client = fixture.observed_client(ConnectionMode::Supervised).await;
    let message = serde_json::from_value(json!({"jsonrpc":"2.0","id":0,"method":"initialize",
        "params":{"protocolVersion":"2025-03-26","capabilities":{},
            "clientInfo":{"name":"get-fence-test","version":"1"}}}))
    .unwrap();
    let response = client
        .post_message(fixture.url().into(), message, None, None, HashMap::new())
        .await
        .unwrap();
    assert!(
        matches!(response, StreamableHttpPostResponse::Json(_, Some(ref id)) if id == "session-1")
    );
    client
}

fn stream_request(fixture: &OAuthSseFixture, control: RequestControl) -> StreamRequest {
    StreamRequest {
        uri: fixture.url().into(),
        session: Some("session-1".into()),
        cursor: None,
        token: None,
        headers: HashMap::new(),
        max_event_bytes: 16 * 1024 * 1024,
        control: Some(control),
        dispatch: None,
        session_observer: None,
    }
}

#[tokio::test]
async fn get_refresh_wait_rechecks_cancelled_retired_and_superseded_authority() {
    for invalidation in 0..3 {
        let fixture = OAuthSseFixture::start(SseOptions {
            common_initial_reply: ResumeReply::AuthenticateThenSuccess,
            ..Default::default()
        })
        .await;
        let client = initialize(&fixture).await;
        let (abort, signal) = pi_core::AbortHandle::new();
        let guard = OperationGuard::new(signal, None);
        let gate = Arc::new(DispatchGate::default());
        let control = RequestControl::new(guard.operation(), &gate);
        assert!(
            control.try_recover().unwrap(),
            "the POST already spent its allowance"
        );
        let exchange = fixture.hold_exchange(1);
        let request = stream_request(&fixture, control);
        let task = tokio::spawn(async move { client.open_stream(request).await });
        exchange.wait_entered().await;
        match invalidation {
            0 => abort.abort(),
            1 => gate.retire(),
            _ => gate.supersede(),
        }
        exchange.release();
        let error = match tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
        {
            Err(error) => error,
            Ok(_) => panic!("retired GET replay reached the server"),
        };
        let expected = match invalidation {
            0 => OperationError::Cancelled,
            1 => OperationError::AuthorityRetired,
            _ => OperationError::TransportSuperseded,
        };
        assert!(matches!(error, HttpError::Io(ref error)
            if error.get_ref().and_then(|error| error.downcast_ref::<OperationError>()) == Some(&expected)));
        let snapshot = fixture.snapshot();
        assert_eq!(snapshot.gets.len(), 1, "no GET after the held exchange");
        assert_eq!(
            snapshot.exchanges.len(),
            1,
            "rotation is not cancelled with its waiter"
        );
        assert_eq!(
            fixture.saved_tokens().await.unwrap().refresh.as_deref(),
            Some("refresh-1")
        );
    }
}

#[tokio::test]
async fn get_recovery_remains_local_after_the_application_allowance_is_spent() {
    let fixture = OAuthSseFixture::start(SseOptions {
        common_initial_reply: ResumeReply::AuthenticateThenSuccess,
        ..Default::default()
    })
    .await;
    let client = initialize(&fixture).await;
    let guard = OperationGuard::new(pi_core::AbortHandle::new().1, None);
    let gate = Arc::new(DispatchGate::default());
    let control = RequestControl::new(guard.operation(), &gate);
    assert!(control.try_recover().unwrap());
    let body = client
        .open_stream(stream_request(&fixture, control.clone()))
        .await
        .unwrap();
    assert!(!control.try_recover().unwrap());
    assert_eq!(fixture.snapshot().gets.len(), 2);
    assert_eq!(fixture.snapshot().exchanges.len(), 1);
    drop(body);
}
