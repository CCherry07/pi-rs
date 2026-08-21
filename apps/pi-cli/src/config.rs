use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "pi", version, about = "Pi coding agent for the terminal")]
pub struct Cli {
    /// Initial prompt. With --print, runs once and exits.
    #[arg(value_name = "PROMPT", trailing_var_arg = true)]
    pub prompt: Vec<String>,

    /// Run one prompt and print only the final assistant text.
    #[arg(short = 'p', long)]
    pub print: bool,

    /// Emit newline-delimited product events.
    #[arg(long)]
    pub json: bool,

    /// Use the terminal alternate screen. This is the default.
    #[arg(long)]
    pub fullscreen: bool,

    /// Use main-screen mode instead of the default alternate screen.
    #[arg(long)]
    pub no_fullscreen: bool,

    #[arg(long, default_value = ".")]
    pub cwd: PathBuf,

    /// Open an exact JSONL session path; creates it when absent.
    #[arg(long)]
    pub session: Option<PathBuf>,

    /// Initial model override. When omitted, models.json owns catalog selection.
    #[arg(long)]
    pub model: Option<String>,

    #[arg(
        long,
        env = "OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    pub base_url: String,

    #[arg(long, env = "OPENAI_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,

    /// Provider override paired with --model. Defaults to openai-compatible
    /// only when the registered model catalog cannot select a model.
    #[arg(long)]
    pub provider: Option<String>,

    /// Root for global skills and sessions (default: PI_AGENT_DIR or ~/.pi/agent).
    #[arg(long, env = "PI_AGENT_DIR")]
    pub agent_dir: Option<PathBuf>,

    /// Load a native plugin from a dynamic library or pi-plugin.toml. May be repeated.
    #[arg(long = "plugin", value_name = "PATH")]
    pub native_plugins: Vec<PathBuf>,

    /// Trust project-local settings and resources without prompting.
    #[arg(short = 'a', long, conflicts_with = "no_approve")]
    pub approve: bool,

    /// Do not trust project-local settings or resources.
    #[arg(long, conflicts_with = "approve")]
    pub no_approve: bool,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_path: PathBuf,
    pub model: Option<String>,
    pub fallback_model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub provider: String,
    pub requested_provider: Option<String>,
    pub trust_project: bool,
    pub trust_override: Option<bool>,
    pub native_plugins: Vec<PathBuf>,
}

impl AppConfig {
    pub fn resolve(cli: &Cli) -> Result<Self, String> {
        let cwd = std::fs::canonicalize(&cli.cwd)
            .map_err(|error| format!("cannot access cwd {}: {error}", cli.cwd.display()))?;
        let agent_dir = cli
            .agent_dir
            .clone()
            .or_else(|| std::env::var_os("PI_AGENT_DIR").map(PathBuf::from))
            .or_else(default_agent_dir)
            .ok_or_else(|| "cannot determine agent directory; pass --agent-dir".to_string())?;
        let session_path = cli.session.clone().unwrap_or_else(|| {
            agent_dir
                .join("sessions")
                .join(format!("{}.jsonl", uuid::Uuid::now_v7()))
        });
        let api_key = cli.api_key.clone().filter(|key| !key.trim().is_empty());
        let requested_provider = cli
            .provider
            .clone()
            .filter(|provider| !provider.trim().is_empty());
        let fallback_model = std::env::var("OPENAI_MODEL")
            .ok()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());
        Ok(Self {
            cwd,
            agent_dir,
            session_path,
            model: cli.model.clone().filter(|model| !model.trim().is_empty()),
            fallback_model,
            base_url: cli.base_url.clone(),
            api_key,
            provider: requested_provider
                .clone()
                .unwrap_or_else(|| "openai-compatible".to_string()),
            requested_provider,
            trust_project: false,
            trust_override: cli
                .approve
                .then_some(true)
                .or(cli.no_approve.then_some(false)),
            native_plugins: cli.native_plugins.clone(),
        })
    }
}

impl Cli {
    pub fn parse_pi() -> Self {
        Self::parse_from(std::env::args_os().map(normalize_pi_arg))
    }

    pub fn fullscreen_enabled(&self) -> bool {
        self.fullscreen || !self.no_fullscreen
    }
}

fn normalize_pi_arg(argument: OsString) -> OsString {
    if argument == "-na" {
        OsString::from("--no-approve")
    } else {
        argument
    }
}

fn default_agent_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".pi").join("agent"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use clap::{CommandFactory, Parser};

    use super::{AppConfig, Cli, normalize_pi_arg};

    #[test]
    fn help_never_renders_the_api_key_value() {
        let help = Cli::command().render_long_help().to_string();
        if let Ok(secret) = std::env::var("OPENAI_API_KEY")
            && !secret.is_empty()
        {
            assert!(!help.contains(&secret));
        }
        assert!(help.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn fullscreen_is_default_and_can_be_disabled_explicitly() {
        assert!(Cli::try_parse_from(["pi"]).unwrap().fullscreen_enabled());
        assert!(
            Cli::try_parse_from(["pi", "--fullscreen"])
                .unwrap()
                .fullscreen_enabled()
        );
        assert!(
            !Cli::try_parse_from(["pi", "--no-fullscreen"])
                .unwrap()
                .fullscreen_enabled()
        );
    }

    #[test]
    fn resolving_a_new_session_does_not_create_its_directory() {
        let directory = tempfile::tempdir().unwrap();
        let session_path = directory.path().join("agent/sessions/new.jsonl");
        let cli = Cli {
            prompt: Vec::new(),
            print: false,
            json: false,
            fullscreen: false,
            no_fullscreen: false,
            cwd: directory.path().to_path_buf(),
            session: Some(session_path.clone()),
            model: None,
            base_url: "https://example.test/v1".to_string(),
            api_key: None,
            provider: None,
            agent_dir: Some(directory.path().join("agent")),
            native_plugins: Vec::new(),
            approve: false,
            no_approve: false,
        };

        AppConfig::resolve(&cli).unwrap();

        assert!(!session_path.parent().unwrap().exists());
    }

    #[test]
    fn explicit_trust_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["pi", "--approve", "--no-approve"]).is_err());
        assert!(Cli::try_parse_from(["pi", "-a"]).unwrap().approve);
        assert!(
            Cli::try_parse_from(["pi", "--no-approve"])
                .unwrap()
                .no_approve
        );
        assert_eq!(normalize_pi_arg(OsString::from("-na")), "--no-approve");
    }

    #[test]
    fn native_plugin_paths_preserve_cli_order() {
        let cli = Cli::try_parse_from([
            "pi",
            "--plugin",
            "first/pi-plugin.toml",
            "--plugin",
            "second/plugin.dylib",
        ])
        .unwrap();
        assert_eq!(
            cli.native_plugins,
            [
                PathBuf::from("first/pi-plugin.toml"),
                PathBuf::from("second/plugin.dylib")
            ]
        );
    }
}
