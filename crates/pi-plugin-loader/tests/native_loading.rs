use std::path::{Path, PathBuf};
use std::process::Command;

use pi_plugin_loader::{NativePluginLoader, NativePluginLoaderOptions};
use pi_plugin_sdk::{AgentHook, NativePluginKind};

#[test]
fn loads_all_three_native_plugin_kinds_and_constructs_fresh_instances() {
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
    assert_eq!(descriptors[0].kind, NativePluginKind::Agent);
    assert_eq!(descriptors[1].kind, NativePluginKind::Provider);
    assert_eq!(descriptors[2].kind, NativePluginKind::Session);
    let agent_plugin = plugins.agent_factories()[0].create().unwrap();
    assert_eq!(agent_plugin.id().as_str(), "native-fixture-agent");
    assert!(agent_plugin.hook_interests().contains(AgentHook::Input));
    assert!(!agent_plugin.hook_interests().contains(AgentHook::Context));
    assert_eq!(
        plugins.provider_factories()[0]
            .create()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-provider"
    );
    assert_eq!(
        plugins.session_factories()[0]
            .create()
            .unwrap()
            .id()
            .as_str(),
        "native-fixture-session"
    );
    assert_eq!(
        plugins.agent_factories()[0].create().unwrap().id().as_str(),
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
    assert_eq!(first_build.descriptors()[0].kind, NativePluginKind::Agent);

    std::fs::copy(
        dynamic_library(target.path(), "native_fixture_session"),
        &reload_artifact,
    )
    .unwrap();
    let second_build = NativePluginLoader::new(reload_options).discover().unwrap();
    assert_eq!(
        second_build.descriptors()[0].kind,
        NativePluginKind::Session
    );
    assert_eq!(
        second_build.session_factories()[0]
            .create()
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
