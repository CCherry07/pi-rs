use super::*;

struct AnonymousProvider;
#[async_trait::async_trait]
impl crate::McpProviderTokenResolver for AnonymousProvider {
    async fn token(&self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

fn save_token(library: &McpLibrary, name: &str, url: &str) {
    library
        .client_auth
        .oauth_store
        .save_tokens(
            name,
            url,
            "fixture-client",
            crate::McpOAuthTokens {
                access_token: "fixture-token".into(),
                token_type: "Bearer".into(),
                refresh_token: None,
                expires_in: Some(3600),
                scope: None,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn isolation_uses_only_each_effective_auth_revision() {
    let provider = Fixture::new(false, 200, false).await;
    let header = Fixture::new(false, 200, false).await;
    let oauth = Fixture::new(false, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false)
        .with_provider_token_resolver(Arc::new(AnonymousProvider));
    let mut provider_entry = provider.entry("direct");
    provider_entry["auth"] = json!({"provider":"fixture"});
    let mut header_entry = header.entry("direct");
    header_entry["headers"] = json!({"aUtHoRiZaTiOn":"Bearer fixed"});
    header_entry["auth"] = json!({"provider":"fixture"});
    configure(
        &library,
        json!({"provider":provider_entry,"header":header_entry,"oauth":oauth.entry("direct")}),
    );
    let supervisor = McpSupervisor::new();
    let revisions = BTreeMap::from([("fixture".into(), [1; 32])]);
    let _first = library
        .prepare_supervised_generation(supervisor.clone(), revisions.clone())
        .await
        .unwrap();
    save_token(&library, "unrelated", "https://unrelated.example/mcp");
    let mut unrelated = revisions.clone();
    unrelated.insert("unused-provider".into(), [9; 32]);
    let _second = library
        .prepare_supervised_generation(supervisor.clone(), unrelated)
        .await
        .unwrap();
    for fixture in [&provider, &header, &oauth] {
        assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
    }
    let _third = library
        .prepare_supervised_generation(
            supervisor.clone(),
            BTreeMap::from([("fixture".into(), [2; 32])]),
        )
        .await
        .unwrap();
    assert_eq!(provider.starts.load(Ordering::Acquire), 2);
    assert_eq!(header.starts.load(Ordering::Acquire), 1);
    assert_eq!(oauth.starts.load(Ordering::Acquire), 1);
    save_token(&library, "oauth", &oauth.url);
    let _fourth = library
        .prepare_supervised_generation(
            supervisor.clone(),
            BTreeMap::from([("fixture".into(), [2; 32])]),
        )
        .await
        .unwrap();
    assert_eq!(provider.starts.load(Ordering::Acquire), 2);
    assert_eq!(header.starts.load(Ordering::Acquire), 1);
    assert_eq!(oauth.starts.load(Ordering::Acquire), 2);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn malformed_oauth_store_is_a_per_entry_failure_not_a_global_failure() {
    let oauth = Fixture::new(false, 200, false).await;
    let header = Fixture::new(false, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), None, false);
    let mut entry = header.entry("direct");
    entry["headers"] = json!({"Authorization":"Bearer fixture"});
    configure(
        &library,
        json!({"header":entry,"oauth":oauth.entry("direct")}),
    );
    std::fs::write(directory.path().join("mcp-auth.json"), "not JSON").unwrap();
    let supervisor = McpSupervisor::new();
    let result = prepare(&library, &supervisor).await;
    assert!(matches!(
        result.server_readiness["oauth"],
        McpServerReadiness::Failed(_)
    ));
    assert!(matches!(
        result.server_readiness["header"],
        McpServerReadiness::Ready
    ));
    assert_eq!(oauth.starts.load(Ordering::Acquire), 0);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn trust_root_and_defining_path_isolate_otherwise_equivalent_connections() {
    let fixture = Fixture::new(false, 200, false).await;
    let directory = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let library = McpLibrary::new(directory.path(), Some(project.path()), false);
    configure(&library, json!({"server":fixture.entry("direct")}));
    let supervisor = McpSupervisor::new();
    let _first = prepare(&library, &supervisor).await;
    let mut trusted = library.clone();
    trusted.trusted = true;
    let _second = prepare(&trusted, &supervisor).await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 2);
    std::fs::create_dir_all(project.path().join(".pi")).unwrap();
    std::fs::write(
        trusted.path(McpScope::Project).unwrap(),
        json!({"mcpServers":{"server":fixture.entry("direct")}}).to_string(),
    )
    .unwrap();
    let _third = prepare(&trusted, &supervisor).await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 3);
    let mut root = trusted.clone();
    root.cwd = None;
    let _fourth = prepare(&root, &supervisor).await;
    assert_eq!(fixture.starts.load(Ordering::Acquire), 4);
    supervisor.close().await.unwrap();
}

#[tokio::test]
async fn only_successful_mutations_invalidate_the_published_generation() {
    for operation in ["logout", "disable", "remove"] {
        let fixture = Fixture::new(false, 200, false).await;
        let directory = tempfile::tempdir().unwrap();
        let library = McpLibrary::new(directory.path(), None, false);
        configure(&library, json!({"server":fixture.entry("direct")}));
        let supervisor = McpSupervisor::new();
        let prepared = prepare(&library, &supervisor).await;
        let generation = prepared.readiness_watch.as_ref().unwrap().downgrade();
        let mut bound = library.clone();
        bound.generation = Some(generation.clone());
        assert!(bound.active_supervisor().is_none());
        bound.publish_generation();
        assert!(bound.active_supervisor().is_some());
        // A denied write must not kill the live transport.
        assert!(
            bound
                .set_enabled(McpScope::Project, "server", false)
                .is_err()
        );
        assert!(matches!(
            supervisor.snapshot().servers["server"],
            ConnectionReadiness::Ready(_)
        ));
        match operation {
            "logout" => {
                bound.logout("server", None).unwrap();
            }
            "disable" => bound
                .set_enabled(McpScope::Global, "server", false)
                .unwrap(),
            "remove" => {
                assert!(bound.remove(McpScope::Global, "server").unwrap());
            }
            _ => unreachable!(),
        }
        assert!(supervisor.snapshot().servers.is_empty());
        assert_eq!(fixture.starts.load(Ordering::Acquire), 1);
        bound.shutdown_generation(false).await.unwrap();
        assert!(generation.is_retired());
        assert!(bound.active_supervisor().is_none());
    }
}
