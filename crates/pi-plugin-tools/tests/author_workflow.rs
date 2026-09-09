use std::path::Path;
use std::process::Command;

use pi_plugin_loader::{NativePluginLoader, NativePluginLoaderOptions};
use pi_plugin_manager::{InstallScope, PluginManager, PluginManagerOptions};
use pi_plugin_tools::{
    HOST_TARGET, NewOptions, PackageOptions, PluginKind, SdkSource, new_plugin, package, verify,
};

/// Exercise the author's public interface through the existing installer and
/// loader, using real cdylibs for every supported lifecycle.
#[tokio::test]
async fn scaffold_build_verify_install_and_construct_all_plugin_kinds() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let cargo_config = root.path().join(".cargo");
    std::fs::create_dir(&cargo_config).unwrap();
    // Share one nested Cargo cache across the three independently generated crates.
    let cache = workspace.join("target/plugin-tools-fixtures");
    let config = toml::Table::from_iter([(
        "build".into(),
        toml::Value::Table(toml::Table::from_iter([(
            "target-dir".into(),
            cache.to_string_lossy().into_owned().into(),
        )])),
    )]);
    std::fs::write(
        cargo_config.join("config.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    for kind in [PluginKind::Agent, PluginKind::Provider, PluginKind::Session] {
        let name = format!("author-fixture-{}", kind.as_str());
        let project = root.path().join(&name);
        new_plugin(&NewOptions {
            destination: project.clone(),
            name: name.clone(),
            kind,
            sdk: SdkSource::Path(workspace.clone()),
        })
        .unwrap();
        let bundle = package(&PackageOptions {
            manifest_path: project.join("Cargo.toml"),
            output: project.join("dist"),
            target_dir: Some(cache.clone()),
            locked: false,
            debug: true,
        })
        .unwrap();
        let report = verify(&bundle, false).unwrap();
        assert_eq!(report.release.id, name);
        assert_eq!(report.release.kind, kind.as_str());
        assert_eq!(report.host_verified.as_deref(), Some(HOST_TARGET));
        assert!(
            project
                .join(".github/workflows/release.yml.example")
                .is_file()
        );
        assert!(!project.join(".pi").exists());

        let output = Command::new(env!("CARGO"))
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &cache)
            .args(["test", "--quiet", "--locked", "--target", HOST_TARGET])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let agent = root.path().join(format!("installed-{}", kind.as_str()));
        let manager = PluginManager::new(PluginManagerOptions::new(&project, &agent)).unwrap();
        let installed = manager
            .install(bundle.to_string_lossy(), None, InstallScope::Global)
            .await
            .unwrap();
        assert_eq!(installed.id, name);
        let plugins = NativePluginLoader::new(NativePluginLoaderOptions::new(&project, &agent))
            .discover()
            .unwrap();
        assert_eq!(plugins.descriptors().len(), 1);
        let actual = match kind {
            PluginKind::Agent => plugins.agent_factories()[0]
                .create()
                .unwrap()
                .id()
                .as_str()
                .to_string(),
            PluginKind::Provider => plugins.provider_factories()[0]
                .create()
                .unwrap()
                .id()
                .as_str()
                .to_string(),
            PluginKind::Session => plugins.session_factories()[0]
                .create()
                .unwrap()
                .id()
                .as_str()
                .to_string(),
        };
        assert_eq!(actual, name);
        // A completed lock also supports a second reproducible package build.
        let rebuilt = package(&PackageOptions {
            manifest_path: project.join("Cargo.toml"),
            output: project.join("dist-locked"),
            target_dir: Some(cache.clone()),
            locked: true,
            debug: true,
        })
        .unwrap();
        assert_eq!(verify(&rebuilt, true).unwrap().release, report.release);
        // Matching JSON/TOML declarations cannot disguise a different binary ID.
        let manifest_path = rebuilt.join(pi_plugin_tools::RELEASE_FILE);
        let mut remote: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        remote["id"] = serde_json::json!("different-plugin");
        std::fs::write(&manifest_path, serde_json::to_vec(&remote).unwrap()).unwrap();
        let local_path = rebuilt.join("pi-plugin.toml");
        let mut local: toml::Value =
            toml::from_str(&std::fs::read_to_string(&local_path).unwrap()).unwrap();
        local["plugin"]["id"] = toml::Value::String("different-plugin".into());
        std::fs::write(&local_path, toml::to_string(&local).unwrap()).unwrap();
        verify(&rebuilt, true).unwrap();
        assert!(
            verify(&rebuilt, false)
                .unwrap_err()
                .to_string()
                .contains("manifest")
        );

        std::fs::write(
            project.join("src/lib.rs"),
            "compile_error!(\"author build failure\");",
        )
        .unwrap();
        let error = package(&PackageOptions {
            manifest_path: project.join("Cargo.toml"),
            output: project.join("failed-build"),
            target_dir: Some(cache.clone()),
            locked: true,
            debug: true,
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("author build failure"),
            "{error}"
        );
        assert!(!project.join("failed-build").exists());
        assert_eq!(verify(&bundle, true).unwrap().release, report.release);
    }
}
