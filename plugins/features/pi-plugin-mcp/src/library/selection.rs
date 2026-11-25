//! Preserve selection intent only when current policy proves the absent identity is allowed.
//! Journals contain provider-safe names, not reversible remote names or server provenance.
use super::*;

pub(super) type ToolSelectionReader = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

impl McpLibrary {
    /// Supplies the current desired selection after the host's current grant filtering.
    /// Supervised preparation separates policy-qualified absence from unresolved identities;
    /// neither category is executable until exact inventory and current policy permit it.
    pub fn with_preserved_tool_selection(mut self, names: Vec<String>) -> Self {
        self.preserved_tool_selection = names;
        self
    }

    /// Supplies grant-filtered historical selection candidates for in-generation lane changes.
    /// These enlarge the allowance, never the current selection. Current desired names are
    /// always candidates too. Strict standalone preparation ignores both selection inputs.
    pub fn with_tool_selection_candidates(mut self, names: Vec<String>) -> Self {
        self.tool_selection_candidates = names;
        self
    }

    /// Installs a host-owned live desired-selection reader for startup activation.
    /// Capture only a weak session reference: executable views must not pin a session.
    pub fn with_tool_selection_reader(mut self, reader: ToolSelectionReader) -> Self {
        self.tool_selection_reader = Some(reader);
        self
    }

    pub(super) fn prepare_tool_selection(
        &mut self,
        policies: &BTreeMap<String, McpServerPolicy>,
        readiness: &BTreeMap<String, McpServerReadiness>,
        registered: &[String],
    ) -> Selection {
        let candidates = self
            .preserved_tool_selection
            .iter()
            .chain(&self.tool_selection_candidates)
            .cloned()
            .collect::<Vec<_>>();
        let mut selection = classify(&candidates, policies, readiness, registered);
        // Also quarantine ambiguous direct/default projections that have never
        // been selected. Quarantine itself must not create new desired intent.
        selection.quarantined = classify(registered, policies, readiness, registered).unresolved;
        self.preserved_tool_selection.clear();
        self.tool_selection_candidates = selection
            .allowed
            .iter()
            .chain(&selection.unresolved)
            .cloned()
            .collect();
        self.selection_ambiguities = selection.unresolved.len();
        self.denied_selection_count = selection.denied_after_inventory;
        selection
    }

    pub(crate) fn merge_preserved_tool_selection(
        &self,
        active: &mut Vec<String>,
    ) -> Result<(), String> {
        if self.tool_selection_candidates.is_empty() {
            return Ok(());
        }
        let failure = || {
            "MCP startup could not read current tool-selection intent; selection was left unchanged.".to_owned()
        };
        let desired =
            self.tool_selection_reader.as_ref().ok_or_else(failure)?().map_err(|_| failure())?;
        for name in desired {
            if self.tool_selection_candidates.contains(&name) && !active.contains(&name) {
                active.push(name);
            }
        }
        Ok(())
    }

    pub(crate) fn selection_diagnostic(&self) -> Option<String> {
        if self.selection_ambiguities == 0 && self.denied_selection_count == 0 {
            return None;
        }
        Some(format!(
            "MCP tool selection: {} saved identities remain unresolved until inventory is available and are not executable in this generation; {} completed-inventory identities had no exact visible match and were removed.",
            self.selection_ambiguities, self.denied_selection_count
        ))
    }
}

#[derive(Default)]
pub(super) struct Selection {
    pub(super) allowed: Vec<String>,
    pub(super) unresolved: Vec<String>,
    pub(super) quarantined: Vec<String>,
    denied_after_inventory: usize,
}

