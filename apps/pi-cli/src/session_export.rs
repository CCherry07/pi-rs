use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use pi_core::{ContentBlock, CustomMessageContent, Message};
use pi_session::{
    AgentMessage, AgentSession, SessionDocument, SessionEntry, SessionError, inspect_session_file,
};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};
use serde_json::Value;

const HTML_STYLE: &str = r#"
:root { color-scheme: light dark; --bg:#f7f7f5; --panel:#fff; --text:#202124; --muted:#687076; --border:#d9d9d4; --user:#e9f2ff; --tool:#f3f3ef; --error:#8f1d1d; }
@media (prefers-color-scheme: dark) { :root { --bg:#151615; --panel:#1f211f; --text:#eceeec; --muted:#9da39e; --border:#3a3d3a; --user:#1d344d; --tool:#292b29; --error:#ffb4ab; } }
* { box-sizing:border-box; }
body { margin:0; background:var(--bg); color:var(--text); font:15px/1.55 ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
main { width:min(920px,calc(100% - 32px)); margin:32px auto 64px; }
header { margin-bottom:24px; }
h1 { margin:0 0 8px; font-size:24px; }
.meta,.label,.footer { color:var(--muted); }
.message,.notice { margin:14px 0; padding:16px 18px; border:1px solid var(--border); border-radius:12px; background:var(--panel); overflow-wrap:anywhere; }
.message.user { background:var(--user); }
.message.tool { background:var(--tool); }
.message.error { border-color:var(--error); }
.label { margin-bottom:8px; font-size:12px; font-weight:700; letter-spacing:.06em; text-transform:uppercase; }
.body > :first-child { margin-top:0; } .body > :last-child { margin-bottom:0; }
pre { overflow:auto; padding:12px; border-radius:8px; background:rgba(127,127,127,.12); white-space:pre-wrap; }
code { font-family:ui-monospace,SFMono-Regular,Consolas,monospace; }
blockquote { margin-left:0; padding-left:14px; border-left:3px solid var(--border); color:var(--muted); }
table { border-collapse:collapse; max-width:100%; } th,td { border:1px solid var(--border); padding:6px 8px; }
details { margin:8px 0; } summary { cursor:pointer; color:var(--muted); }
img { max-width:100%; height:auto; border-radius:8px; }
.footer { margin-top:28px; font-size:12px; }
@media print { body { background:#fff; } main { width:100%; margin:0; } .message,.notice { break-inside:avoid; } }
"#;

pub(crate) fn resolve_user_path(cwd: &Path, input: &str) -> PathBuf {
    let path = if input == "~" {
        home_directory().unwrap_or_else(|| PathBuf::from(input))
    } else if let Some(rest) = input.strip_prefix("~/") {
        home_directory()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(input))
    } else {
        PathBuf::from(input)
    };
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub(crate) fn validate_v4_import(path: &Path) -> Result<(), String> {
    inspect_session_file(path)
        .map(|_| ())
        .map_err(|error| format!("unsupported session format in {}: {error}", path.display()))
}

pub(crate) fn export_jsonl(
    session: &AgentSession,
    requested_path: Option<&str>,
) -> Result<PathBuf, String> {
    let destination = requested_path.map_or_else(
        || {
            session
                .runtime()
                .cwd()
                .join(format!("pi-rs-session-{}.jsonl", session.log().header().id))
        },
        |path| resolve_user_path(session.runtime().cwd(), path),
    );
    session
        .log()
        .export_branch(destination)
        .map_err(|error| error.to_string())
}

pub(crate) fn export_html(
    session: &AgentSession,
    requested_path: Option<&str>,
) -> Result<PathBuf, String> {
    let destination = requested_path.map_or_else(
        || {
            session
                .runtime()
                .cwd()
                .join(format!("pi-rs-session-{}.html", session.log().header().id))
        },
        |path| resolve_user_path(session.runtime().cwd(), path),
    );
    export_html_to(session, &destination)
}

pub(crate) fn export_html_to(
    session: &AgentSession,
    destination: &Path,
) -> Result<PathBuf, String> {
    if !session.log().is_materialized() {
        return Err("nothing to export yet; start a conversation first".to_string());
    }
    let document = session.log().load().map_err(|error| error.to_string())?;
    let rendered = render_document_html(&document).map_err(|error| error.to_string())?;
    atomic_write(destination, rendered.as_bytes()).map_err(|error| error.to_string())?;
    Ok(destination.to_path_buf())
}

fn render_document_html(document: &SessionDocument) -> Result<String, SessionError> {
    let branch = document.branch()?;
    let title = document
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Pi session {}", document.header.id));
    let mut output = String::new();
    let _ = write!(
        output,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{HTML_STYLE}</style></head><body><main><header><h1>{}</h1><div class=\"meta\">{} · {}</div></header>",
        escape_html(&title),
        escape_html(&title),
        escape_html(&document.header.id),
        escape_html(&document.header.cwd.display().to_string()),
    );
    for record in branch {
        render_entry(&mut output, &record.entry);
    }
    output.push_str(
        "<div class=\"footer\">Exported by pi-rs · active branch only</div></main></body></html>",
    );
    Ok(output)
}

fn render_entry(output: &mut String, entry: &SessionEntry) {
    match entry {
        SessionEntry::Message(message) => render_agent_message(output, &message.message),
        SessionEntry::CustomMessage(message) if message.display => {
            let text = custom_content_text(&message.content);
            render_article(
                output,
                "notice",
                &message.custom_type,
                &markdown_html(&text),
            );
        }
        SessionEntry::Compaction(compaction) => render_collapsible_notice(
            output,
            "Compaction summary",
            &markdown_html(&compaction.summary),
        ),
        SessionEntry::BranchSummary(summary) => {
            render_collapsible_notice(output, "Branch summary", &markdown_html(&summary.summary))
        }
        SessionEntry::ModelChange(model) => render_collapsible_notice(
            output,
            "Model changed",
            &format!(
                "<code>{}/{}</code>",
                escape_html(model.provider.as_str()),
                escape_html(model.model_id.as_str())
            ),
        ),
        SessionEntry::ThinkingLevelChange(thinking) => render_collapsible_notice(
            output,
            "Thinking level changed",
            &format!("<code>{}</code>", escape_html(&thinking.thinking_level)),
        ),
        _ => {}
    }
}

fn render_agent_message(output: &mut String, agent_message: &AgentMessage) {
    if let Some(message) = agent_message.as_standard() {
        match message {
            Message::User(user) => {
                let text = agent_message
                    .display_text()
                    .map(str::to_string)
                    .unwrap_or_else(|| user_text(&user.content));
                render_article(output, "message user", "You", &markdown_html(&text));
            }
            Message::Assistant(assistant) => {
                let body = render_content_blocks(&assistant.content, true);
                let class = if assistant.error_message.is_some() {
                    "message assistant error"
                } else {
                    "message assistant"
                };
                render_article(output, class, "Assistant", &body);
                if let Some(error) = &assistant.error_message {
                    render_article(
                        output,
                        "notice error",
                        "Provider error",
                        &format!("<pre>{}</pre>", escape_html(error)),
                    );
                }
            }
            Message::ToolResult(result) => {
                let class = if result.is_error {
                    "message tool error"
                } else {
                    "message tool"
                };
                let mut body = format!("<pre>{}</pre>", escape_html(&user_text(&result.content)));
                if let Some(details) = &result.details
                    && let Ok(details) = serde_json::to_string_pretty(details)
                {
                    let _ = write!(
                        body,
                        "<details><summary>Details</summary><pre>{}</pre></details>",
                        escape_html(&details)
                    );
                }
                render_article(
                    output,
                    class,
                    &format!("Tool · {}", result.tool_name),
                    &body,
                );
            }
            Message::Custom(custom) if custom.display => render_article(
                output,
                "notice",
                &custom.custom_type,
                &markdown_html(&custom_content_text(&custom.content)),
            ),
            Message::Custom(_) => {}
        }
        return;
    }

    if agent_message.role() == "bashExecution"
        && let Some(value) = agent_message.as_custom()
    {
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = value
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        render_article(
            output,
            "message tool",
            "Shell",
            &format!(
                "<pre><code>$ {}\n{}</code></pre>",
                escape_html(command),
                escape_html(result)
            ),
        );
    }
}

fn render_content_blocks(blocks: &[ContentBlock], markdown_text: bool) -> String {
    let mut output = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) if markdown_text => {
                output.push_str(&markdown_html(&text.text))
            }
            ContentBlock::Text(text) => {
                let _ = write!(output, "<pre>{}</pre>", escape_html(&text.text));
            }
            ContentBlock::Thinking(thinking) => {
                let body = if thinking.redacted == Some(true) {
                    "<em>Redacted reasoning</em>".to_string()
                } else {
                    markdown_html(&thinking.thinking)
                };
                let _ = write!(
                    output,
                    "<details><summary>Thinking</summary>{body}</details>"
                );
            }
            ContentBlock::Image(image) if safe_embedded_image(&image.mime_type) => {
                let _ = write!(
                    output,
                    "<img alt=\"Embedded image\" src=\"data:{};base64,{}\">",
                    escape_html(&image.mime_type),
                    escape_html(&image.data)
                );
            }
            ContentBlock::Image(image) => {
                let _ = write!(
                    output,
                    "<div class=\"meta\">Image omitted ({})</div>",
                    escape_html(&image.mime_type)
                );
            }
            ContentBlock::ToolCall(call) => {
                let arguments = serde_json::to_string_pretty(&call.arguments)
                    .unwrap_or_else(|_| call.arguments.to_string());
                let _ = write!(
                    output,
                    "<details><summary>Tool call · {}</summary><pre>{}</pre></details>",
                    escape_html(&call.name),
                    escape_html(&arguments)
                );
            }
        }
    }
    output
}

fn render_article(output: &mut String, class: &str, label: &str, body: &str) {
    let _ = write!(
        output,
        "<article class=\"{}\"><div class=\"label\">{}</div><div class=\"body\">{}</div></article>",
        escape_html(class),
        escape_html(label),
        body
    );
}

fn render_collapsible_notice(output: &mut String, label: &str, body: &str) {
    let _ = write!(
        output,
        "<details class=\"notice\"><summary>{}</summary><div class=\"body\">{}</div></details>",
        escape_html(label),
        body
    );
}

fn markdown_html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let parser = Parser::new_ext(markdown, options).map(sanitize_markdown_event);
    let mut output = String::new();
    html::push_html(&mut output, parser);
    output
}

fn sanitize_markdown_event(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_link_destination(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_link_destination(dest_url),
            title,
            id,
        }),
        event => event,
    }
}

