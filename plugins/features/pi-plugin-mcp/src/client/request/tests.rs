use super::*;
use crate::http_client::{PiHttpClient, PiHttpClientError};
use rmcp::transport::{DynamicTransportError, WorkerTransport};

type Transport = WorkerTransport<crate::http_transport::HttpWorker<PiHttpClient>>;

fn stream_failure(cause: HttpError) -> McpRequestError {
    let error = Arc::new(HttpError::Client(PiHttpClientError::response_stream(cause)));
    McpRequestError::from_service(ServiceError::TransportSend(DynamicTransportError::new::<
        Transport,
        RoleClient,
    >(error)))
}

#[test]
fn stream_failure_retains_its_cause_without_application_auth_or_replay_classification() {
    for cause in [
        HttpError::SessionExpired,
        HttpError::Auth(rmcp::transport::auth::AuthError::AuthorizationRequired),
    ] {
        let was_auth = auth_challenge(&cause).is_some();
        let error = stream_failure(cause);
        let http = error.http().expect("typed transport failure");
        assert!(auth_challenge(http).is_none());
        let HttpError::Client(stream) = http else {
            panic!("missing stream provenance")
        };
        assert!(stream.status().is_none());
        assert!(stream.network_flags().is_none());
        assert!(stream.auth_rejection().is_none());
        let original = stream
            .response_stream_cause()
            .expect("original cause retained");
        if was_auth {
            assert!(auth_challenge(original).is_some());
        } else {
            assert!(matches!(original, HttpError::SessionExpired));
        }
        assert_eq!(stream.to_string(), "MCP response stream failed");
    }
}

#[test]
fn stream_failure_preserves_cancellation_and_authority_control_types() {
    for cause in [
        OperationError::Cancelled,
        OperationError::AuthorityRetired,
        OperationError::TransportSuperseded,
    ] {
        let error = stream_failure(cause.into_http());
        assert!(matches!(error, McpRequestError::Control(actual) if actual == cause));
    }
}
