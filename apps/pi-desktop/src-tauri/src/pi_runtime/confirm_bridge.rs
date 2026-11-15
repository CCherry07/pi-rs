//! Desktop confirmation bridge for plugins.
//!
//! Desktop runs the Pi runtime in RPC presentation mode and does not render
//! declarative plugin surfaces, so semantic confirmations (Computer Use send,
//! delete, foreground steps) are answered by a native sheet attached to the
//! main window. A dismissed or failed sheet is a refusal. Without a main
//! window there is no confirmation surface: the bridge reports that instead
//! of opening an unparented system alert, so the task stops for review.

use async_trait::async_trait;
use pi_session::PluginUiBridge;
use tauri::{AppHandle, Manager, Runtime, UserAttentionType};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

const MAIN_WINDOW: &str = "main";
/// First button: AppKit binds Return to it.
const DECLINE: &str = "取消";
const APPROVE: &str = "继续";

/// Only an explicit press of the approval button approves.
fn is_approval(result: Option<&MessageDialogResult>) -> bool {
    matches!(result, Some(MessageDialogResult::Custom(label)) if label == APPROVE)
}

pub(crate) struct DesktopConfirmBridge<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> DesktopConfirmBridge<R> {
    pub(crate) fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

#[async_trait]
impl<R: Runtime> PluginUiBridge for DesktopConfirmBridge<R> {
    async fn confirm(&self, title: String, message: String) -> Result<Option<bool>, String> {
        // An unparented dialog is hosted by a separate macOS notification
        // process, outside the conversation, and has answered without a
        // click. Only an app-owned sheet counts as a confirmation surface.
        let Some(window) = self.app.get_webview_window(MAIN_WINDOW) else {
            return Ok(None);
        };
        // Never take keyboard focus: a Return typed into another app would
        // land on the sheet. Ask for attention instead, and make the refusal
        // the default button so a stray Return can only decline.
        let _ = window.show();
        let _ = window.request_user_attention(Some(UserAttentionType::Critical));
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.app
            .dialog()
            .message(message)
            .title(title)
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                DECLINE.into(),
                APPROVE.into(),
            ))
            .parent(&window)
            .show_with_result(move |result| {
                let _ = sender.send(result);
            });
        let answer = receiver.await.ok();
        Ok(Some(is_approval(answer.as_ref())))
    }

    fn supports_confirm(&self) -> bool {
        self.app.get_webview_window(MAIN_WINDOW).is_some()
    }
}

/// Debug-build acceptance hook: `PI_DESKTOP_CONFIRM_SELFTEST=1` shows one
/// confirmation through the bridge at startup and records the answer in
/// `PI_DESKTOP_CONFIRM_SELFTEST_OUT` (default `/tmp/pi-desktop-confirm.txt`).
#[cfg(debug_assertions)]
pub(crate) fn spawn_selftest<R: Runtime>(app: AppHandle<R>) {
    if std::env::var_os("PI_DESKTOP_CONFIRM_SELFTEST").is_none() {
        return;
    }
    let out = std::env::var("PI_DESKTOP_CONFIRM_SELFTEST_OUT")
        .unwrap_or_else(|_| "/tmp/pi-desktop-confirm.txt".into());
    tauri::async_runtime::spawn(async move {
        let bridge = DesktopConfirmBridge::new(app);
        let answer = bridge
            .confirm(
                "确认桌面操作".into(),
                "在 WeChat 中执行「发送」，将提交「明天开会」。这一步可能无法撤销，是否继续？"
                    .into(),
            )
            .await;
        let _ = std::fs::write(&out, format!("{answer:?}\n"));
    });
}
