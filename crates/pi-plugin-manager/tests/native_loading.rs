use std::path::{Path, PathBuf};
use std::process::Command;

use pi_plugin::AgentHook;
use pi_plugin::native::NativePluginKind;
use pi_plugin_manager::loader::{NativePluginLoader, NativePluginLoaderOptions};

#[tokio::test]
async fn loads_both_native_kinds_including_session_hooks_and_constructs_fresh_instances() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native-plugins");
    let target = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO"))
        .args(["build", "--workspace", "--quiet"])
        .arg("--manifest-path")
        .arg(fixture.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.path())
        .status()
        .unwrap();
    assert!(status.success());

    let agent_dir = target.path().join("agent-dir");
    let provider_package = agent_dir.join("plugins/native-fixture-provider/0.1.0");
    std::fs::create_dir_all(&provider_package).unwrap();
    std::fs::copy(
        dynamic_library(target.path(), "native_fixture_provider"),
        provider_package.join(dynamic_library_name("native_fixture_provider")),
    )
    .unwrap();
    std::fs::write(
        provider_package.join("pi-plugin.toml"),
        format!(
            r#"schema = 1
[plugin]
id = "native-fixture-provider"
version = "0.1.0"
kind = "provider"
artifact = "{}"

[options]
marker = "from-manifest"
"#,
            dynamic_library_name("native_fixture_provider")
        ),
    )
    .unwrap();

    let mut options = NativePluginLoaderOptions::new(&fixture, &agent_dir);
    options.explicit_paths = [
        dynamic_library(target.path(), "native_fixture_agent"),
        dynamic_library(target.path(), "native_fixture_session"),
    ]
    .into_iter()
    .collect();
    let plugins = NativePluginLoader::new(options).discover().unwrap();

    let descriptors = plugins.descriptors();
    assert_eq!(descriptors.len(), 3);
    assert_eq!(descriptors[0].kind, NativePluginKind::Plugin);
    assert_eq!(descriptors[1].kind, NativePluginKind::Plugin);
    assert_eq!(descriptors[2].kind, NativePluginKind::Provider);
    let plugin = plugins.plugin_factories()[0].create().unwrap().unwrap();
    assert_eq!(plugin.id().as_str(), "native-fixture-agent");
    assert!(plugin.hook_interests().contains(AgentHook::Input));
    assert!(!plugin.hook_interests().contains(AgentHook::Context));
    // Both families dispatch through the same native object and retain its state.
    use pi_plugin::{
        AgentPluginContext, BeforeAgentStartEvent, SessionIdentity, SessionPluginContext,
        SessionStartEvent, SessionStartReason,
    };
    plugin
        .session_start(
            &SessionPluginContext::unavailable_for_testing(
                plugin.id(),
                1,
                SessionIdentity {
                    id: "native-session".into(),
                    path: target.path().join("session.jsonl"),
                    cwd: target.path().to_path_buf(),
                    parent_session_id: None,
                },
            ),
            &SessionStartEvent {
                reason: SessionStartReason::Startup,
                previous_session_file: None,
            },
        )
        .await
        .unwrap();
    let (_, signal) = pi_core::AbortHandle::new();
    let patch = plugin
        .before_agent_start(
            AgentPluginContext::unavailable_for_testing(
                plugin.id(),
                "native-run".into(),
                target.path().to_path_buf(),
                signal,
            ),
            BeforeAgentStartEvent {
                system_prompt: String::new(),
                input_messages: Vec::new(),
                active_tools: Vec::new(),
                provider_id: "test".into(),
                model_id: "test".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(patch.system_prompt.as_deref(), Some("started=true"));
    assert_eq!(
        plugins.provider_factories()[0]
            .create()
            .unwrap()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-provider"
    );
    assert_eq!(
        plugins.plugin_factories()[1]
            .create()
            .unwrap()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-session"
    );
    assert_eq!(
        plugins.plugin_factories()[0]
            .create()
            .unwrap()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-agent"
    );

    let reload_dir = target.path().join("reload-source");
    std::fs::create_dir_all(&reload_dir).unwrap();
    let reload_artifact = reload_dir.join(dynamic_library_name("mutable_fixture"));
    std::fs::copy(
        dynamic_library(target.path(), "native_fixture_agent"),
        &reload_artifact,
    )
    .unwrap();
    let reload_agent_dir = target.path().join("reload-agent-dir");
    let mut reload_options = NativePluginLoaderOptions::new(&fixture, &reload_agent_dir);
    reload_options.explicit_paths.push(reload_artifact.clone());
    let first_build = NativePluginLoader::new(reload_options.clone())
        .discover()
        .unwrap();
    assert_eq!(first_build.descriptors()[0].kind, NativePluginKind::Plugin);

    std::fs::copy(
        dynamic_library(target.path(), "native_fixture_session"),
        &reload_artifact,
    )
    .unwrap();
    let second_build = NativePluginLoader::new(reload_options).discover().unwrap();
    assert_eq!(second_build.descriptors()[0].kind, NativePluginKind::Plugin);
    assert_eq!(
        second_build.plugin_factories()[0]
            .create()
            .unwrap()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-session"
    );
}

fn dynamic_library(target: &Path, crate_name: &str) -> PathBuf {
    target.join("debug").join(dynamic_library_name(crate_name))
}

fn dynamic_library_name(crate_name: &str) -> String {
    format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        crate_name,
        std::env::consts::DLL_SUFFIX
    )
}