fn safe_link_destination(destination: CowStr<'_>) -> CowStr<'_> {
    let trimmed = destination.trim();
    let lower = trimmed.to_ascii_lowercase();
    let has_unsafe_scheme = lower
        .split_once(':')
        .is_some_and(|(scheme, _)| !matches!(scheme, "http" | "https" | "mailto"));
    if has_unsafe_scheme {
        CowStr::from("#")
    } else {
        destination
    }
}

fn user_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn custom_content_text(content: &CustomMessageContent) -> String {
    match content {
        CustomMessageContent::Text(text) => text.clone(),
        CustomMessageContent::Blocks(blocks) => user_text(blocks),
    }
}

fn safe_embedded_image(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

fn escape_html(input: &str) -> String {
    input.chars().fold(
        String::with_capacity(input.len()),
        |mut output, character| {
            match character {
                '&' => output.push_str("&amp;"),
                '<' => output.push_str("&lt;"),
                '>' => output.push_str("&gt;"),
                '"' => output.push_str("&quot;"),
                '\'' => output.push_str("&#39;"),
                _ => output.push(character),
            }
            output
        },
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("export destination is a directory: {}", path.display()),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = sibling_transaction_path(path, "export");
    let backup = path
        .exists()
        .then(|| sibling_transaction_path(path, "backup"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Some(backup) = &backup {
            std::fs::rename(path, backup)?;
        }
        if let Err(error) = std::fs::rename(&temporary, path) {
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, path);
            }
            return Err(error);
        }
        if let Some(backup) = &backup {
            let _ = std::fs::remove_file(backup);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn sibling_transaction_path(path: &Path, purpose: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "session".into(), |name| name.to_os_string());
    name.push(format!(".{purpose}-{}.tmp", uuid::Uuid::now_v7()));
    path.with_file_name(name)
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use pi_core::{AssistantMessage, StopReason, Usage, UserMessage};
    use pi_session::{SessionHeader, SessionLog};

    use super::*;

    #[test]
    fn html_export_renders_markdown_and_escapes_active_content() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("session.jsonl"),
            SessionHeader::new("html", directory.path()),
        )
        .unwrap();
        log.append_message(Message::User(UserMessage::text(
            "# Request\n<script>alert(1)</script>\n[bad](javascript:alert(1))",
            1,
        )))
        .unwrap();
        log.append_message(Message::assistant(AssistantMessage {
            content: vec![ContentBlock::Text(pi_core::TextContent::new("**Done**"))],
            api: "test".to_string(),
            provider: "test".into(),
            model: "test".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            deferred: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp_ms: 2,
        }))
        .unwrap();
        let document = log.load().unwrap();

        let html = render_document_html(&document).unwrap();

        assert!(html.contains("<h1>Request</h1>"));
        assert!(html.contains("<strong>Done</strong>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("javascript:alert"));
    }

    #[test]
    fn import_validation_accepts_legacy_and_v4_headers() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy.jsonl");
        std::fs::write(
            &legacy,
            r#"{"type":"session","version":3,"id":"legacy"}
"#,
        )
        .unwrap();
        let current = directory.path().join("current.jsonl");
        SessionLog::create(&current, SessionHeader::new("current", directory.path())).unwrap();

        assert!(validate_v4_import(&legacy).is_ok());
        assert!(validate_v4_import(&current).is_ok());
    }
}
