//! OS/browser/terminal ownership for MCP OAuth. The library owns discovery, PKCE,
//! state validation and credentials; no OS interaction leaks into reusable crates.
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{AbortHandle, AbortSignal};
use pi_plugin_mcp::{
    McpOAuthError, McpOAuthHost, McpOAuthLoginRequest, McpOAuthPrompt, McpOAuthRedirect,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[async_trait]
trait BrowserOpener: Send + Sync {
    async fn open(&self, url: &str) -> Result<(), ()>;
}
struct SystemBrowser;
#[async_trait]
impl BrowserOpener for SystemBrowser {
    async fn open(&self, url: &str) -> Result<(), ()> {
        // Arguments are not shell-expanded. Never log the callback or tokens.
        #[cfg(target_os = "macos")]
        let mut command = tokio::process::Command::new("open");
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut c = tokio::process::Command::new("rundll32");
            c.arg("url.dll,FileProtocolHandler");
            c
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut command = tokio::process::Command::new("xdg-open");
        command
            .arg(url)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match tokio::time::timeout(Duration::from_secs(5), command.status()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => Err(()),
        }
    }
}

pub(crate) struct NativeMcpOAuthHost {
    browser: Arc<dyn BrowserOpener>,
}
impl Default for NativeMcpOAuthHost {
    fn default() -> Self {
        Self {
            browser: Arc::new(SystemBrowser),
        }
    }
}

#[async_trait]
impl McpOAuthHost for NativeMcpOAuthHost {
    async fn sign_in(
        &self,
        request: McpOAuthLoginRequest,
        prompt: Arc<dyn McpOAuthPrompt>,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError> {
        let (redirect_url, listener) =
            bind_callback(request.callback_url(), request.callback_port()).await?;
        if listener.is_none() {
            prompt.notify("Could not start a loopback listener at the configured redirect. Paste the complete redirected address after authorization, even if the browser shows a connection error.").map_err(McpOAuthError::Failed)?;
        }
        let (mut flow, redirect) = request.start(&redirect_url, signal.clone()).await?;
        validate_browser_url(&redirect.authorization_url)?;
        prompt
            .show_authorization_url(&redirect.authorization_url)
            .map_err(McpOAuthError::Failed)?;
        tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            opened = self.browser.open(&redirect.authorization_url) => if opened.is_err() {
                prompt.notify("Could not open a browser automatically. Open the authorization URL above manually.").map_err(McpOAuthError::Failed)?;
            }
        }
        let response = tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            callback = receive_callback(listener, &redirect) => callback?,
            pasted = prompt.redirect_url(signal.clone()) => pasted.map_err(McpOAuthError::Failed)?.ok_or(McpOAuthError::Cancelled)?,
        };
        flow.complete_redirect_url(response.trim(), signal).await
    }
}

fn validate_browser_url(value: &str) -> Result<(), McpOAuthError> {
    let valid = Url::parse(value).ok().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
    });
    if valid {
        Ok(())
    } else {
        Err(McpOAuthError::Failed(
            "Authorization endpoint must be an HTTP(S) browser URL without userinfo".into(),
        ))
    }
}

async fn bind_callback(
    configured: Option<&str>,
    port: Option<u16>,
) -> Result<(String, Option<TcpListener>), McpOAuthError> {
    let default_url = format!("http://127.0.0.1:{}/callback", port.unwrap_or(0));
    let mut redirect = Url::parse(configured.unwrap_or(&default_url))
        .map_err(|_| McpOAuthError::Failed("Invalid OAuth callback URL".into()))?;
    let ip = match redirect.host() {
        Some(url::Host::Domain("localhost")) => {
            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        }
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => Some(ip.into()),
        Some(url::Host::Ipv6(ip)) if ip.is_loopback() => Some(ip.into()),
        _ => None,
    };
    let Some(ip) = ip.filter(|_| redirect.scheme() == "http") else {
        return Ok((redirect.into(), None));
    };
    match TcpListener::bind((ip, redirect.port_or_known_default().unwrap_or(0))).await {
        Ok(listener) => {
            let port = listener
                .local_addr()
                .map_err(|_| McpOAuthError::Failed("Could not inspect callback listener".into()))?
                .port();
            redirect
                .set_port(Some(port))
                .map_err(|_| McpOAuthError::Failed("Invalid callback port".into()))?;
            Ok((redirect.into(), Some(listener)))
        }
        Err(_) => {
            // Port zero is not a usable manual redirect if an ephemeral bind failed.
            if redirect.port() == Some(0) {
                redirect.set_port(Some(53682)).expect("HTTP URL");
            }
            Ok((redirect.into(), None))
        }
    }
}

