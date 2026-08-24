use std::fs;
use std::path::Path;
use std::process::Command;

use pi_js_package_manager::{PackageManager, ResolveRequest};
use serde_json::json;

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "export default function () {}\n").unwrap();
}

fn write_json(path: &Path, value: serde_json::Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

fn request(
    cwd: &Path,
    agent_dir: &Path,
    explicit_sources: Vec<String>,
    project_trusted: bool,
    discover_extensions: bool,
) -> ResolveRequest {
    ResolveRequest {
        cwd: cwd.to_path_buf(),
        agent_dir: agent_dir.to_path_buf(),
        project_trusted,
        explicit_sources,
        discover_extensions,
    }
}

#[tokio::test]
async fn matches_pi_package_manager_extension_source_precedence() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let paths = [
        root.path().join("cli.ts"),
        cwd.join(".pi/settings/project.ts"),
        cwd.join(".pi/extensions/project-auto.ts"),
        agent_dir.join("settings/user.ts"),
        agent_dir.join("extensions/user-auto.ts"),
        agent_dir.join("npm/node_modules/example-package/extension.ts"),
    ];
    for path in &paths {
        touch(path);
    }
    write_json(
        &cwd.join(".pi/settings.json"),
        json!({"extensions": ["./settings/project.ts"]}),
    );
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "extensions": ["./settings/user.ts"],
            "packages": ["npm:example-package@^1.0.0 || >=2.0.0 <3.0.0"]
        }),
    );
    write_json(
        &agent_dir.join("npm/node_modules/example-package/package.json"),
        json!({
            "name": "example-package",
            "version": "1.0.0",
            "pi": {"extensions": ["./extension.ts"]}
        }),
    );

    let resolution = PackageManager::new(request(
        &cwd,
        &agent_dir,
        vec![paths[0].display().to_string()],
        true,
        true,
    ))
    .resolve()
    .await
    .unwrap();

    assert_eq!(resolution.extension_paths, paths);
}

#[tokio::test]
async fn untrusted_projects_exclude_project_settings_packages_and_automatic_extensions() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let cli = root.path().join("cli.ts");
    let project_settings = cwd.join(".pi/settings/project.ts");
    let project_auto = cwd.join(".pi/extensions/project-auto.ts");
    let user_auto = agent_dir.join("extensions/user-auto.ts");
    for path in [&cli, &project_settings, &project_auto, &user_auto] {
        touch(path);
    }
    write_json(
        &cwd.join(".pi/settings.json"),
        json!({
            "extensions": ["./settings/project.ts"],
            "packages": ["npm:must-not-install"]
        }),
    );

    let resolution = PackageManager::new(request(
        &cwd,
        &agent_dir,
        vec![cli.display().to_string()],
        false,
        true,
    ))
    .resolve()
    .await
    .unwrap();

    assert_eq!(resolution.extension_paths, [cli, user_auto]);
}

#[tokio::test]
async fn applies_package_manifests_extension_filters_and_ignore_files() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let package_root = agent_dir.join("local-package");
    let package_keep = package_root.join("extensions/keep.ts");
    let package_skip = package_root.join("extensions/skip.ts");
    let auto_keep = agent_dir.join("extensions/auto-keep.ts");
    let auto_skip = agent_dir.join("extensions/auto-skip.ts");
    for path in [&package_keep, &package_skip, &auto_keep, &auto_skip] {
        touch(path);
    }
    write_json(
        &package_root.join("package.json"),
        json!({"pi": {"extensions": ["extensions/*.ts"]}}),
    );
    fs::write(agent_dir.join("extensions/.gitignore"), "auto-skip.ts\n").unwrap();
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "packages": [{
                "source": "./local-package",
                "extensions": ["extensions/*.ts", "!extensions/skip.ts"]
            }]
        }),
    );

    let resolution = PackageManager::new(request(&cwd, &agent_dir, Vec::new(), true, true))
        .resolve()
        .await
        .unwrap();

    assert_eq!(resolution.extension_paths, [auto_keep, package_keep]);
}

#[tokio::test]
async fn no_extensions_preserves_explicit_local_directory_discovery() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let explicit_root = root.path().join("explicit");
    let first = explicit_root.join("a.ts");
    let second = explicit_root.join("nested/index.ts");
    let automatic = agent_dir.join("extensions/automatic.ts");
    for path in [&first, &second, &automatic] {
        touch(path);
    }

    let resolution = PackageManager::new(request(
        &cwd,
        &agent_dir,
        vec![explicit_root.display().to_string()],
        true,
        false,
    ))
    .resolve()
    .await
    .unwrap();

    assert_eq!(resolution.extension_paths, [first, second]);
}

