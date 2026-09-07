use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use tauri::menu::MenuItem;
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, Submenu};
use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};
use tauri::{Manager, Runtime};

use crate::native_i18n::{locale_for_app, strings, NativeLocale, NativeLocaleState};

pub struct MenuItemRegistry<R: Runtime> {
    items: Mutex<HashMap<String, MenuItem<R>>>,
}

impl<R: Runtime> Default for MenuItemRegistry<R> {
    fn default() -> Self {
        Self {
            items: Mutex::new(HashMap::new()),
        }
    }
}

impl<R: Runtime> MenuItemRegistry<R> {
    fn register(&self, id: &str, item: &MenuItem<R>) {
        if let Ok(mut items) = self.items.lock() {
            items.insert(id.to_string(), item.clone());
        }
    }

    fn set_accelerator(&self, id: &str, accelerator: Option<&str>) -> tauri::Result<bool> {
        let item = match self.items.lock() {
            Ok(items) => items.get(id).cloned(),
            Err(_) => return Ok(false),
        };
        if let Some(item) = item {
            item.set_accelerator(accelerator)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MenuAcceleratorUpdate {
    pub id: String,
    pub accelerator: Option<String>,
}

#[tauri::command]
pub fn menu_set_accelerators<R: Runtime>(
    app: tauri::AppHandle<R>,
    updates: Vec<MenuAcceleratorUpdate>,
) -> Result<(), String> {
    let registry = app.state::<MenuItemRegistry<R>>();
    for update in updates {
        registry
            .set_accelerator(&update.id, update.accelerator.as_deref())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn menu_set_locale<R: Runtime>(app: tauri::AppHandle<R>, locale: String) -> Result<(), String> {
    app.state::<NativeLocaleState>()
        .set(NativeLocale::from_language_tag(&locale));
    let menu = build_menu(&app).map_err(|error| error.to_string())?;
    app.set_menu(menu).map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide_menu();
    }
    crate::tray::refresh_menu(&app)?;
    Ok(())
}

pub(crate) fn build_menu<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
) -> tauri::Result<Menu<R>> {
    let registry = handle.state::<MenuItemRegistry<R>>();
    let labels = strings(locale_for_app(handle));
    let app_name = handle.package_info().name.clone();
    let about_item =
        MenuItemBuilder::with_id("about", format!("{} {app_name}", labels.about)).build(handle)?;
    let check_updates_item =
        MenuItemBuilder::with_id("check_for_updates", labels.check_updates).build(handle)?;
    let settings_item = MenuItemBuilder::with_id("file_open_settings", labels.settings)
        .accelerator("CmdOrCtrl+,")
        .build(handle)?;
    let app_menu = Submenu::with_items(
        handle,
        app_name.clone(),
        true,
        &[
            &about_item,
            &check_updates_item,
            &settings_item,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::services(handle, Some(labels.services))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::hide(handle, Some(labels.hide))?,
            &PredefinedMenuItem::hide_others(handle, Some(labels.hide_others))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::quit(handle, Some(labels.quit))?,
        ],
    )?;

    let new_agent_item =
        MenuItemBuilder::with_id("file_new_agent", labels.new_agent).build(handle)?;
    let new_worktree_agent_item =
        MenuItemBuilder::with_id("file_new_worktree_agent", labels.new_worktree_agent)
            .build(handle)?;
    let new_clone_agent_item =
        MenuItemBuilder::with_id("file_new_clone_agent", labels.new_clone_agent).build(handle)?;
    let add_workspace_item =
        MenuItemBuilder::with_id("file_add_workspace", labels.add_workspaces).build(handle)?;
    let add_workspace_from_url_item =
        MenuItemBuilder::with_id("file_add_workspace_from_url", labels.add_workspace_from_url)
            .build(handle)?;

    registry.register("file_new_agent", &new_agent_item);
    registry.register("file_new_worktree_agent", &new_worktree_agent_item);
    registry.register("file_new_clone_agent", &new_clone_agent_item);

    #[cfg(target_os = "linux")]
    let file_menu = {
        let close_window_item =
            MenuItemBuilder::with_id("file_close_window", labels.close_window).build(handle)?;
        let quit_item = MenuItemBuilder::with_id("file_quit", labels.quit).build(handle)?;
        Submenu::with_items(
            handle,
            labels.file,
            true,
            &[
                &new_agent_item,
                &new_worktree_agent_item,
                &new_clone_agent_item,
                &PredefinedMenuItem::separator(handle)?,
                &add_workspace_item,
                &add_workspace_from_url_item,
                &PredefinedMenuItem::separator(handle)?,
                &close_window_item,
                &quit_item,
            ],
        )?
    };
    #[cfg(not(target_os = "linux"))]
    let file_menu = Submenu::with_items(
        handle,
        labels.file,
        true,
        &[
            &new_agent_item,
            &new_worktree_agent_item,
            &new_clone_agent_item,
            &PredefinedMenuItem::separator(handle)?,
            &add_workspace_item,
            &add_workspace_from_url_item,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::close_window(handle, Some(labels.close_window))?,
            #[cfg(not(target_os = "macos"))]
            &PredefinedMenuItem::quit(handle, Some(labels.quit))?,
        ],
    )?;

    let edit_menu = Submenu::with_items(
        handle,
        labels.edit,
        true,
        &[
            &PredefinedMenuItem::undo(handle, Some(labels.undo))?,
            &PredefinedMenuItem::redo(handle, Some(labels.redo))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::cut(handle, Some(labels.cut))?,
            &PredefinedMenuItem::copy(handle, Some(labels.copy))?,
            &PredefinedMenuItem::paste(handle, Some(labels.paste))?,
            &PredefinedMenuItem::select_all(handle, Some(labels.select_all))?,
        ],
    )?;

    let cycle_model_item = MenuItemBuilder::with_id("composer_cycle_model", labels.cycle_model)
        .accelerator("CmdOrCtrl+Shift+M")
        .build(handle)?;
    let cycle_reasoning_item =
        MenuItemBuilder::with_id("composer_cycle_reasoning", labels.cycle_reasoning)
            .accelerator("CmdOrCtrl+Shift+R")
            .build(handle)?;
    registry.register("composer_cycle_model", &cycle_model_item);
    registry.register("composer_cycle_reasoning", &cycle_reasoning_item);

    let composer_menu = Submenu::with_items(
        handle,
        labels.composer,
        true,
        &[&cycle_model_item, &cycle_reasoning_item],
    )?;

    let toggle_projects_sidebar_item = MenuItemBuilder::with_id(
        "view_toggle_projects_sidebar",
        labels.toggle_projects_sidebar,
    )
    .build(handle)?;
    let toggle_git_sidebar_item =
        MenuItemBuilder::with_id("view_toggle_git_sidebar", labels.toggle_git_sidebar)
            .build(handle)?;
    let toggle_debug_panel_item =
        MenuItemBuilder::with_id("view_toggle_debug_panel", labels.toggle_debug_panel)
            .accelerator("CmdOrCtrl+Shift+D")
            .build(handle)?;
    let toggle_terminal_item =
        MenuItemBuilder::with_id("view_toggle_terminal", labels.toggle_terminal)
            .accelerator("CmdOrCtrl+Shift+T")
            .build(handle)?;
    let next_agent_item =
        MenuItemBuilder::with_id("view_next_agent", labels.next_agent).build(handle)?;
    let prev_agent_item =
        MenuItemBuilder::with_id("view_prev_agent", labels.previous_agent).build(handle)?;
    let next_workspace_item =
        MenuItemBuilder::with_id("view_next_workspace", labels.next_workspace).build(handle)?;
    let prev_workspace_item =
        MenuItemBuilder::with_id("view_prev_workspace", labels.previous_workspace).build(handle)?;
    registry.register(
        "view_toggle_projects_sidebar",
        &toggle_projects_sidebar_item,
    );
    registry.register("view_toggle_git_sidebar", &toggle_git_sidebar_item);
    registry.register("view_toggle_debug_panel", &toggle_debug_panel_item);
    registry.register("view_toggle_terminal", &toggle_terminal_item);
    registry.register("view_next_agent", &next_agent_item);
    registry.register("view_prev_agent", &prev_agent_item);
    registry.register("view_next_workspace", &next_workspace_item);
    registry.register("view_prev_workspace", &prev_workspace_item);

    #[cfg(target_os = "linux")]
    let view_menu = {
        let fullscreen_item =
            MenuItemBuilder::with_id("view_fullscreen", labels.toggle_full_screen).build(handle)?;
        Submenu::with_items(
            handle,
            labels.view,
            true,
            &[
                &toggle_projects_sidebar_item,
                &toggle_git_sidebar_item,
                &PredefinedMenuItem::separator(handle)?,
                &toggle_debug_panel_item,
                &toggle_terminal_item,
                &PredefinedMenuItem::separator(handle)?,
                &next_agent_item,
                &prev_agent_item,
                &next_workspace_item,
                &prev_workspace_item,
                &PredefinedMenuItem::separator(handle)?,
                &fullscreen_item,
            ],
        )?
    };
    #[cfg(not(target_os = "linux"))]
    let view_menu = Submenu::with_items(
        handle,
        labels.view,
        true,
        &[
            &toggle_projects_sidebar_item,
            &toggle_git_sidebar_item,
            &PredefinedMenuItem::separator(handle)?,
            &toggle_debug_panel_item,
            &toggle_terminal_item,
            &PredefinedMenuItem::separator(handle)?,
            &next_agent_item,
            &prev_agent_item,
            &next_workspace_item,
            &prev_workspace_item,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::fullscreen(handle, Some(labels.toggle_full_screen))?,
        ],
    )?;

    #[cfg(target_os = "linux")]
    let window_menu = {
        let minimize_item =
            MenuItemBuilder::with_id("window_minimize", labels.minimize).build(handle)?;
        let maximize_item =
            MenuItemBuilder::with_id("window_maximize", labels.maximize).build(handle)?;
        let close_item =
            MenuItemBuilder::with_id("window_close", labels.close_window).build(handle)?;
        Submenu::with_items(
            handle,
            labels.window,
            true,
            &[
                &minimize_item,
                &maximize_item,
                &PredefinedMenuItem::separator(handle)?,
                &close_item,
            ],
        )?
    };
    #[cfg(not(target_os = "linux"))]
    let window_menu = Submenu::with_items(
        handle,
        labels.window,
        true,
        &[
            &PredefinedMenuItem::minimize(handle, Some(labels.minimize))?,
            &PredefinedMenuItem::maximize(handle, Some(labels.maximize))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::close_window(handle, Some(labels.close_window))?,
        ],
    )?;

    #[cfg(target_os = "linux")]
    let help_menu = {
        let about_item =
            MenuItemBuilder::with_id("help_about", format!("{} {app_name}", labels.about))
                .build(handle)?;
        Submenu::with_items(handle, labels.help, true, &[&about_item])?
    };
    #[cfg(not(target_os = "linux"))]
    let help_menu = Submenu::with_items(handle, labels.help, true, &[])?;

    Menu::with_items(
        handle,
        &[
            &app_menu,
            &file_menu,
            &edit_menu,
            &composer_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ],
    )
}

pub(crate) fn handle_menu_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: tauri::menu::MenuEvent,
) {
    match event.id().as_ref() {
        "about" | "help_about" => {
            if let Some(window) = app.get_webview_window("about") {
                let _ = window.show();
                let _ = window.set_focus();
                return;
            }
            let labels = strings(locale_for_app(app));
            let _ = WebviewWindowBuilder::new(app, "about", WebviewUrl::App("index.html".into()))
                .title(format!("{} Pi", labels.about))
                .resizable(false)
                .inner_size(360.0, 240.0)
                .center()
                .build();
        }
        "check_for_updates" => {
            let _ = app.emit("updater-check", ());
        }
        "file_new_agent" => emit_menu_event(app, "menu-new-agent"),
        "file_new_worktree_agent" => emit_menu_event(app, "menu-new-worktree-agent"),
        "file_new_clone_agent" => emit_menu_event(app, "menu-new-clone-agent"),
        "file_add_workspace" => emit_menu_event(app, "menu-add-workspace"),
        "file_add_workspace_from_url" => emit_menu_event(app, "menu-add-workspace-from-url"),
        "file_open_settings" => emit_menu_event(app, "menu-open-settings"),
        "file_close_window" | "window_close" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.close();
            }
        }
        "file_quit" => {
            app.exit(0);
        }
        "view_fullscreen" => {
            if let Some(window) = app.get_webview_window("main") {
                let is_fullscreen = window.is_fullscreen().unwrap_or(false);
                let _ = window.set_fullscreen(!is_fullscreen);
            }
        }
        "view_toggle_projects_sidebar" => emit_menu_event(app, "menu-toggle-projects-sidebar"),
        "view_toggle_git_sidebar" => emit_menu_event(app, "menu-toggle-git-sidebar"),
        "view_toggle_debug_panel" => emit_menu_event(app, "menu-toggle-debug-panel"),
        "view_toggle_terminal" => emit_menu_event(app, "menu-toggle-terminal"),
        "view_next_agent" => emit_menu_event(app, "menu-next-agent"),
        "view_prev_agent" => emit_menu_event(app, "menu-prev-agent"),
        "view_next_workspace" => emit_menu_event(app, "menu-next-workspace"),
        "view_prev_workspace" => emit_menu_event(app, "menu-prev-workspace"),
        "composer_cycle_model" => emit_menu_event(app, "menu-composer-cycle-model"),
        "composer_cycle_reasoning" => emit_menu_event(app, "menu-composer-cycle-reasoning"),
        "window_minimize" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.minimize();
            }
        }
        "window_maximize" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
            }
        }
        _ => {}
    }
}

fn emit_menu_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit(event, ());
    } else {
        let _ = app.emit(event, ());
    }
}
