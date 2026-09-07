use tauri::Manager;
#[cfg(desktop)]
use tauri::RunEvent;
#[cfg(target_os = "macos")]
use tauri::WindowEvent;

mod agent_paths;
mod backend;
mod dictation;
mod event_sink;
mod files;
mod git;
mod git_utils;
#[cfg(desktop)]
mod menu;
#[cfg(not(desktop))]
#[path = "menu_mobile.rs"]
mod menu;
mod native_i18n;
mod notifications;
mod pi_runtime;
mod prompts;
mod settings;
mod shared;
mod state;
mod storage;
#[cfg(desktop)]
mod terminal;
#[cfg(not(desktop))]
#[path = "terminal_mobile.rs"]
mod terminal;
mod tray;
mod types;
mod utils;
mod window;
mod workspaces;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        // Avoid WebKit compositing issues on NVIDIA Linux setups (GBM buffer errors).
        if std::env::var_os("__NV_PRIME_RENDER_OFFLOAD").is_none() {
            std::env::set_var("__NV_PRIME_RENDER_OFFLOAD", "1");
        }
        let is_wayland = std::env::var("XDG_SESSION_TYPE")
            .map(|session| session.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
            || std::env::var_os("WAYLAND_DISPLAY").is_some();
        let has_nvidia = std::path::Path::new("/proc/driver/nvidia/version").exists();
        if is_wayland && has_nvidia && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
        {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        let is_x11 = !is_wayland && std::env::var_os("DISPLAY").is_some();
        // Work around sporadic blank WebKitGTK renders on X11 by disabling compositing mode.
        // Keep Wayland untouched because this can interfere with input behavior on some setups.
        if is_x11 && std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    #[cfg(desktop)]
    let builder = tauri::Builder::default()
        .manage(menu::MenuItemRegistry::<tauri::Wry>::default())
        .manage(native_i18n::NativeLocaleState::default())
        .manage(tray::TrayState::default())
        .on_menu_event(menu::handle_menu_event)
        .enable_macos_default_menu(false);

    #[cfg(not(desktop))]
    let builder = tauri::Builder::default();

    let builder = builder
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            #[cfg(target_os = "macos")]
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            #[cfg(desktop)]
            {
                // The Builder menu callback runs before core plugins register
                // PathResolver. Locale settings can only be read during setup.
                app.set_menu(menu::build_menu(app.handle())?)?;
            }
            let state = state::AppState::load(app.handle());
            app.manage(state);
            app.manage(pi_runtime::create_state().map_err(std::io::Error::other)?);
            #[cfg(target_os = "macos")]
            {
                let tray_state = app.state::<tray::TrayState>();
                tray::initialize(app.handle(), tray_state.inner())?;
            }
            #[cfg(target_os = "windows")]
            {
                if let Some(main_window) = app.get_webview_window("main") {
                    let _ = main_window.set_decorations(false);
                    // Keep menu accelerators wired while suppressing a visible native menu bar.
                    let _ = main_window.hide_menu();
                }
            }
            Ok(())
        });

    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_window_state::Builder::default().build());

    let app = builder
        .plugin(tauri_plugin_liquid_glass::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            settings::get_app_settings,
            settings::update_app_settings,
            files::file_read,
            files::file_write,
            files::read_image_as_data_url,
            files::write_text_file,
            pi_runtime::pi_desktop_info,
            menu::menu_set_accelerators,
            menu::menu_set_locale,
            tray::set_tray_recent_threads,
            workspaces::list_workspaces,
            workspaces::is_workspace_path_dir,
            workspaces::add_workspace,
            workspaces::add_workspace_from_git_url,
            workspaces::add_clone,
            workspaces::add_worktree,
            workspaces::worktree_setup_status,
            workspaces::worktree_setup_mark_ran,
            workspaces::remove_workspace,
            workspaces::remove_worktree,
            workspaces::rename_worktree,
            workspaces::rename_worktree_upstream,
            workspaces::apply_worktree_changes,
            workspaces::update_workspace_settings,
            pi_runtime::pi_start_thread,
            pi_runtime::pi_send_user_message,
            pi_runtime::pi_turn_steer,
            pi_runtime::pi_turn_interrupt,
            git::generate_commit_message,
            pi_runtime::pi_generate_run_metadata,
            pi_runtime::pi_resume_thread,
            pi_runtime::pi_read_thread,
            pi_runtime::pi_thread_live_subscribe,
            pi_runtime::pi_thread_live_unsubscribe,
            pi_runtime::pi_fork_thread,
            pi_runtime::pi_list_threads,
            pi_runtime::pi_list_archived_threads,
            pi_runtime::pi_archive_thread,
            pi_runtime::pi_unarchive_thread,
            pi_runtime::pi_delete_thread,
            pi_runtime::pi_compact_thread,
            pi_runtime::pi_set_thread_name,
            pi_runtime::pi_configure_thread,
            git::get_git_status,
            git::init_git_repo,
            git::create_github_repo,
            git::list_git_roots,
            git::get_git_diffs,
            git::get_git_log,
            git::get_git_commit_diff,
            git::get_git_remote,
            git::stage_git_file,
            git::stage_git_all,
            git::unstage_git_file,
            git::revert_git_file,
            git::revert_git_all,
            git::commit_git,
            git::push_git,
            git::pull_git,
            git::fetch_git,
            git::sync_git,
            git::get_github_issues,
            git::get_github_pull_requests,
            git::get_github_pull_request_diff,
            git::get_github_pull_request_comments,
            git::checkout_github_pull_request,
            workspaces::list_workspace_files,
            workspaces::read_workspace_file,
            workspaces::open_workspace_in,
            workspaces::get_open_app_icon,
            git::list_git_branches,
            git::checkout_git_branch,
            git::create_git_branch,
            pi_runtime::pi_model_list,
            pi_runtime::pi_skills_list,
            prompts::prompts_list,
            prompts::prompts_create,
            prompts::prompts_update,
            prompts::prompts_delete,
            prompts::prompts_move,
            prompts::prompts_workspace_dir,
            prompts::prompts_global_dir,
            terminal::terminal_open,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close,
            dictation::dictation_model_status,
            dictation::dictation_download_model,
            dictation::dictation_cancel_download,
            dictation::dictation_remove_model,
            dictation::dictation_start,
            dictation::dictation_request_permission,
            dictation::dictation_stop,
            dictation::dictation_cancel,
            notifications::is_macos_debug_build,
            notifications::app_build_type,
            notifications::send_notification_fallback,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    app.run(|app_handle, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            if let Some(window) = app_handle.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    });
}
