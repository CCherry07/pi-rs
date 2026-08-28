use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pi_js_package_manager::ResolveRequest as JsResolveRequest;

#[derive(Debug, Parser)]
#[command(name = "pi", version, about = "Pi coding agent for the terminal")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<CliCommand>,

    /// Initial prompt. With --print, runs once and exits.
    #[arg(value_name = "PROMPT", trailing_var_arg = true)]
    pub(crate) prompt: Vec<String>,

    /// Run one prompt and print only the final assistant text.
    #[arg(short = 'p', long)]
    pub(crate) print: bool,

    /// Emit newline-delimited product events.
    #[arg(long)]
    pub(crate) json: bool,

    /// Use the terminal alternate screen. This is the default.
    #[arg(long)]
    pub(crate) fullscreen: bool,

    /// Use main-screen mode instead of the default alternate screen.
    #[arg(long)]
    pub(crate) no_fullscreen: bool,

    #[arg(long, default_value = ".", global = true)]
    pub(crate) cwd: PathBuf,

    /// Open an exact JSONL session path; creates it when absent.
    #[arg(long)]
    pub(crate) session: Option<PathBuf>,

    /// Initial model override. When omitted, models.json owns catalog selection.
    #[arg(long)]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        env = "OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    pub(crate) base_url: String,

    /// API key override. The default OpenAI-compatible provider also reads OPENAI_API_KEY.
    #[arg(long, hide_env_values = true)]
    pub(crate) api_key: Option<String>,

    /// Provider override paired with --model. Defaults to openai-compatible
    /// only when the registered model catalog cannot select a model.
    #[arg(long)]
    pub(crate) provider: Option<String>,

    /// Root for global skills and sessions (default: PI_AGENT_DIR or ~/.pi/agent).
    #[arg(long, env = "PI_AGENT_DIR", global = true)]
    pub(crate) agent_dir: Option<PathBuf>,

    /// Load a native plugin from a dynamic library or pi-plugin.toml. May be repeated.
    #[arg(long = "plugin", value_name = "PATH")]
    pub(crate) native_plugins: Vec<PathBuf>,

    /// Load a local, npm, or git JavaScript/TypeScript extension source. May be repeated.
    #[arg(short = 'e', long = "extension", value_name = "SOURCE")]
    pub(crate) extensions: Vec<String>,

    /// Disable automatic JavaScript/TypeScript extension discovery.
    #[arg(long)]
    pub(crate) no_extensions: bool,

    /// Trust project-local settings and resources without prompting.
    #[arg(short = 'a', long, conflicts_with = "no_approve", global = true)]
    pub(crate) approve: bool,

    /// Do not trust project-local settings or resources.
    #[arg(long, conflicts_with = "approve", global = true)]
    pub(crate) no_approve: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum CliCommand {
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Install and manage native plugins.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Install and configure a JavaScript extension package.
    Install {
        source: String,
        /// Store the package in the current project's .pi directory.
        #[arg(short = 'l', long = "local")]
        local: bool,
    },
    /// Remove a configured JavaScript extension package.
    #[command(alias = "uninstall")]
    Remove {
        source: String,
        /// Remove the package from the current project's .pi directory.
        #[arg(short = 'l', long = "local")]
        local: bool,
    },
    /// List configured JavaScript extension packages.
    List,
    /// Update configured JavaScript extension packages.
    Update {
        /// Update the configured package matching this source.
        source: Option<String>,
        /// Update every configured extension package.
        #[arg(long)]
        extensions: bool,
        /// Update one configured extension package.
        #[arg(long = "extension", value_name = "SOURCE")]
        extension: Option<String>,
        /// Request a pi-rs self-update (not yet implemented).
        #[arg(long = "self")]
        self_update: bool,
        /// Request a model catalog refresh (not handled by the JS package manager).
        #[arg(long)]
        models: bool,
        /// Request all update targets (not yet implemented).
        #[arg(long)]
        all: bool,
        /// Force a self-update check. Has no effect on extension updates.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum AuthCommand {
    /// Store an API key or an existing OAuth access token.
    Login {
        /// Provider ID. Omit it to select from built-in and models.json providers.
        provider: Option<String>,
        /// Store an API key. When omitted, the secret is prompted without echo.
        #[arg(long, conflicts_with = "oauth_token")]
        api_key: bool,
        /// Run the provider's browser/device OAuth flow.
        #[arg(long, conflicts_with_all = ["api_key", "oauth_token", "token"])]
        oauth: bool,
        /// Store an existing OAuth access token. Currently supported by anthropic.
        #[arg(long, conflicts_with_all = ["api_key", "oauth"])]
        oauth_token: bool,
        /// Secret value. Prefer the hidden prompt to avoid shell history and process listings.
        #[arg(long, hide = true)]
        token: Option<String>,
        /// OAuth refresh token, retained for future automatic refresh support.
        #[arg(long, hide = true, requires = "oauth_token")]
        refresh_token: Option<String>,
        /// OAuth expiry as Unix epoch milliseconds.
        #[arg(long, requires = "oauth_token")]
        expires: Option<f64>,
    },
    /// Remove a stored provider credential.
    Logout { provider: String },
    /// Show configured credential types without printing secrets.
    Status { provider: Option<String> },
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum PluginCommand {
    /// Resolve, verify, and install a native plugin package.
    Install {
        /// Local package path, release manifest URL, GitHub release, or registry source.
        source: String,
        /// Install into the current project's .pi directory.
        #[arg(short = 'l', long = "local")]
        local: bool,
        /// Semver requirement applied to the requested plugin.
        #[arg(long)]
        version: Option<String>,
        /// Static registry index URL; also read from PI_PLUGIN_REGISTRY.
        #[arg(long, env = "PI_PLUGIN_REGISTRY")]
        registry: Option<String>,
    },
    /// List plugins recorded in plugins.lock.
    List {
        /// List the current project's plugins instead of global plugins.
        #[arg(short = 'l', long = "local")]
        local: bool,
    },
    /// Reconcile plugins.json into plugins.lock and the local package store.
    Sync {
        /// Reconcile the current project's plugins instead of global plugins.
        #[arg(short = 'l', long = "local")]
        local: bool,
        /// Static registry index URL; also read from PI_PLUGIN_REGISTRY.
        #[arg(long, env = "PI_PLUGIN_REGISTRY")]
        registry: Option<String>,
    },
    /// Remove an installed plugin.
    Remove {
        id: String,
        /// Remove from the current project's .pi directory.
        #[arg(short = 'l', long = "local")]
        local: bool,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) cwd: PathBuf,
    pub(crate) agent_dir: PathBuf,
    pub(crate) session_path: PathBuf,
    pub(crate) model: Option<String>,
    pub(crate) fallback_model: String,
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) provider: String,
    pub(crate) requested_provider: Option<String>,
    pub(crate) trust_override: Option<bool>,
    pub(crate) native_plugins: Vec<PathBuf>,
    pub(crate) extensions: Vec<String>,
    pub(crate) discover_extensions: bool,
    pub(crate) extension_flag_values: BTreeMap<String, serde_json::Value>,
}

impl AppConfig {
    pub(crate) fn resolve(cli: &Cli) -> Result<Self, String> {
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
        let requested_provider = cli
            .provider
            .clone()
            .filter(|provider| !provider.trim().is_empty());
        let provider = requested_provider
            .clone()
            .unwrap_or_else(|| "openai-compatible".to_string());
        let api_key = cli
            .api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .or_else(|| {
                (provider == "openai-compatible")
                    .then(|| std::env::var("OPENAI_API_KEY").ok())
                    .flatten()
                    .filter(|key| !key.trim().is_empty())
            });
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
            provider,
            requested_provider,
            trust_override: cli
                .approve
                .then_some(true)
                .or(cli.no_approve.then_some(false)),
            native_plugins: cli.native_plugins.clone(),
            extensions: cli.extensions.clone(),
            discover_extensions: !cli.no_extensions,
            extension_flag_values: BTreeMap::new(),
        })
    }

    pub(crate) fn javascript_resolve_request(&self, project_trusted: bool) -> JsResolveRequest {
        JsResolveRequest {
            cwd: self.cwd.clone(),
            agent_dir: self.agent_dir.clone(),
            project_trusted,
            explicit_sources: self.extensions.clone(),
            discover_extensions: self.discover_extensions,
        }
    }
}

