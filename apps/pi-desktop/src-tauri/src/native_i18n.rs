use std::sync::Mutex;

use tauri::{AppHandle, Manager, Runtime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeLocale {
    En,
    ZhCn,
}

impl NativeLocale {
    pub(crate) fn from_language_tag(value: &str) -> Self {
        let normalized = value.trim().replace('_', "-").to_ascii_lowercase();
        if normalized == "zh" || normalized.starts_with("zh-") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

#[derive(Default)]
pub(crate) struct NativeLocaleState {
    resolved_locale: Mutex<Option<NativeLocale>>,
}

impl NativeLocaleState {
    pub(crate) fn set(&self, locale: NativeLocale) {
        if let Ok(mut current) = self.resolved_locale.lock() {
            *current = Some(locale);
        }
    }

    pub(crate) fn resolve<R: Runtime>(&self, app: &AppHandle<R>) -> NativeLocale {
        if let Ok(current) = self.resolved_locale.lock() {
            if let Some(locale) = *current {
                return locale;
            }
        }
        resolve_initial_locale(app)
    }
}

fn resolve_initial_locale<R: Runtime>(app: &AppHandle<R>) -> NativeLocale {
    let preference = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|directory| std::fs::read_to_string(directory.join("settings.json")).ok())
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .and_then(|value| {
            value
                .get("uiLocale")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });

    match preference.as_deref() {
        Some(value) if !value.eq_ignore_ascii_case("system") => {
            NativeLocale::from_language_tag(value)
        }
        _ => resolve_system_locale(),
    }
}

fn resolve_system_locale() -> NativeLocale {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| NativeLocale::from_language_tag(&value))
        .unwrap_or(NativeLocale::En)
}

pub(crate) fn locale_for_app<R: Runtime>(app: &AppHandle<R>) -> NativeLocale {
    app.state::<NativeLocaleState>().resolve(app)
}

#[derive(Clone, Copy)]
pub(crate) struct NativeStrings {
    pub(crate) about: &'static str,
    pub(crate) check_updates: &'static str,
    pub(crate) settings: &'static str,
    pub(crate) services: &'static str,
    pub(crate) hide: &'static str,
    pub(crate) hide_others: &'static str,
    pub(crate) quit: &'static str,
    pub(crate) new_agent: &'static str,
    pub(crate) new_worktree_agent: &'static str,
    pub(crate) new_clone_agent: &'static str,
    pub(crate) add_workspaces: &'static str,
    pub(crate) add_workspace_from_url: &'static str,
    pub(crate) close_window: &'static str,
    pub(crate) file: &'static str,
    pub(crate) edit: &'static str,
    pub(crate) undo: &'static str,
    pub(crate) redo: &'static str,
    pub(crate) cut: &'static str,
    pub(crate) copy: &'static str,
    pub(crate) paste: &'static str,
    pub(crate) select_all: &'static str,
    pub(crate) composer: &'static str,
    pub(crate) cycle_model: &'static str,
    pub(crate) cycle_reasoning: &'static str,
    pub(crate) view: &'static str,
    pub(crate) toggle_projects_sidebar: &'static str,
    pub(crate) toggle_git_sidebar: &'static str,
    pub(crate) toggle_debug_panel: &'static str,
    pub(crate) toggle_terminal: &'static str,
    pub(crate) next_agent: &'static str,
    pub(crate) previous_agent: &'static str,
    pub(crate) next_workspace: &'static str,
    pub(crate) previous_workspace: &'static str,
    pub(crate) toggle_full_screen: &'static str,
    pub(crate) window: &'static str,
    pub(crate) minimize: &'static str,
    pub(crate) maximize: &'static str,
    pub(crate) help: &'static str,
    pub(crate) recent_threads: &'static str,
    pub(crate) no_recent_threads: &'static str,
    pub(crate) workspaces: &'static str,
    pub(crate) workspace: &'static str,
}

pub(crate) fn strings(locale: NativeLocale) -> NativeStrings {
    match locale {
        NativeLocale::En => NativeStrings {
            about: "About",
            check_updates: "Check for Updates...",
            settings: "Settings...",
            services: "Services",
            hide: "Hide",
            hide_others: "Hide Others",
            quit: "Quit",
            new_agent: "New Agent",
            new_worktree_agent: "New Worktree Agent",
            new_clone_agent: "New Clone Agent",
            add_workspaces: "Add Workspaces...",
            add_workspace_from_url: "Add Workspace from URL...",
            close_window: "Close Window",
            file: "File",
            edit: "Edit",
            undo: "Undo",
            redo: "Redo",
            cut: "Cut",
            copy: "Copy",
            paste: "Paste",
            select_all: "Select All",
            composer: "Composer",
            cycle_model: "Cycle Model",
            cycle_reasoning: "Cycle Reasoning Mode",
            view: "View",
            toggle_projects_sidebar: "Toggle Projects Sidebar",
            toggle_git_sidebar: "Toggle Git Sidebar",
            toggle_debug_panel: "Toggle Debug Panel",
            toggle_terminal: "Toggle Terminal",
            next_agent: "Next Agent",
            previous_agent: "Previous Agent",
            next_workspace: "Next Workspace",
            previous_workspace: "Previous Workspace",
            toggle_full_screen: "Toggle Full Screen",
            window: "Window",
            minimize: "Minimize",
            maximize: "Maximize",
            help: "Help",
            recent_threads: "Recent Threads",
            no_recent_threads: "No recent threads",
            workspaces: "Workspaces",
            workspace: "Workspace",
        },
        NativeLocale::ZhCn => NativeStrings {
            about: "关于",
            check_updates: "检查更新...",
            settings: "设置...",
            services: "服务",
            hide: "隐藏",
            hide_others: "隐藏其他窗口",
            quit: "退出",
            new_agent: "新建智能体",
            new_worktree_agent: "新建工作树智能体",
            new_clone_agent: "新建克隆智能体",
            add_workspaces: "添加工作区...",
            add_workspace_from_url: "从 URL 添加工作区...",
            close_window: "关闭窗口",
            file: "文件",
            edit: "编辑",
            undo: "撤销",
            redo: "重做",
            cut: "剪切",
            copy: "复制",
            paste: "粘贴",
            select_all: "全选",
            composer: "输入区",
            cycle_model: "切换模型",
            cycle_reasoning: "切换推理模式",
            view: "视图",
            toggle_projects_sidebar: "切换项目侧边栏",
            toggle_git_sidebar: "切换 Git 侧边栏",
            toggle_debug_panel: "切换调试面板",
            toggle_terminal: "切换终端",
            next_agent: "下一个智能体",
            previous_agent: "上一个智能体",
            next_workspace: "下一个工作区",
            previous_workspace: "上一个工作区",
            toggle_full_screen: "切换全屏",
            window: "窗口",
            minimize: "最小化",
            maximize: "最大化",
            help: "帮助",
            recent_threads: "最近的任务",
            no_recent_threads: "没有最近的任务",
            workspaces: "工作区",
            workspace: "工作区",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{strings, NativeLocale};

    #[test]
    fn normalizes_chinese_language_tags() {
        assert_eq!(NativeLocale::from_language_tag("zh-CN"), NativeLocale::ZhCn);
        assert_eq!(
            NativeLocale::from_language_tag("zh_CN.UTF-8"),
            NativeLocale::ZhCn
        );
        assert_eq!(NativeLocale::from_language_tag("en-US"), NativeLocale::En);
    }

    #[test]
    fn exposes_localized_native_labels() {
        assert_eq!(strings(NativeLocale::En).recent_threads, "Recent Threads");
        assert_eq!(strings(NativeLocale::ZhCn).recent_threads, "最近的任务");
    }
}
