use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pi_js_package_manager::{
    ManageOperation, ManageResult, PackageManager, PackageManagerError, PackageScope,
    ResolveRequest,
};
use serde_json::json;
use tempfile::TempDir;

fn request(root: &TempDir, trusted: bool) -> ResolveRequest {
    ResolveRequest {
        cwd: root.path().join("project"),
        agent_dir: root.path().join("agent"),
        project_trusted: trusted,
        explicit_sources: Vec::new(),
        discover_extensions: true,
    }
}

fn write_json(path: &Path, value: serde_json::Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

fn git(cwd: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(repo: &Path, contents: &str, message: &str) -> String {
    fs::write(repo.join("index.ts"), contents).unwrap();
    git(repo, &["add", "index.ts"]);
    git(repo, &["commit", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

#[cfg(unix)]
fn install_fake_npm(agent_dir: &Path) -> PathBuf {
    let script = agent_dir.join("fake-npm.sh");
    fs::create_dir_all(agent_dir).unwrap();
    fs::write(
        &script,
        r#"#!/bin/sh
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' "$*" >> "$script_dir/npm.log"
command=$1
shift
case "$command" in
  view)
    printf '"2.0.0"\n'
    ;;
  install)
    specs=""
    prefix=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --prefix|--cwd)
          prefix=$2
          shift 2
          ;;
        --*) shift ;;
        *) specs="$specs $1"; shift ;;
      esac
    done
    for spec in $specs; do
      name=${spec%@*}
      [ -n "$name" ] || name=$spec
      mkdir -p "$prefix/node_modules/$name"
      printf '{"name":"%s","version":"2.0.0"}\n' "$name" > "$prefix/node_modules/$name/package.json"
    done
    ;;
  uninstall)
    name=$1
    shift
    prefix=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --prefix|--cwd) prefix=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    rm -rf "$prefix/node_modules/$name"
    ;;
esac
"#,
    )
    .unwrap();
    script
}

#[cfg(unix)]
#[tokio::test]
async fn install_list_and_remove_share_managed_state_and_preserve_settings() {
    let root = TempDir::new().unwrap();
    let request = request(&root, true);
    fs::create_dir_all(&request.cwd).unwrap();
    let npm = install_fake_npm(&request.agent_dir);
    let settings_path = request.agent_dir.join("settings.json");
    write_json(
        &settings_path,
        json!({
            "theme": "night",
            "npmCommand": ["sh", npm],
            "packages": []
        }),
    );
    let mut manager = PackageManager::new(request.clone());

    assert!(matches!(
        manager
            .manage(ManageOperation::Install {
                source: "npm:example".to_string(),
                scope: PackageScope::User,
            })
            .await
            .unwrap(),
        ManageResult::Installed { .. }
    ));
    let listed = manager.manage(ManageOperation::List).await.unwrap();
    let ManageResult::Listed { packages } = listed else {
        panic!("expected package list");
    };
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].source, "npm:example");
    assert_eq!(packages[0].scope, PackageScope::User);
    assert!(packages[0].installed_path.is_some());

    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(settings["theme"], "night");
    assert_eq!(settings["packages"], json!(["npm:example"]));

    let removed = manager
        .manage(ManageOperation::Remove {
            source: "npm:example@9".to_string(),
            scope: PackageScope::User,
        })
        .await
        .unwrap();
    assert!(matches!(
        removed,
        ManageResult::Removed {
            configured: true,
            ..
        }
    ));
    assert!(!request.agent_dir.join("npm/node_modules/example").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn update_batches_unpinned_npm_and_skips_exact_versions() {
    let root = TempDir::new().unwrap();
    let request = request(&root, true);
    fs::create_dir_all(&request.cwd).unwrap();
    let npm = install_fake_npm(&request.agent_dir);
    write_json(
        &request.agent_dir.join("settings.json"),
        json!({
            "npmCommand": ["sh", npm],
            "packages": ["npm:floating", "npm:pinned@1.0.0"]
        }),
    );
    for name in ["floating", "pinned"] {
        write_json(
            &request
                .agent_dir
                .join("npm/node_modules")
                .join(name)
                .join("package.json"),
            json!({ "name": name, "version": "1.0.0" }),
        );
    }
    let mut manager = PackageManager::new(request.clone());

    let result = manager
        .manage(ManageOperation::Update { source: None })
        .await
        .unwrap();
    assert!(matches!(result, ManageResult::Updated { ref sources } if sources.len() == 2));
    let log = fs::read_to_string(request.agent_dir.join("npm.log")).unwrap();
    assert!(log.contains("view floating version --json"));
    assert!(log.contains("install floating@latest"));
    assert!(!log.contains("view pinned"));
    assert!(!log.contains("install pinned"));
}

#[tokio::test]
async fn project_writes_require_trust_and_list_marks_filtered_entries() {
    let root = TempDir::new().unwrap();
    let request = request(&root, false);
    fs::create_dir_all(&request.cwd).unwrap();
    fs::create_dir_all(request.cwd.join("extension")).unwrap();
    write_json(
        &request.agent_dir.join("settings.json"),
        json!({
            "packages": [{
                "source": "./shared",
                "autoload": false,
                "extensions": ["index.ts"],
                "futureField": true
            }]
        }),
    );
    let mut manager = PackageManager::new(request);

    let error = manager
        .manage(ManageOperation::Install {
            source: "./extension".to_string(),
            scope: PackageScope::Project,
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PackageManagerError::ProjectNotTrusted));

    let ManageResult::Listed { packages } = manager.manage(ManageOperation::List).await.unwrap()
    else {
        panic!("expected package list");
    };
    assert_eq!(packages.len(), 1);
    assert!(packages[0].filtered);
    assert_eq!(packages[0].scope, PackageScope::User);
}

#[tokio::test]
async fn update_reconciles_managed_git_checkout_to_its_remote_branch() {
    let root = TempDir::new().unwrap();
    let request = request(&root, true);
    fs::create_dir_all(&request.cwd).unwrap();
    let remote = root.path().join("remote");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--initial-branch=main"]);
    git(&remote, &["config", "user.email", "pi@example.test"]);
    git(&remote, &["config", "user.name", "Pi Test"]);
    commit(&remote, "// v1", "v1");

    let installed = request.agent_dir.join("git/github.com/test/extension");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    git(
        root.path(),
        &[
            "clone",
            remote.to_str().unwrap(),
            installed.to_str().unwrap(),
        ],
    );
    write_json(
        &request.agent_dir.join("settings.json"),
        json!({ "packages": ["git:github.com/test/extension"] }),
    );
    let expected = commit(&remote, "// v2", "v2");
    let mut manager = PackageManager::new(request);

    manager
        .manage(ManageOperation::Update {
            source: Some("git:github.com/test/extension".to_string()),
        })
        .await
        .unwrap();

    assert_eq!(git(&installed, &["rev-parse", "HEAD"]), expected);
    assert_eq!(
        fs::read_to_string(installed.join("index.ts")).unwrap(),
        "// v2"
    );
}

#[tokio::test]
async fn targeted_update_suggests_required_source_prefixes() {
    let root = TempDir::new().unwrap();
    let request = request(&root, true);
    fs::create_dir_all(&request.cwd).unwrap();
    write_json(
        &request.agent_dir.join("settings.json"),
        json!({ "packages": ["npm:example"] }),
    );
    let mut manager = PackageManager::new(request);

    let error = manager
        .manage(ManageOperation::Update {
            source: Some("example".to_string()),
        })
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "No matching package found for example. Did you mean npm:example?"
    );
}
