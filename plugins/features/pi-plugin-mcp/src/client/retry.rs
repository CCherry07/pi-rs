//! Pi connection retry policy over typed rmcp failures, never formatted error matching.
use super::*;
use rmcp::service::{ClientInitializeError, ServiceError};
use rmcp::transport::streamable_http_client::StreamableHttpError;

const CONNECT_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(250), Duration::from_millis(1000)];

#[derive(Debug)]
pub(super) struct ConnectionFailure {
    pub error: McpError,
    pub transient_http: bool,
    pub legacy_fallback: bool,
}

impl From<McpError> for ConnectionFailure {
    fn from(error: McpError) -> Self {
        Self {
            error,
            transient_http: false,
            legacy_fallback: false,
        }
    }
}

impl ConnectionFailure {
    pub(super) fn handshake(error: McpError, cause: &ClientInitializeError) -> Self {
        Self {
            error,
            transient_http: transient_initialize(cause),
            // Narrow the existing compatibility fallback to the actual oracle regression:
            // an uncorrelated JSON-RPC error in discover. A failed transport, authorization
            // or ordinary initialize error is not permission to repeat initialization.
            legacy_fallback: matches!(
                cause,
                ClientInitializeError::UncorrelatedErrorResponse { .. }
            ),
        }
    }
}

fn transient_initialize(error: &ClientInitializeError) -> bool {
    match error {
        ClientInitializeError::TransportError { error, .. } => {
            transient_transport(error.error.as_ref())
        }
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            transient_initialize(fallback)
        }
        _ => false,
    }
}

fn transient_service(error: &ServiceError) -> bool {
    match error {
        ServiceError::TransportSend(error) => transient_transport(error.error.as_ref()),
        _ => false,
    }
}

fn transient_transport(error: &(dyn std::error::Error + 'static)) -> bool {
    // StreamableHttpError::Client intentionally does not expose its inner error as source().
    // All three HTTP adapters use reqwest; stdio and application/JSON-RPC errors never qualify.
    match error.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
        Some(StreamableHttpError::HttpStatus { status, .. }) => transient_status(*status),
        Some(StreamableHttpError::Client(error)) => match error.status() {
            Some(status) => transient_status(status),
            None => {
                error.is_connect() || error.is_timeout() || error.is_request() || error.is_body()
            }
        },
        _ => false,
    }
}

fn transient_status(status: http::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429)
        || status.is_server_error() && status != http::StatusCode::NOT_IMPLEMENTED
}

/// A retry repeats the complete setup (handshake and tool discovery), never only a broken
/// handshake on an ambiguous transport. Dropping this future also drops the current attempt
/// or its backoff timer; there is no detached retry work.
pub(super) async fn connect_discovered(
    config: McpServerConfig,
    options: McpConnectionOptions,
    auth: McpClientAuth,
) -> Result<(Arc<McpConnection>, Vec<McpToolSpec>), McpError> {
    let http = matches!(config.transport, McpTransport::Http { .. });
    for attempt in 0..=CONNECT_RETRY_DELAYS.len() {
        match discover_once(config.clone(), options.clone(), auth.clone()).await {
            Ok(result) => return Ok(result),
            Err(failure) => {
                let Some(delay) = CONNECT_RETRY_DELAYS
                    .get(attempt)
                    .filter(|_| http && failure.transient_http)
                else {
                    return Err(failure.error);
                };
                tokio::time::sleep(*delay).await;
            }
        }
    }
    unreachable!("the last attempt always returns")
}

async fn discover_once(
    config: McpServerConfig,
    options: McpConnectionOptions,
    auth: McpClientAuth,
) -> Result<(Arc<McpConnection>, Vec<McpToolSpec>), ConnectionFailure> {
    let server = connect_server(config, options, auth).await?;
    let tools = if server
        .metadata()
        .is_some_and(|info| info.capabilities.tools.is_some())
    {
        match server.peer.list_all_tools().await {
            Ok(tools) => tools,
            Err(cause) => {
                let transient_http = transient_service(&cause);
                server.handler.stderr.finish().await;
                return Err(ConnectionFailure {
                    error: McpError::ListTools {
                        server: server.name.clone(),
                        message: server
                            .handler
                            .stderr
                            .message("server rejected discovery or the connection closed"),
                    },
                    transient_http,
                    legacy_fallback: false,
                });
            }
        }
    } else {
        Vec::new()
    };
    Ok((server, tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn network_errors_are_typed_and_error_text_never_drives_retry() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/unavailable"))
            .send()
            .await
            .unwrap_err();
        assert!(transient_transport(&StreamableHttpError::Client(error)));
        assert!(!transient_transport(
            &StreamableHttpError::<reqwest::Error>::UnexpectedServerResponse(
                "HTTP 503: network".into()
            )
        ));
    }

    #[test]
    fn status_classification_is_exact_and_auth_protocol_and_closed_failures_are_permanent() {
        for code in 100..=599 {
            assert_eq!(
                transient_status(http::StatusCode::from_u16(code).unwrap()),
                matches!(code, 408 | 429 | 500 | 502..=599),
                "{code}"
            );
        }
        assert!(!transient_initialize(
            &ClientInitializeError::ConnectionClosed("network 503".into())
        ));
        assert!(!transient_initialize(&ClientInitializeError::JsonRpcError(
            rmcp::ErrorData::internal_error("HTTP 503", None)
        )));
        assert!(!transient_service(&ServiceError::TransportClosed));
        assert!(!transient_transport(
            &StreamableHttpError::<reqwest::Error>::SessionExpired
        ));
        assert!(!transient_transport(
            &StreamableHttpError::<reqwest::Error>::Auth(
                rmcp::transport::auth::AuthError::AuthorizationRequired
            )
        ));
    }
}
