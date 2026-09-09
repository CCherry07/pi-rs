use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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

    /// Output/control mode compatible with Pi: text, json, or rpc.
    #[arg(long, value_enum)]
    pub(crate) mode: Option<OutputMode>,

    /// Serve Agent Client Protocol stable v1 over stdin/stdout.
    #[arg(
        long,
        conflicts_with_all = ["print", "json", "mode", "session", "session_id", "prompt"]
    )]
    pub(crate) acp: bool,

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

    /// Use an exact project session ID, creating it when missing.
    #[arg(long, conflicts_with = "session")]
    pub(crate) session_id: Option<String>,

    /// Set the session display name.
    #[arg(short = 'n', long)]
    pub(crate) name: Option<String>,

    /// Initial model override. When omitted, models.json owns catalog selection.
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// Initial reasoning level override.
    #[arg(long, value_enum)]
    pub(crate) thinking: Option<ThinkingLevelArg>,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputMode {
    Text,
    Json,
    Rpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ThinkingLevelArg {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<ThinkingLevelArg> for pi_core::ThinkingLevel {
    fn from(value: ThinkingLevelArg) -> Self {
        match value {
            ThinkingLevelArg::Off => Self::Off,
            ThinkingLevelArg::Minimal => Self::Minimal,
            ThinkingLevelArg::Low => Self::Low,
            ThinkingLevelArg::Medium => Self::Medium,
            ThinkingLevelArg::High => Self::High,
            ThinkingLevelArg::Xhigh => Self::XHigh,
            ThinkingLevelArg::Max => Self::Max,
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum CliCommand {
    /// Maintain the Hermes-managed skill library.
    Curator {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Create, package, publish, install and manage native plugins.
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
    /// Create a native plugin crate and release workflow.
    New {
        path: PathBuf,
        /// Plugin/crate name (defaults to the destination directory name).
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "agent")]
        kind: pi_plugin_tools::PluginKind,
        /// Local pi-rs checkout or pi-plugin-sdk crate.
        #[arg(long, conflicts_with = "sdk_rev")]
        sdk: Option<PathBuf>,
        /// Exact SDK commit; defaults to the commit used to build this host.
        #[arg(long)]
        sdk_rev: Option<String>,
    },
    /// Build and verify a native plugin for this host's target.
    Package {
        #[arg(long, default_value = "Cargo.toml")]
        manifest_path: PathBuf,
        #[arg(short, long, default_value = "dist")]
        output: PathBuf,
        /// Cargo compilation cache, separate from the release output.
        #[arg(long)]
        target_dir: Option<PathBuf>,
        /// Require the existing Cargo.lock without updating dependency resolution.
        #[arg(long)]
        locked: bool,
        /// Build with the dev profile (release is the default).
        #[arg(long)]
        debug: bool,
    },
    /// Verify all checksums and this host's native binary compatibility.
    Verify {
        #[arg(default_value = "dist")]
        path: PathBuf,
        /// Only check bundle structure and checksums; do not load native code.
        #[arg(long)]
        integrity_only: bool,
    },
    /// Merge release bundles produced on separate native runners.
    Merge {
        #[arg(required = true, num_args = 1..)]
        bundles: Vec<PathBuf>,
        #[arg(short, long, default_value = "release")]
        output: PathBuf,
    },
    /// Publish a verified release through an authenticated GitHub CLI.
    Publish {
        #[command(subcommand)]
        destination: PluginPublishCommand,
    },
    /// Print a static registry JSON fragment for a verified local bundle.
    RegistryEntry {
        #[arg(default_value = "dist")]
        bundle: PathBuf,
        #[arg(long)]
        manifest_url: String,
    },
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

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum PluginPublishCommand {
    Github {
        #[arg(long, default_value = "dist")]
        bundle: PathBuf,
        #[arg(long)]
        repo: String,
        /// Existing vX.Y.Z tag matching the plugin version.
        #[arg(long)]
        tag: String,
        /// Leave the release as a draft after uploading.
        #[arg(long)]
        draft: bool,
    },
}

pub(crate) type AppConfig = pi_sdk::ProductConfig;

pub(crate) fn resolve_app_config(cli: &Cli) -> Result<AppConfig, String> {
    let cwd = std::fs::canonicalize(&cli.cwd)
        .map_err(|error| format!("cannot access cwd {}: {error}", cli.cwd.display()))?;
    let agent_dir = cli
        .agent_dir
        .clone()
        .or_else(pi_sdk::default_agent_dir)
        .ok_or_else(|| "cannot determine agent directory; pass --agent-dir".to_string())?;
    let mut config = AppConfig::new(cwd, agent_dir);
    if let Some(session_path) = &cli.session {
        config.session_path.clone_from(session_path);
    }
    config.model = cli.model.clone().filter(|model| !model.trim().is_empty());
    config.thinking = cli.thinking.map(Into::into);
    config.base_url.clone_from(&cli.base_url);
    config.requested_provider = cli
        .provider
        .clone()
        .filter(|provider| !provider.trim().is_empty());
    config.provider = config
        .requested_provider
        .clone()
        .unwrap_or_else(|| "openai-compatible".to_string());
    config.api_key = cli
        .api_key
        .clone()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| {
            (config.provider == "openai-compatible")
                .then(|| std::env::var("OPENAI_API_KEY").ok())
                .flatten()
                .filter(|key| !key.trim().is_empty())
        });
    config.trust_override = cli
        .approve
        .then_some(true)
        .or(cli.no_approve.then_some(false));
    config.native_plugins.clone_from(&cli.native_plugins);
    config.extensions.clone_from(&cli.extensions);
    config.discover_extensions = !cli.no_extensions;
    Ok(config)
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use clap::{CommandFactory, Parser};

    use super::{
        AuthCommand, Cli, CliCommand, PluginCommand, normalize_pi_arg, resolve_app_config,
    };

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
    fn curator_preserves_maintenance_flags_as_subcommand_arguments() {
        let scoped =
            Cli::try_parse_from(["pi", "curator", "run", "--scope", "project", "--dry-run"])
                .unwrap();
        assert!(
            matches!(scoped.command, Some(CliCommand::Curator { arguments }) if arguments == ["run", "--scope", "project", "--dry-run"])
        );
        let cli =
            Cli::try_parse_from(["pi", "curator", "run", "--dry-run", "--consolidate"]).unwrap();
        assert!(
            matches!(cli.command, Some(CliCommand::Curator { arguments })
            if arguments == ["run", "--dry-run", "--consolidate"])
        );
        let cli =
            Cli::try_parse_from(["pi", "curator", "rollback", "--id", "snapshot-123"]).unwrap();
        assert!(
            matches!(cli.command, Some(CliCommand::Curator { arguments })
            if arguments == ["rollback", "--id", "snapshot-123"])
        );
    }

    #[test]
    fn acp_is_a_dedicated_stdio_mode() {
        assert!(Cli::try_parse_from(["pi", "--acp"]).unwrap().acp);
        assert!(Cli::try_parse_from(["pi", "--acp", "--print", "hello"]).is_err());
        assert!(Cli::try_parse_from(["pi", "--acp", "--mode", "rpc"]).is_err());
        assert!(Cli::try_parse_from(["pi", "--acp", "--session", "old.jsonl"]).is_err());
        assert!(Cli::try_parse_from(["pi", "--acp", "hello"]).is_err());
    }

    #[test]
    fn botmux_session_identity_and_name_flags_match_pi() {
        let parsed = Cli::try_parse_from([
            "pi",
            "--session-id",
            "018f4f7c-example",
            "--name",
            "BotMux task",
        ])
        .unwrap();
        assert_eq!(parsed.session_id.as_deref(), Some("018f4f7c-example"));
        assert_eq!(parsed.name.as_deref(), Some("BotMux task"));
        assert_eq!(
            Cli::try_parse_from(["pi", "-n", "short"])
                .unwrap()
                .name
                .as_deref(),
            Some("short")
        );
        assert!(
            Cli::try_parse_from(["pi", "--session", "old.jsonl", "--session-id", "new-id"])
                .is_err()
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
            mode: None,
            acp: false,
            fullscreen: false,
            no_fullscreen: false,
            cwd: directory.path().to_path_buf(),
            session: Some(session_path.clone()),
            session_id: None,
            name: None,
            model: None,
            thinking: None,
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

        resolve_app_config(&cli).unwrap();

        assert!(!session_path.parent().unwrap().exists());
    }

    #[test]
    fn session_dir_setting_rehomes_only_implicit_new_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "pi",
            "--cwd",
            directory.path().to_str().unwrap(),
            "--agent-dir",
            directory.path().join("agent").to_str().unwrap(),
        ])
        .unwrap();
        let mut config = resolve_app_config(&cli).unwrap();
        let original_name = config.session_path.file_name().unwrap().to_owned();

        config.apply_session_dir_setting(Some("custom-sessions"), false);

        assert_eq!(
            config.session_path,
            PathBuf::from("custom-sessions").join(original_name)
        );

        let explicit = directory.path().join("explicit.jsonl");
        config.session_path = explicit.clone();
        config.apply_session_dir_setting(Some("ignored"), true);
        assert_eq!(config.session_path, explicit);
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
    fn native_author_commands_parse_without_becoming_prompts() {
        let new = Cli::try_parse_from([
            "pi", "plugin", "new", "hello", "--kind", "provider", "--sdk", "/sdk",
        ])
        .unwrap();
        assert!(matches!(
            new.command,
            Some(CliCommand::Plugin {
                command: PluginCommand::New {
                    kind: pi_plugin_tools::PluginKind::Provider,
                    ..
                }
            })
        ));
        assert!(new.prompt.is_empty());
        for args in [
            vec![
                "pi",
                "plugin",
                "package",
                "--locked",
                "--output",
                "dist-next",
            ],
            vec!["pi", "plugin", "verify", "dist", "--integrity-only"],
            vec![
                "pi", "plugin", "merge", "mac", "linux", "--output", "release",
            ],
            vec![
                "pi",
                "plugin",
                "publish",
                "github",
                "--repo",
                "owner/repo",
                "--tag",
                "v1.0.0",
                "--draft",
            ],
            vec![
                "pi",
                "plugin",
                "registry-entry",
                "dist",
                "--manifest-url",
                "https://example.com/release.json",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Some(CliCommand::Plugin { .. })));
            assert!(cli.prompt.is_empty());
        }
        assert!(
            Cli::try_parse_from(["pi", "plugin", "new", "hello", "--kind", "unknown"]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "pi",
                "plugin",
                "new",
                "hello",
                "--sdk",
                "/sdk",
                "--sdk-rev",
                "abc"
            ])
            .is_err()
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