impl Cli {
    pub(crate) fn parse_pi() -> Self {
        Self::parse_from(std::env::args_os().map(normalize_pi_arg))
    }

    pub(crate) fn try_parse_pi_from(arguments: Vec<String>) -> Result<Self, clap::Error> {
        Self::try_parse_from(
            std::iter::once(OsString::from("pi"))
                .chain(arguments.into_iter().map(OsString::from))
                .map(normalize_pi_arg),
        )
    }

    pub(crate) fn fullscreen_enabled(&self) -> bool {
        self.fullscreen || !self.no_fullscreen
    }
}

fn normalize_pi_arg(argument: OsString) -> OsString {
    if argument == "-na" {
        OsString::from("--no-approve")
    } else if argument == "-ne" {
        OsString::from("--no-extensions")
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

    use super::{AppConfig, AuthCommand, Cli, CliCommand, PluginCommand, normalize_pi_arg};

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
            command: None,
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
            extensions: Vec::new(),
            no_extensions: false,
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

    #[test]
    fn javascript_extension_paths_and_discovery_flags_match_pi_cli_shape() {
        let cli = Cli::try_parse_from([
            "pi",
            "-e",
            "first.ts",
            "--extension",
            "npm:example-extension@1.0.0",
            "--no-extensions",
        ])
        .unwrap();

        assert_eq!(
            cli.extensions,
            [
                "first.ts".to_string(),
                "npm:example-extension@1.0.0".to_string()
            ]
        );
        assert!(cli.no_extensions);
        assert_eq!(normalize_pi_arg(OsString::from("-ne")), "--no-extensions");
    }

    #[test]
    fn auth_commands_parse_without_becoming_prompts() {
        let login = Cli::try_parse_from([
            "pi",
            "auth",
            "login",
            "anthropic",
            "--oauth-token",
            "--refresh-token",
            "refresh",
            "--expires",
            "123",
        ])
        .unwrap();
        assert!(matches!(
            login.command,
            Some(CliCommand::Auth {
                command: AuthCommand::Login {
                    ref provider,
                    oauth: false,
                    oauth_token: true,
                    ref refresh_token,
                    expires: Some(123.0),
                    ..
                }
            }) if provider.as_deref() == Some("anthropic") && refresh_token.as_deref() == Some("refresh")
        ));
        assert!(login.prompt.is_empty());

        let logout = Cli::try_parse_from(["pi", "auth", "logout", "xai"]).unwrap();
        assert!(matches!(
            logout.command,
            Some(CliCommand::Auth {
                command: AuthCommand::Logout { ref provider }
            }) if provider == "xai"
        ));
    }

    #[test]
    fn native_plugin_package_commands_parse_without_becoming_prompts() {
        let cli = Cli::try_parse_from([
            "pi",
            "plugin",
            "install",
            "registry:frontend-check@^1",
            "--registry",
            "https://plugins.example/index.json",
            "--local",
            "--approve",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Plugin {
                command: PluginCommand::Install {
                    local: true,
                    registry: Some(ref registry),
                    ..
                }
            }) if registry == "https://plugins.example/index.json"
        ));
        assert!(cli.approve);
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn javascript_package_commands_match_pi_cli_shape() {
        let install =
            Cli::try_parse_from(["pi", "install", "npm:example@^1", "--local", "--approve"])
                .unwrap();
        assert!(matches!(
            install.command,
            Some(CliCommand::Install {
                ref source,
                local: true
            }) if source == "npm:example@^1"
        ));
        assert!(install.prompt.is_empty());

        let remove = Cli::try_parse_from(["pi", "uninstall", "npm:example"]).unwrap();
        assert!(matches!(remove.command, Some(CliCommand::Remove { .. })));

        let update = Cli::try_parse_from([
            "pi",
            "update",
            "--extension",
            "git:github.com/example/pi-extension",
        ])
        .unwrap();
        assert!(matches!(
            update.command,
            Some(CliCommand::Update {
                extension: Some(ref source),
                ..
            }) if source == "git:github.com/example/pi-extension"
        ));

        let list = Cli::try_parse_from(["pi", "list", "--no-approve"]).unwrap();
        assert!(matches!(list.command, Some(CliCommand::List)));
    }
}
