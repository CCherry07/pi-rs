//! Readiness setup for tests of discovery, grants and result projection, not startup latency.
//! Indirect MCP attempts do not gate construction. Only complete idle generations publish;
//! never retry a model turn or tool execution to hide a missing-tool failure.
use std::time::Duration;

use pi_session::{AgentSessionEvent, PiSession};

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn wait_for_tools(session: &PiSession, names: &[&str]) {
    tokio::time::timeout(READINESS_TIMEOUT, async {
        loop {
            let tools = session.current().catalog().tool_specs();
            if names
                .iter()
                .all(|name| tools.iter().any(|tool| tool.name == *name))
            {
                return;
            }
            session.reload().await.expect("readiness setup reload");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("MCP catalog did not become ready for {names:?}"));
}

pub(super) async fn status(session: &PiSession) -> String {
    let current = session.current();
    let mut events = current.subscribe().events;
    current
        .prompt("/mcp status")
        .await
        .expect("read MCP status");
    let mut notices = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AgentSessionEvent::PluginNotice { message, .. } = event.event {
            notices.push(message);
        }
    }
    notices.join("\n")
}

pub(super) async fn wait_for_status(session: &PiSession, server: &str, expected: &str) -> String {
    tokio::time::timeout(READINESS_TIMEOUT, async {
        loop {
            let report = status(session).await;
            if report
                .lines()
                .any(|line| line.starts_with(&format!("{server} · ")) && line.contains(expected))
            {
                return report;
            }
            session.reload().await.expect("readiness setup reload");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("MCP server {server} did not reach {expected}"))
}