fn classify(
    candidates: &[String],
    policies: &BTreeMap<String, McpServerPolicy>,
    readiness: &BTreeMap<String, McpServerReadiness>,
    registered: &[String],
) -> Selection {
    let mut selection = Selection::default();
    let mut seen = std::collections::BTreeSet::new();
    // Include disabled namespaces too: an absent identity must not be attributed to
    // a different enabled server just because its namespace is a shorter prefix.
    let namespaces = readiness
        .keys()
        .map(|server| {
            (
                server,
                format!("mcp__{}__", crate::naming::identifier_component(server)),
            )
        })
        .collect::<Vec<_>>();
    for name in candidates {
        if !seen.insert(name) {
            continue;
        }
        if crate::resources::RESOURCE_TOOL_NAMES.contains(&name.as_str()) {
            // Shared resource identities have no single-server provenance. Actual
            // registered helpers already enforce current per-server visibility.
            if !registered.contains(name)
                && policies.iter().any(|(server, policy)| {
                    unavailable(readiness.get(server)) && policy.exposure != McpExposure::Hidden
                })
            {
                selection.allowed.push(name.clone());
            }
            continue;
        }
        if name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            continue;
        }
        // Hash shortening retains at most 55 bytes of the original qualified name.
        // If that cuts into a namespace, even the server identity cannot be proved.
        let owners = namespaces
            .iter()
            .filter(|(_, prefix)| {
                name.starts_with(prefix.as_str())
                    || (prefix.len() > 55
                        && name.len() == 64
                        && has_hash_shape(name)
                        && name.starts_with(&prefix[..55]))
            })
            .collect::<Vec<_>>();
        let unavailable_owners = owners
            .iter()
            .filter(|(server, _)| unavailable(readiness.get(*server)))
            .collect::<Vec<_>>();
        if unavailable_owners.is_empty() {
            // Complete inventories assign names before hiding tools, using the
            // same collision-safe projection as execution. Never invent aliases.
            if !registered.contains(name)
                && owners.iter().any(|(server, _)| {
                    matches!(
                        readiness.get(*server),
                        Some(
                            McpServerReadiness::Ready
                                | McpServerReadiness::ReadyWithRefreshError(_)
                        )
                    )
                })
            {
                selection.denied_after_inventory += 1;
            }
            continue;
        }
        let visibility = |server: &String, prefix: &String| {
            policies.get(server).map_or(Visibility::Hidden, |policy| {
                if prefix.len() > 55 {
                    if policy.exposure == McpExposure::Hidden
                        && policy
                            .tool_exposure
                            .iter()
                            .all(|(_, exposure)| *exposure == McpExposure::Hidden)
                    {
                        Visibility::Hidden
                    } else {
                        Visibility::Ambiguous
                    }
                } else {
                    visible_alias(policy, &name[prefix.len()..])
                }
            })
        };
        if owners.len() != 1 || registered.contains(name) {
            // A current visible alias can still be renamed when a pending
            // namespace contributes a collision peer. Quarantine it until every
            // relevant inventory is known, rather than activating a guessed owner.
            if registered.contains(name)
                || unavailable_owners.iter().any(|(server, prefix)| {
                    !matches!(visibility(server, prefix), Visibility::Hidden)
                })
            {
                selection.unresolved.push(name.clone());
            }
            continue;
        }
        let (server, prefix) = owners[0];
        match visibility(server, prefix) {
            Visibility::Visible => selection.allowed.push(name.clone()),
            Visibility::Hidden => {}
            Visibility::Ambiguous => selection.unresolved.push(name.clone()),
        }
    }
    selection
}

fn unavailable(readiness: Option<&McpServerReadiness>) -> bool {
    matches!(
        readiness,
        Some(
            McpServerReadiness::Pending
                | McpServerReadiness::NeedsAuth(_)
                | McpServerReadiness::Failed(_)
                | McpServerReadiness::Disconnected
        )
    )
}

enum Visibility {
    Visible,
    Hidden,
    Ambiguous,
}

