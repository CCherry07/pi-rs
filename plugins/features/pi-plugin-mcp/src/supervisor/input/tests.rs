use super::*;
use crate::McpProviderTokenResolver;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENDPOINT: &str = "PI_MCP_SUPERVISOR_VALIDATION_ENDPOINT";

struct AuthTripwire(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl McpProviderTokenResolver for AuthTripwire {
    async fn token(&self, _: &str) -> Result<Option<String>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        // Even the unfixed path cannot contact the non-loopback fixture hostname:
        // fail before any HTTP request rather than supplying a usable credential.
        Err("fixture forbids authentication work".into())
    }
}

#[test]
fn expanded_local_provider_url_is_validated_before_auth_or_transport() {
    if std::env::var_os(ENDPOINT).is_none() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        for endpoint in [
            "http://provider.invalid/mcp".to_owned(),
            format!("http://user:fixture@{}/mcp", listener.local_addr().unwrap()),
        ] {
            // Child-process environment is explicit and isolated from parallel tests;
            // never mutate this process's environment with unsafe set_var.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([CHILD_TEST, "--exact", "--nocapture"])
                .env(ENDPOINT, endpoint)
                .output()
                .unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "invalid resolved configuration reached a transport"
            );
            assert!(
                output.status.success(),
                "isolated validation regression failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let definition = crate::config::parse_document(
                &serde_json::json!({"mcpServers":{"fixture":{
                    "url": format!("${{{ENDPOINT}}}"),
                    "auth": {"provider":"fixture-provider"}
                }}})
                .to_string(),
            )
            .unwrap()
            .servers
            .remove(0)
            .into_connection_definition()
            .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let input = ConnectionInput::local(
                definition,
                LocalValues {
                    cwd: directory.path().to_owned(),
                    allow_commands: false,
                },
                (&crate::policy::McpServerPolicy::direct()).into(),
                McpClientAuth::default()
                    .with_provider_tokens(Arc::new(AuthTripwire(calls.clone()))),
                [0; 32],
            )
            .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                input.connect(pi_core::AbortHandle::new().1),
            )
            .await
            .expect("invalid configuration must fail before connection/discovery");
            assert_eq!(calls.load(Ordering::SeqCst), 0, "provider resolver ran");
            let error = result.err().expect("invalid expanded URL must fail");
            assert!(matches!(error, McpError::InvalidHttp { .. }), "{error}");
            let message = error.to_string();
                "provider auth requires https or a loopback URL"
            } else {
                "no URL userinfo or fragment"
            };
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("provider.invalid"));
            assert!(!message.contains("user:fixture"));
        });
}
