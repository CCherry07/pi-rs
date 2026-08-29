use std::path::Path;
use std::process::Command;

use pi_session::AgentSession;

use crate::session_export::export_html_to;

const DEFAULT_SHARE_VIEWER_URL: &str = "https://pi.dev/session/";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SharedSession {
    viewer_url: String,
    gist_url: String,
}

trait GistUploader: Send + 'static {
    fn upload_html(self, path: &Path) -> Result<SharedSession, String>;
}

struct GitHubCliUploader;

impl GistUploader for GitHubCliUploader {
    fn upload_html(self, path: &Path) -> Result<SharedSession, String> {
        let authentication = Command::new("gh")
            .args(["auth", "status"])
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    "GitHub CLI (gh) is not installed; install it and run `gh auth login`"
                        .to_string()
                } else {
                    format!("failed to check GitHub CLI authentication: {error}")
                }
            })?;
        if !authentication.status.success() {
            return Err("GitHub CLI is not logged in; run `gh auth login` first".to_string());
        }

        let created = Command::new("gh")
            .args(["gist", "create", "--public=false"])
            .arg(path)
            .output()
            .map_err(|error| format!("failed to start GitHub CLI: {error}"))?;
        if !created.status.success() {
            let stderr = String::from_utf8_lossy(&created.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("failed to create secret gist: {}", created.status)
            } else {
                format!("failed to create secret gist: {stderr}")
            });
        }
        let stdout = String::from_utf8_lossy(&created.stdout);
        let gist_url = extract_gist_url(&stdout)
            .ok_or_else(|| "GitHub CLI did not return a gist URL".to_string())?;
        let gist_id =
            gist_id(&gist_url).ok_or_else(|| format!("could not parse gist ID from {gist_url}"))?;
        let base = std::env::var("PI_SHARE_VIEWER_URL")
            .unwrap_or_else(|_| DEFAULT_SHARE_VIEWER_URL.to_string());
        Ok(SharedSession {
            viewer_url: viewer_url(&base, gist_id),
            gist_url,
        })
    }
}

pub(crate) async fn share_session(session: &AgentSession) -> Result<String, String> {
    let shared = share_session_with(session, GitHubCliUploader).await?;
    Ok(format!(
        "Share URL: {}\nGist: {}",
        shared.viewer_url, shared.gist_url
    ))
}

async fn share_session_with<U: GistUploader>(
    session: &AgentSession,
    uploader: U,
) -> Result<SharedSession, String> {
    let temporary = tempfile::tempdir()
        .map_err(|error| format!("failed to create temporary share directory: {error}"))?;
    let html_path = temporary.path().join("session.html");
    export_html_to(session, &html_path)?;
    tokio::task::spawn_blocking(move || {
        let result = uploader.upload_html(&html_path);
        drop(temporary);
        result
    })
    .await
    .map_err(|error| format!("share task failed: {error}"))?
}

fn extract_gist_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .rev()
        .find(|part| part.starts_with("https://gist.github.com/"))
        .map(|part| part.trim_end_matches('/').to_string())
}

fn gist_id(url: &str) -> Option<&str> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|part| part.split(['?', '#']).next())
        .filter(|part| !part.is_empty())
}

fn viewer_url(base: &str, gist_id: &str) -> String {
    format!("{}#{gist_id}", base.trim_end_matches('#'))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pi_agent::AgentOptions;
    use pi_core::{Message, ProviderId, UserMessage};
    use pi_runtime::{PiRuntime, SystemPrompt};
    use pi_session::{AgentSession, SessionHeader, SessionLog};

    use super::*;

    struct RecordingUploader {
        observed: Arc<Mutex<Option<String>>>,
    }

    impl GistUploader for RecordingUploader {
        fn upload_html(self, path: &Path) -> Result<SharedSession, String> {
            let html = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
            *self
                .observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(html);
            Ok(SharedSession {
                viewer_url: "https://viewer.invalid/#abc".to_string(),
                gist_url: "https://gist.github.com/example/abc".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn share_exports_html_before_calling_the_uploader() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let log = SessionLog::create(&path, SessionHeader::new("share", directory.path())).unwrap();
        log.append_message(Message::User(UserMessage::text("share me", 1)))
            .unwrap();
        drop(log);
        let runtime = PiRuntime::builder()
            .provider_plugin(
                pi_plugin_openai::OpenAiCompatiblePlugin::new(
                    pi_plugin_openai::OpenAiCompatibleConfig::without_api_key(
                        "https://example.invalid/v1",
                    ),
                )
                .unwrap(),
            )
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("openai-compatible"),
                cwd: directory.path().to_path_buf(),
                ..AgentOptions::default()
            })
            .system_prompt(SystemPrompt::Final("test".to_string()))
            .build()
            .unwrap();
        let session = AgentSession::open(runtime, &path).await.unwrap();
        let observed = Arc::new(Mutex::new(None));

        let shared = share_session_with(
            &session,
            RecordingUploader {
                observed: Arc::clone(&observed),
            },
        )
        .await
        .unwrap();

        assert_eq!(shared.viewer_url, "https://viewer.invalid/#abc");
        let html = observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap();
        assert!(html.contains("share me"));
        session.shutdown().await;
    }

    #[test]
    fn parses_gist_output_and_builds_the_pi_viewer_url() {
        let output = "Created secret gist: https://gist.github.com/example/abc123\n";
        let url = extract_gist_url(output).unwrap();

        assert_eq!(url, "https://gist.github.com/example/abc123");
        assert_eq!(gist_id(&url), Some("abc123"));
        assert_eq!(
            viewer_url("https://pi.dev/session/", "abc123"),
            "https://pi.dev/session/#abc123"
        );
    }
}