async fn receive_callback(
    listener: Option<TcpListener>,
    redirect: &McpOAuthRedirect,
) -> Result<String, McpOAuthError> {
    let Some(listener) = listener else {
        return std::future::pending().await;
    };
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|_| {
            McpOAuthError::Failed("OAuth callback listener failed; retry login".into())
        })?;
        let response = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).await.ok()?;
                if count == 0 { return None; }
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > 8192 { return None; }
                if bytes.windows(4).any(|part| part == b"\r\n\r\n") { break; }
            }
            let valid = callback_target(&bytes, redirect);
            let (status, message) = if valid.is_some() {
                ("200 OK", "Authorization callback received. Return to Pi for final status.")
            } else {
                ("400 Bad Request", "Invalid callback. Return to the authorization browser tab.")
            };
            let reply = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{message}", message.len());
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
            valid
        }).await;
        if let Ok(Some(callback)) = response {
            return Ok(callback);
        }
    }
}

fn callback_target(request: &[u8], redirect: &McpOAuthRedirect) -> Option<String> {
    let line = std::str::from_utf8(request).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if !target.starts_with('/') || target.starts_with("//") || target.contains('#') {
        return None;
    }
    let expected = Url::parse(&redirect.redirect_url).ok()?;
    let received = expected.join(target).ok()?;
    if received.origin() != expected.origin()
        || received.path() != expected.path()
        || received
            .query_pairs()
            .filter(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .collect::<Vec<_>>()
            != [redirect.state.clone()]
    {
        return None;
    }
    Some(received.into())
}

pub(crate) struct ShellOAuthPrompt;
#[async_trait]
impl McpOAuthPrompt for ShellOAuthPrompt {
    fn show_authorization_url(&self, url: &str) -> Result<(), String> {
        println!("Authorize MCP access in your browser:\n{url}");
        Ok(())
    }
    fn notify(&self, message: &str) -> Result<(), String> {
        eprintln!("{message}");
        Ok(())
    }
    async fn redirect_url(&self, signal: AbortSignal) -> Result<Option<String>, String> {
        // Agent-launched shell commands can complete through the browser callback
        // without a TTY. Never consume their piped stdin or start a raw-mode reader.
        if !std::io::stdin().is_terminal() {
            signal.wait().await;
            return Ok(None);
        }
        // An owned, cancellable reader: browser completion must not strand a
        // blocking stdin task or leave the terminal raw until the user hits Enter.
        let (abort, stop) = AbortHandle::new();
        let (send, receive) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let _ = send.send(read_pasted_redirect(stop));
        });
        let _reader = ShellReader {
            abort,
            thread: Some(thread),
        };
        tokio::select! {
            biased;
            () = signal.wait() => Ok(None),
            result = receive => result.map_err(|_| "MCP callback input stopped unexpectedly".to_string())?,
        }
    }
}
struct ShellReader {
    abort: AbortHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for ShellReader {
    fn drop(&mut self) {
        self.abort.abort();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn read_pasted_redirect(signal: AbortSignal) -> Result<Option<String>, String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    eprintln!(
        "Waiting for callback; or paste the complete redirect URL (hidden). Esc/Ctrl-C cancels."
    );
    crossterm::terminal::enable_raw_mode().map_err(|_| "Could not start MCP callback input")?;
    let _raw = RawMode;
    let mut value = String::new();
    while !signal.is_aborted() {
        if !event::poll(Duration::from_millis(25))
            .map_err(|_| "Could not read MCP callback input")?
        {
            continue;
        }
        match event::read().map_err(|_| "Could not read MCP callback input")? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => return Ok((!value.trim().is_empty()).then_some(value)),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    value.push(ch)
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                _ => {}
            },
            Event::Paste(paste) => value.push_str(&paste),
            _ => {}
        }
        if value.len() > 16384 {
            return Err("MCP callback URL is too long".into());
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