#[tokio::test]
async fn installs_missing_npm_sources_with_the_configured_command() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let fake_source = root.path().join("fake_npm.rs");
    let fake_binary = root
        .path()
        .join(format!("fake-npm{}", std::env::consts::EXE_SUFFIX));
    fs::write(
        &fake_source,
        r#"
use std::fs;
use std::path::PathBuf;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("root") {
        print!("/definitely-not-a-global-node-modules-directory");
        return;
    }
    let prefix = PathBuf::from(&args[args.iter().position(|arg| arg == "--prefix").unwrap() + 1]);
    let spec = &args[1];
    let name = spec.rsplit_once('@').map_or(spec.as_str(), |(name, _)| name);
    let package = prefix.join("node_modules").join(name);
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("package.json"), format!(
        "{{\"name\":\"{name}\",\"version\":\"1.0.0\",\"pi\":{{\"extensions\":[\"extension.ts\"]}}}}"
    )).unwrap();
    fs::write(package.join("extension.ts"), "export default function () {}\n").unwrap();
}
"#,
    )
    .unwrap();
    let status = Command::new("rustc")
        .args(["--edition", "2024"])
        .arg(&fake_source)
        .arg("-o")
        .arg(&fake_binary)
        .status()
        .unwrap();
    assert!(status.success());
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "npmCommand": [fake_binary],
            "packages": ["npm:installed-on-demand@1.0.0"]
        }),
    );

    let resolution = PackageManager::new(request(&cwd, &agent_dir, Vec::new(), true, true))
        .resolve()
        .await
        .unwrap();
    let installed = agent_dir.join("npm/node_modules/installed-on-demand/extension.ts");

    assert_eq!(
        resolution.extension_paths.as_slice(),
        std::slice::from_ref(&installed)
    );
    assert!(installed.is_file());
}

#[tokio::test]
async fn project_autoload_false_is_a_delta_over_the_user_package() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let package_root = agent_dir.join("npm/node_modules/shared-package");
    let enabled = package_root.join("extensions/enabled.ts");
    let disabled = package_root.join("extensions/disabled.ts");
    for path in [&enabled, &disabled] {
        touch(path);
    }
    write_json(
        &package_root.join("package.json"),
        json!({
            "name": "shared-package",
            "version": "1.0.0",
            "pi": {"extensions": ["extensions/*.ts"]}
        }),
    );
    write_json(
        &agent_dir.join("settings.json"),
        json!({"packages": ["npm:shared-package@1.0.0"]}),
    );
    write_json(
        &cwd.join(".pi/settings.json"),
        json!({
            "packages": [{
                "source": "npm:shared-package@1.0.0",
                "autoload": false,
                "extensions": ["+extensions/enabled.ts", "-extensions/disabled.ts"]
            }]
        }),
    );

    let resolution = PackageManager::new(request(&cwd, &agent_dir, Vec::new(), true, true))
        .resolve()
        .await
        .unwrap();

    assert_eq!(resolution.extension_paths, [enabled]);
}

#[tokio::test]
async fn resolves_git_packages_from_pis_managed_checkout_layout() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let package_root = agent_dir.join("git/github.com/example/pi-extension");
    let extension = package_root.join("extension.ts");
    touch(&extension);
    write_json(
        &package_root.join("package.json"),
        json!({"pi": {"extensions": ["extension.ts"]}}),
    );
    write_json(
        &agent_dir.join("settings.json"),
        json!({"packages": ["git:https://github.com/example/pi-extension#v1"]}),
    );

    let resolution = PackageManager::new(request(&cwd, &agent_dir, Vec::new(), true, true))
        .resolve()
        .await
        .unwrap();

    assert_eq!(resolution.extension_paths, [extension]);
}

#[tokio::test]
async fn an_empty_package_extension_filter_disables_all_extensions() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let package_root = agent_dir.join("local-package");
    touch(&package_root.join("extensions/disabled.ts"));
    write_json(
        &agent_dir.join("settings.json"),
        json!({
            "packages": [{"source": "./local-package", "extensions": []}]
        }),
    );

    let resolution = PackageManager::new(request(&cwd, &agent_dir, Vec::new(), true, true))
        .resolve()
        .await
        .unwrap();

    assert!(resolution.extension_paths.is_empty());
}

#[tokio::test]
async fn resolves_explicit_file_urls_without_node_path_helpers() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent_dir = root.path().join("agent");
    let extension = root.path().join("extension with spaces.ts");
    touch(&extension);
    let source = url::Url::from_file_path(&extension).unwrap().to_string();

    let resolution = PackageManager::new(request(&cwd, &agent_dir, vec![source], true, false))
        .resolve()
        .await
        .unwrap();

    assert_eq!(resolution.extension_paths, [extension]);
}