fn visible_alias(policy: &McpServerPolicy, suffix: &str) -> Visibility {
    let all_overrides_visible = policy
        .tool_exposure
        .iter()
        .all(|(_, exposure)| *exposure != McpExposure::Hidden);
    let has_universal_pattern = policy
        .tool_exposure
        .iter()
        .any(|(pattern, _)| !pattern.is_empty() && pattern.bytes().all(|byte| byte == b'*'));
    if all_overrides_visible && (policy.exposure != McpExposure::Hidden || has_universal_pattern) {
        return Visibility::Visible;
    }
    if policy.exposure == McpExposure::Hidden
        && policy
            .tool_exposure
            .iter()
            .all(|(_, exposure)| *exposure == McpExposure::Hidden)
    {
        return Visibility::Hidden;
    }
    // Alphanumeric suffixes are bijective; underscores can represent punctuation,
    // UTF-16 units, or a hash separator. Never evaluate a lossy alias as a raw name.
    if !suffix.contains('_') {
        return if policy.exposure_for(suffix) == McpExposure::Hidden {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
    if has_hash_shape(suffix) {
        return Visibility::Ambiguous;
    }
    // Normalization of a hidden pattern overapproximates every raw name it can
    // match. Reject overlap even when exact/wildcard precedence might allow one
    // alias: without inventory we cannot choose that alias over a hidden peer.
    if policy.tool_exposure.iter().any(|(pattern, exposure)| {
        *exposure == McpExposure::Hidden && normalized_pattern_matches(pattern, suffix)
    }) {
        return Visibility::Ambiguous;
    }
    if policy.exposure != McpExposure::Hidden
        || policy.tool_exposure.iter().any(|(pattern, exposure)| {
            *exposure != McpExposure::Hidden
                && pattern.contains('*')
                && pattern
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'*')
                && normalized_pattern_matches(pattern, suffix)
        })
    {
        // Such wildcard literals are lossless. Every underscore was consumed by
        // a wildcard, so every raw spelling with this normalization is covered.
        return Visibility::Visible;
    }
    if policy.tool_exposure.iter().any(|(pattern, exposure)| {
        *exposure != McpExposure::Hidden && normalized_pattern_matches(pattern, suffix)
    }) {
        Visibility::Ambiguous
    } else {
        Visibility::Hidden
    }
}

fn has_hash_shape(value: &str) -> bool {
    value.rsplit_once('_').is_some_and(|(_, tail)| {
        tail.len() == 8
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn normalized_pattern_matches(pattern: &str, suffix: &str) -> bool {
    // Keep '*' as the only metacharacter, normalizing each literal UTF-16 unit
    // exactly like naming.rs. DP tracks a conservative language of normalized
    // spellings, not a fabricated remote name. The suffix is at most 64 bytes.
    let mut reachable = vec![false; suffix.len() + 1];
    reachable[0] = true;
    for unit in pattern.encode_utf16() {
        if unit == u16::from(b'*') {
            for index in 1..reachable.len() {
                reachable[index] |= reachable[index - 1];
            }
        } else {
            let literal = match unit {
                0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f => unit as u8,
                _ => b'_',
            };
            for index in (1..reachable.len()).rev() {
                reachable[index] = reachable[index - 1] && suffix.as_bytes()[index - 1] == literal;
            }
            reachable[0] = false;
        }
    }
    reachable[suffix.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naming::qualified_tool_name;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    fn policy(exposure: McpExposure, overrides: &[(&str, McpExposure)]) -> McpServerPolicy {
        McpServerPolicy {
            exposure,
            tool_exposure: overrides
                .iter()
                .map(|(name, exposure)| ((*name).into(), *exposure))
                .collect(),
            ..Default::default()
        }
    }

    fn classify_one(policy: McpServerPolicy, candidates: &[String]) -> Selection {
        classify(
            candidates,
            &BTreeMap::from([("docs".into(), policy)]),
            &BTreeMap::from([("docs".into(), McpServerReadiness::Pending)]),
            &[],
        )
    }

    #[test]
    fn selection_uniform_visible_retains_exact_names_including_hashed_and_unicode_aliases() {
        for exposure in [
            McpExposure::Direct,
            McpExposure::Codemode,
            McpExposure::Deferred,
        ] {
            let candidates = vec![
                qualified_tool_name("docs", "echo", |_| false),
                qualified_tool_name("docs", "a-b", |_| true),
                qualified_tool_name("docs", "😀", |_| false),
                qualified_tool_name("docs", &"long".repeat(30), |_| false),
            ];
            let result = classify_one(policy(exposure, &[]), &candidates);
            assert_eq!(result.allowed, candidates);
            assert_eq!(result.unresolved.len(), 0);
        }
    }

    #[test]
    fn selection_only_unavailable_enabled_servers_qualify() {
        let error = Arc::new(crate::McpError::Start {
            server: "docs".into(),
            message: "fixture".into(),
        });
        let states = [
            (McpServerReadiness::Pending, true),
            (McpServerReadiness::Disconnected, true),
            (McpServerReadiness::Failed(error.clone()), true),
            (
                McpServerReadiness::NeedsAuth(crate::McpAuthorizationRequired {
                    server: "docs".into(),
                    provider: None,
                    static_authorization: false,
                    challenge: Some(
                        "private challenge must not appear in selection diagnostics".into(),
                    ),
                }),
                true,
            ),
            (McpServerReadiness::Ready, false),
            (McpServerReadiness::ReadyWithRefreshError(error), false),
            (McpServerReadiness::Disabled, false),
        ];
        let candidates = names(&["mcp__docs__echo"]);
        for (state, allowed) in states {
            let result = classify(
                &candidates,
                &BTreeMap::from([("docs".into(), McpServerPolicy::default())]),
                &BTreeMap::from([("docs".into(), state)]),
                &[],
            );
            assert_eq!(!result.allowed.is_empty(), allowed);
        }
        assert!(
            classify(&candidates, &BTreeMap::new(), &BTreeMap::new(), &[])
                .allowed
                .is_empty()
        );
    }

    #[test]
    fn selection_registered_non_mcp_removed_and_malformed_names_are_not_allowances() {
        let candidates = names(&[
            "read",
            "codemode",
            "tool_search",
            "mcp__removed__echo",
            "mcp__docs__raw.tool",
            "mcp__docs__echo",
            "mcp__docs__waiting",
            "mcp__docs__waiting",
        ]);
        let result = classify(
            &candidates,
            &BTreeMap::from([("docs".into(), McpServerPolicy::default())]),
            &BTreeMap::from([("docs".into(), McpServerReadiness::Pending)]),
            &names(&["mcp__docs__echo"]),
        );
        assert_eq!(result.allowed, names(&["mcp__docs__waiting"]));
    }

    #[test]
    fn selection_bijective_suffix_obeys_exact_then_ordered_wildcard_policy() {
        let result = classify_one(
            policy(
                McpExposure::Hidden,
                &[
                    ("read*", McpExposure::Hidden),
                    ("*", McpExposure::Direct),
                    ("readAllowed", McpExposure::Deferred),
                ],
            ),
            &names(&[
                "mcp__docs__readAllowed",
                "mcp__docs__readSecret",
                "mcp__docs__write",
            ]),
        );
        assert_eq!(
            result.allowed,
            names(&["mcp__docs__readAllowed", "mcp__docs__write"])
        );
        assert_eq!(result.unresolved.len(), 0);
    }

    #[test]
    fn selection_hidden_punctuation_and_unicode_aliases_are_never_guessed_visible() {
        let result = classify_one(
            policy(
                McpExposure::Direct,
                &[
                    ("private.tool", McpExposure::Hidden),
                    ("😀", McpExposure::Hidden),
                    ("a-b", McpExposure::Hidden),
                    ("a_b", McpExposure::Direct),
                ],
            ),
            &names(&[
                "mcp__docs__private_tool",
                "mcp__docs____",
                "mcp__docs__a_b",
                "mcp__docs__public_tool",
            ]),
        );
        assert_eq!(result.allowed, names(&["mcp__docs__public_tool"]));
        assert_eq!(result.unresolved.len(), 3);
    }

    #[test]
    fn selection_mixed_policy_requires_covering_visible_wildcard_not_a_lossy_exact_alias() {
        let result = classify_one(
            policy(
                McpExposure::Hidden,
                &[
                    ("read.secret*", McpExposure::Hidden),
                    ("read*", McpExposure::Deferred),
                    ("safe_tool", McpExposure::Direct),
                    ("other.*", McpExposure::Direct),
                ],
            ),
            &names(&[
                "mcp__docs__read_item",
                "mcp__docs__read_secret",
                "mcp__docs__safe_tool",
                "mcp__docs__other_tool",
            ]),
        );
        assert_eq!(result.allowed, names(&["mcp__docs__read_item"]));
        assert_eq!(result.unresolved.len(), 3);
    }

    #[test]
    fn selection_hash_shaped_mixed_names_are_ambiguous_even_when_exact_alias_is_visible() {
        let hashed = qualified_tool_name("docs", "a-b", |_| true);
        let result = classify_one(
            policy(McpExposure::Direct, &[("a-b", McpExposure::Hidden)]),
            std::slice::from_ref(&hashed),
        );
        assert!(result.allowed.is_empty());
        assert_eq!(result.unresolved.len(), 1);
        let result = classify_one(policy(McpExposure::Hidden, &[]), &[hashed]);
        assert!(result.allowed.is_empty());
        assert_eq!(result.unresolved.len(), 0);
    }

    #[test]
    fn selection_overlapping_and_truncated_server_namespaces_are_ambiguous() {
        let long_server = "s".repeat(60);
        let policies = BTreeMap::from([
            ("docs".into(), McpServerPolicy::default()),
            ("docs__nested".into(), McpServerPolicy::default()),
            (long_server.clone(), McpServerPolicy::default()),
        ]);
        let readiness = BTreeMap::from([
            ("docs".into(), McpServerReadiness::Pending),
            // Even a disabled overlapping identity prevents guessing another server as origin.
            ("docs__nested".into(), McpServerReadiness::Disabled),
            (long_server.clone(), McpServerReadiness::Pending),
        ]);
        let result = classify(
            &[
                "mcp__docs__nested__echo".into(),
                qualified_tool_name(&long_server, "echo", |_| false),
            ],
            &policies,
            &readiness,
            &[],
        );
        assert!(result.allowed.is_empty());
        assert_eq!(result.unresolved.len(), 2);
    }

    #[test]
    fn selection_unresolved_names_wait_for_exact_inventory_without_losing_terminal_failure_intent()
    {
        let long_server = "s".repeat(60);
        let cases = [
            (
                "docs".to_owned(),
                policy(McpExposure::Direct, &[("a-b", McpExposure::Hidden)]),
                qualified_tool_name("docs", "a-b", |_| true),
            ),
            (
                long_server.clone(),
                McpServerPolicy::default(),
                qualified_tool_name(&long_server, "echo", |_| false),
            ),
        ];
        for (server, policy, name) in cases {
            let candidates = vec![name.clone()];
            let policies = BTreeMap::from([(server.clone(), policy)]);
            for state in [
                McpServerReadiness::Pending,
                McpServerReadiness::Failed(Arc::new(crate::McpError::Start {
                    server: server.clone(),
                    message: "terminal fixture failure".into(),
                })),
            ] {
                let selection = classify(
                    &candidates,
                    &policies,
                    &BTreeMap::from([(server.clone(), state)]),
                    &[],
                );
                assert_eq!(selection.unresolved, candidates);
                assert!(selection.allowed.is_empty());
                assert_eq!(selection.denied_after_inventory, 0);
            }
            let ready = BTreeMap::from([(server, McpServerReadiness::Ready)]);
            let resolved = classify(&candidates, &policies, &ready, &candidates);
            assert!(resolved.unresolved.is_empty());
            assert!(resolved.allowed.is_empty());
            assert_eq!(resolved.denied_after_inventory, 0);
            let missing_or_hidden = classify(&candidates, &policies, &ready, &[]);
            assert!(missing_or_hidden.unresolved.is_empty());
            assert!(missing_or_hidden.allowed.is_empty());
            assert_eq!(missing_or_hidden.denied_after_inventory, 1);
        }
    }

    #[test]
    fn selection_partial_ready_namespace_alias_is_quarantined_even_without_saved_intent() {
        let directory = tempfile::tempdir().unwrap();
        let alias = "mcp__docs__nested__echo";
        let policies = BTreeMap::from([
            ("docs".into(), McpServerPolicy::direct()),
            ("docs__nested".into(), McpServerPolicy::default()),
        ]);
        let readiness = BTreeMap::from([
            ("docs".into(), McpServerReadiness::Ready),
            ("docs__nested".into(), McpServerReadiness::Pending),
        ]);
        for desired in [vec![], names(&[alias])] {
            let mut library = McpLibrary::new(directory.path(), None, false)
                .with_preserved_tool_selection(desired.clone());
            let selection = library.prepare_tool_selection(&policies, &readiness, &names(&[alias]));
            assert_eq!(selection.unresolved, desired);
            assert_eq!(selection.quarantined, [alias]);
            assert!(selection.allowed.is_empty());
        }
        let mut library = McpLibrary::new(directory.path(), None, false)
            .with_preserved_tool_selection(names(&[alias]));
        let all_ready = BTreeMap::from([
            ("docs".into(), McpServerReadiness::Ready),
            ("docs__nested".into(), McpServerReadiness::Ready),
        ]);
        let selection = library.prepare_tool_selection(&policies, &all_ready, &names(&[alias]));
        assert!(selection.unresolved.is_empty());
        assert!(selection.quarantined.is_empty());
    }

    #[test]
    fn selection_shared_resource_tools_require_unavailable_server_wide_visibility() {
        let candidates = names(&crate::resources::RESOURCE_TOOL_NAMES);
        let allowed = classify_one(policy(McpExposure::Deferred, &[]), &candidates);
        assert_eq!(allowed.allowed, candidates);
        let denied = classify_one(
            policy(McpExposure::Hidden, &[("*", McpExposure::Direct)]),
            &candidates,
        );
        assert!(denied.allowed.is_empty());
        let ready = classify(
            &candidates,
            &BTreeMap::from([("docs".into(), McpServerPolicy::default())]),
            &BTreeMap::from([("docs".into(), McpServerReadiness::Ready)]),
            &[],
        );
        assert!(ready.allowed.is_empty());
    }

    #[tokio::test]
    async fn selection_supervised_projection_quarantines_then_resolves_without_renaming_survivors()
    {
        use axum::response::IntoResponse;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let handler_gate = gate.clone();
        let app = axum::Router::new().route("/{kind}", axum::routing::post(
            move |axum::extract::Path(kind): axum::extract::Path<String>, axum::Json(request): axum::Json<Value>| {
                let gate = handler_gate.clone();
                async move {
                    let id = &request["id"];
                    if id.is_null() { return http::StatusCode::ACCEPTED.into_response(); }
                    let result = match request["method"].as_str().unwrap() {
                        "server/discover" => return axum::Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"legacy fixture"}})).into_response(),
                        "initialize" => {
                            if kind == "pending" { gate.acquire().await.unwrap().forget(); }
                            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"selection-fixture","version":"1"}})
                        },
                        "tools/list" => json!({"tools":[{"name":if kind == "pending" { "echo" } else { "nested__echo" },"inputSchema":{"type":"object"}}]}),
                        other => panic!("unexpected request {other}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                }
            }
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("mcp.json"),
            json!({"mcpServers":{
                "docs":{"url":format!("{base}/ready"),"exposure":"direct"},
                "docs__nested":{"url":format!("{base}/pending"),"exposure":"deferred"}
            }})
            .to_string(),
        )
        .unwrap();
        let alias = "mcp__docs__nested__echo";
        let library = McpLibrary::new(directory.path(), None, false)
            .with_preserved_tool_selection(names(&[alias]));
        let supervisor = crate::McpSupervisor::new();
        let partial = library
            .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(partial.unresolved_tool_selection, [alias]);
        assert!(partial.temporarily_unavailable_tools.is_empty());
        let (_, old_registry) = pi_plugin::RegistriesBuilder::new()
            .register_plugins(vec![partial.plugin.clone()])
            .unwrap();
        assert!(old_registry.tool(alias).is_none());
        gate.add_permits(1);
        let snapshot = supervisor
            .wait_for(&names(&["docs__nested"]), std::time::Duration::from_secs(3))
            .await;
        assert!(matches!(
            snapshot.servers["docs__nested"],
            crate::supervisor::ConnectionReadiness::Ready(_)
        ));
        assert!(
            old_registry.tool(alias).is_none(),
            "ready completion must not mutate the published view"
        );
        let next = library
            .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
            .await
            .unwrap();
        assert!(next.unresolved_tool_selection.is_empty());
        let (_, registry) = pi_plugin::RegistriesBuilder::new()
            .register_plugins(vec![next.plugin.clone()])
            .unwrap();
        assert!(registry.tool(alias).is_some());
        assert!(
            registry
                .tool(&qualified_tool_name("docs__nested", "echo", |_| true))
                .is_some()
        );
        assert!(old_registry.tool(alias).is_none());
        supervisor.close().await.unwrap();
        task.abort();
    }

    #[test]
    fn selection_visibility_proof_never_accepts_a_hidden_raw_normalization_peer() {
        let alphabet = ["a", "b", "_", "-", ".", "😀"];
        let mut raw_names = vec![String::new()];
        for _ in 0..3 {
            let mut next = raw_names.clone();
            for prefix in &raw_names {
                for unit in alphabet {
                    next.push(format!("{prefix}{unit}"));
                }
            }
            raw_names = next;
        }
        for policy in [
            policy(McpExposure::Direct, &[("a.*", McpExposure::Hidden)]),
            policy(
                McpExposure::Hidden,
                &[("a*", McpExposure::Direct), ("*b", McpExposure::Hidden)],
            ),
            policy(
                McpExposure::Deferred,
                &[("😀", McpExposure::Hidden), ("a-b", McpExposure::Hidden)],
            ),
            policy(
                McpExposure::Hidden,
                &[("*", McpExposure::Direct), ("a_b", McpExposure::Hidden)],
            ),
            policy(McpExposure::Hidden, &[("a_b", McpExposure::Direct)]),
        ] {
            for raw in &raw_names {
                let suffix = crate::naming::identifier_component(raw);
                if matches!(visible_alias(&policy, &suffix), Visibility::Visible) {
                    assert_ne!(
                        policy.exposure_for(raw),
                        McpExposure::Hidden,
                        "unsafe proof for raw {raw:?} as {suffix:?}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn selection_standalone_defaults_empty_and_untrusted_project_is_never_a_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".pi")).unwrap();
        std::fs::write(
            project.path().join(".pi/mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"must-not-run"}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(directory.path(), Some(project.path()), false)
            .with_preserved_tool_selection(names(&["mcp__docs__echo"]))
            .with_tool_selection_candidates(names(&["mcp__docs__otherLane"]));
        let standalone = library.prepare_generation().await.unwrap();
        assert!(standalone.temporarily_unavailable_tools.is_empty());
        let supervisor = crate::McpSupervisor::new();
        let supervised = library
            .prepare_supervised_generation(supervisor.clone(), BTreeMap::new())
            .await
            .unwrap();
        assert!(supervised.temporarily_unavailable_tools.is_empty());
        assert!(supervised.server_readiness.is_empty());
        supervisor.close().await.unwrap();
    }

    #[test]
    fn selection_history_enlarges_allowance_but_never_startup_current_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut library = McpLibrary::new(directory.path(), None, false)
            .with_preserved_tool_selection(names(&["mcp__docs__current"]))
            .with_tool_selection_candidates(names(&["mcp__docs__otherLane", "mcp__removed__echo"]))
            .with_tool_selection_reader(Arc::new(|| Ok(names(&["mcp__docs__current"]))));
        let allowed = library.prepare_tool_selection(
            &BTreeMap::from([("docs".into(), McpServerPolicy::default())]),
            &BTreeMap::from([("docs".into(), McpServerReadiness::Pending)]),
            &[],
        );
        assert_eq!(
            allowed.allowed,
            names(&["mcp__docs__current", "mcp__docs__otherLane"])
        );
        let mut active = names(&["read", "codemode"]);
        library.merge_preserved_tool_selection(&mut active).unwrap();
        assert_eq!(active, names(&["read", "codemode", "mcp__docs__current"]));
        assert!(library.selection_diagnostic().is_none());
    }
}
