use super::*;
use crate::McpProviderTokenResolver;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENDPOINT: &str = "PI_MCP_SUPERVISOR_VALIDATION_ENDPOINT";
const CHILD_TEST: &str =
    "supervisor::input::tests::expanded_local_provider_url_is_validated_before_auth_or_transport";

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
fn equivalence_tracks_effective_auth_source_without_reading_credentials() {
    let make = |config: McpServerConfig, auth| {
        ConnectionInput::new(
            config,
            (&crate::policy::McpServerPolicy::direct()).into(),
            auth,
            [0; 32],
        )
        .unwrap()
    };
    let http = McpServerConfig::http("server", "http://127.0.0.1/mcp");
    let auth = McpClientAuth::default();
    assert!(make(http.clone(), auth.clone()).equivalent(&make(http.clone(), auth.clone())));
    assert!(
        !make(http.clone(), auth.clone()).equivalent(&make(http.clone(), McpClientAuth::default()))
    );

    let file =
        |path| McpClientAuth::default().with_oauth_store(crate::McpOAuthCredentialStore::new(path));
    assert!(
        make(http.clone(), file("not-created/auth.json"))
            .equivalent(&make(http.clone(), file("not-created/auth.json")))
    );
    assert!(
        !make(http.clone(), file("not-created/auth.json"))
            .equivalent(&make(http.clone(), file("not-created/other.json")))
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = http.clone().provider_auth("fixture-provider");
    let resolver: Arc<dyn McpProviderTokenResolver> = Arc::new(AuthTripwire(calls.clone()));
    let provider_auth = |resolver| McpClientAuth::default().with_provider_tokens(resolver);
    assert!(
        make(provider.clone(), provider_auth(resolver.clone()))
            .equivalent(&make(provider.clone(), provider_auth(resolver)))
    );
    assert!(
        !make(
            provider.clone(),
            provider_auth(Arc::new(AuthTripwire(calls.clone())))
        )
        .equivalent(&make(
            provider,
            provider_auth(Arc::new(AuthTripwire(calls.clone())))
        ))
    );

    // Explicit headers and stdio do not use these distinct stores or providers.
    let header = http.headers(BTreeMap::from([(
        "aUtHoRiZaTiOn".into(),
        "Bearer fixture".into(),
    )]));
    assert!(make(header.clone(), auth.clone()).equivalent(&make(header, McpClientAuth::default())));
    let stdio = McpServerConfig::stdio("server", "never-executed");
    assert!(make(stdio.clone(), auth).equivalent(&make(stdio, McpClientAuth::default())));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn local_equivalence_includes_caller_header_source_order() {
    let directory = tempfile::tempdir().unwrap();
    let definition = |headers: &str| {
        let document =
            r#"{"mcpServers":{"server":{"url":"http://127.0.0.1/mcp","headers":HEADERS}}}"#
                .replace("HEADERS", headers);
        crate::config::parse_document(&document)
            .unwrap()
            .servers
            .remove(0)
            .into_connection_definition()
            .unwrap()
    };
    let auth = McpClientAuth::default();
    let make = |definition| {
        ConnectionInput::local(
            definition,
            LocalValues {
                cwd: directory.path().to_owned(),
                allow_commands: false,
            },
            (&crate::policy::McpServerPolicy::direct()).into(),
            auth.clone(),
            [0; 32],
        )
        .unwrap()
    };
    let first = definition(r#"{"X-Order":"first","x-order":"second"}"#);
    let same = definition(r#"{"X-Order":"first","x-order":"second"}"#);
    let reversed = definition(r#"{"x-order":"second","X-Order":"first"}"#);
    assert!(make(first.clone()).equivalent(&make(same)));
    assert!(!make(first).equivalent(&make(reversed)));
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
            let expected = if std::env::var(ENDPOINT)
                .unwrap()
                .contains("provider.invalid")
            {
                "provider auth requires https or a loopback URL"
            } else {
                "no URL userinfo or fragment"
            };
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("provider.invalid"));
            assert!(!message.contains("user:fixture"));
        });
}
