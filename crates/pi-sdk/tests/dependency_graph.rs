//! Keep product implementations out of the shared core/eval production dependency graph.
//! Every crates/ package also excludes Coding products from its own test dependencies.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Value, json};

const GENERIC_PACKAGES: &[&str] = &[
    "pi-core",
    "pi-plugin",
    "pi-agent",
    "pi-runtime",
    "pi-session",
    "pi-sdk",
    "pi-eval",
];
const CODING_PRODUCTS: &[&str] = &["pi-coding", "pi-coding-eval"];
const PRODUCT_DIRECTORIES: &[&str] = &["domains", "apps", "plugins", "bindings", "packages"];

#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
    resolve: Option<Resolution>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
}

#[derive(Deserialize)]
struct Resolution {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    pkg: String,
    dep_kinds: Vec<DependencyUse>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum DependencyKind {
    Build,
    Dev,
}

#[derive(Deserialize)]
struct DependencyUse {
    /// Cargo represents a normal dependency as null.
    kind: Option<DependencyKind>,
    target: Option<String>,
}

impl Dependency {
    fn uses(&self, include_dev: bool) -> impl Iterator<Item = &DependencyUse> {
        self.dep_kinds
            .iter()
            .filter(move |usage| include_dev || usage.kind != Some(DependencyKind::Dev))
    }

    fn description(&self, include_dev: bool) -> String {
        let uses = self
            .uses(include_dev)
            .map(|usage| {
                let kind = match usage.kind {
                    Some(DependencyKind::Build) => "build",
                    Some(DependencyKind::Dev) => "dev",
                    None => "normal",
                };
                match &usage.target {
                    Some(target) => format!("{kind} @ {target}"),
                    None => kind.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}; {uses}", self.name)
    }
}

fn product_manifest<'a>(package: &'a Package, workspace: &Path) -> Option<&'a Path> {
    let relative = package.manifest_path.strip_prefix(workspace).ok()?;
    let directory = relative.components().next()?.as_os_str().to_str()?;
    PRODUCT_DIRECTORIES.contains(&directory).then_some(relative)
}

fn named_workspace_packages<'a>(
    metadata: &'a Metadata,
    names: &[&str],
) -> Result<Vec<&'a Package>, String> {
    names
        .iter()
        .map(|name| {
            let candidates = metadata
                .packages
                .iter()
                .filter(|package| {
                    package.name == *name && metadata.workspace_members.contains(&package.id)
                })
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [package] => Ok(*package),
                _ => Err(format!(
                    "expected exactly one workspace package named {name}, found {}",
                    candidates.len()
                )),
            }
        })
        .collect()
}

fn dependency_violations(metadata: &Metadata, roots: &[&str]) -> Result<Vec<String>, String> {
    let roots = named_workspace_packages(metadata, roots)?;
    trace_dependency_violations(metadata, &roots, false, |package| {
        product_manifest(package, &metadata.workspace_root).is_some()
    })
}

fn coding_dependency_violations(metadata: &Metadata) -> Result<Vec<String>, String> {
    let products = named_workspace_packages(metadata, CODING_PRODUCTS)?;
    let product_ids = products
        .iter()
        .map(|package| package.id.as_str())
        .collect::<HashSet<_>>();
    let mut roots = metadata
        .packages
        .iter()
        .filter(|package| {
            metadata.workspace_members.contains(&package.id)
                && package
                    .manifest_path
                    .starts_with(metadata.workspace_root.join("crates"))
        })
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Err("cargo metadata has no workspace packages under crates/".to_string());
    }
    roots.sort_by(|left, right| left.name.cmp(&right.name));
    trace_dependency_violations(metadata, &roots, true, |package| {
        product_ids.contains(package.id.as_str())
    })
}

fn trace_dependency_violations(
    metadata: &Metadata,
    roots: &[&Package],
    include_root_dev: bool,
    forbidden: impl Fn(&Package) -> bool,
) -> Result<Vec<String>, String> {
    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("cargo metadata omitted its dependency resolution")?;
    let packages = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<HashMap<_, _>>();
    let nodes = resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut violations = Vec::new();

    for root in roots {
        let mut pending = VecDeque::from([(root.id.as_str(), root.name.clone())]);
        let mut visited = HashSet::new();
        while let Some((id, path)) = pending.pop_front() {
            if !visited.insert(id) {
                continue;
            }
            let package = packages
                .get(id)
                .ok_or_else(|| format!("dependency package {id} is absent from cargo metadata"))?;
            let node = nodes
                .get(id)
                .ok_or_else(|| format!("dependency node {id} is absent from cargo resolution"))?;
            if forbidden(package) {
                let manifest = package
                    .manifest_path
                    .strip_prefix(&metadata.workspace_root)
                    .unwrap_or(&package.manifest_path);
                violations.push(format!("{path} ({})", manifest.display()));
            }
            // A package's tests include its own dev dependencies, not the dev dependencies
            // of packages it consumes. Every crates/ workspace package is checked as a root.
            let include_dev = include_root_dev && id == root.id;
            for dependency in &node.deps {
                if dependency.dep_kinds.is_empty() {
                    return Err(format!(
                        "cargo metadata omitted dependency kinds for {id} -> {}",
                        dependency.pkg
                    ));
                }
                if dependency.uses(include_dev).next().is_none() {
                    continue;
                }
                // The resolved package ID is authoritative. `name` may be a dependency alias.
                let target = packages.get(dependency.pkg.as_str()).ok_or_else(|| {
                    format!(
                        "dependency package {} is absent from cargo metadata",
                        dependency.pkg
                    )
                })?;
                pending.push_back((
                    target.id.as_str(),
                    format!(
                        "{path} --[{}]--> {}",
                        dependency.description(include_dev),
                        target.name
                    ),
                ));
            }
        }
    }
    Ok(violations)
}

#[test]
fn workspace_respects_domain_dependency_boundaries() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pi-sdk must live under crates/");
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        // Include optional dependencies and every target. A platform filter would hide leaks.
        .args([
            "metadata",
            "--locked",
            "--format-version=1",
            "--all-features",
        ])
        .output()
        .expect("run cargo metadata for the domain dependency boundary");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .expect("decode cargo metadata with resolved dependency kinds");
    let mut violations = dependency_violations(&metadata, GENERIC_PACKAGES)
        .expect("validate a complete dependency graph for every generic package");
    violations.extend(
        coding_dependency_violations(&metadata)
            .expect("validate Coding product boundaries for every crates/ workspace package"),
    );
    assert!(
        violations.is_empty(),
        "shared core/eval production dependencies must point inward and crates/ tests must not depend on Coding products; forbidden dependency paths:\n{}",
        violations.join("\n")
    );
}

fn dependency(alias: &str, package: &str, kind: Value, target: Value) -> Value {
    json!({ "name": alias, "pkg": package, "dep_kinds": [{ "kind": kind, "target": target }] })
}

fn synthetic_graph(root_dependencies: Vec<Value>, adapter_dependencies: Vec<Value>) -> Metadata {
    serde_json::from_value(json!({
        "workspace_root": "/repo",
        "workspace_members": ["root-id", "adapter-id", "product-id", "product-eval-id"],
        "packages": [
            { "id": "root-id", "name": "pi-sdk", "manifest_path": "/repo/crates/pi-sdk/Cargo.toml" },
            { "id": "adapter-id", "name": "adapter", "manifest_path": "/repo/crates/adapter/Cargo.toml" },
            { "id": "product-id", "name": "pi-coding", "manifest_path": "/repo/domains/coding/Cargo.toml" },
            { "id": "product-eval-id", "name": "pi-coding-eval", "manifest_path": "/repo/domains/coding/eval/Cargo.toml" }
        ],
        "resolve": { "nodes": [
            { "id": "root-id", "deps": root_dependencies },
            { "id": "adapter-id", "deps": adapter_dependencies },
            { "id": "product-id", "deps": [] },
            { "id": "product-eval-id", "deps": [] }
        ] }
    }))
    .unwrap()
}

#[test]
fn detects_transitive_product_dependencies_with_a_complete_path() {
    let graph = synthetic_graph(
        vec![dependency(
            "adapter",
            "adapter-id",
            Value::Null,
            Value::Null,
        )],
        vec![dependency("coding", "product-id", Value::Null, Value::Null)],
    );
    assert_eq!(
        dependency_violations(&graph, &["pi-sdk"]).unwrap(),
        [
            "pi-sdk --[adapter; normal]--> adapter --[coding; normal]--> pi-coding (domains/coding/Cargo.toml)"
        ]
    );
}

#[test]
fn follows_renamed_target_specific_build_dependencies() {
    let graph = synthetic_graph(
        vec![dependency(
            "renamed_build_helper",
            "adapter-id",
            json!("build"),
            json!("cfg(target_os = \"windows\")"),
        )],
        vec![dependency("coding", "product-id", Value::Null, Value::Null)],
    );
    let violations = dependency_violations(&graph, &["pi-sdk"]).unwrap();
    assert_eq!(violations.len(), 1);
    assert!(violations[0].contains("renamed_build_helper; build @ cfg(target_os = \"windows\")"));
    assert!(violations[0].ends_with("pi-coding (domains/coding/Cargo.toml)"));
}

#[test]
fn excludes_dev_only_paths_but_includes_mixed_dependency_kinds() {
    let mut graph = synthetic_graph(
        vec![dependency(
            "fixture",
            "adapter-id",
            json!("dev"),
            Value::Null,
        )],
        vec![dependency("coding", "product-id", Value::Null, Value::Null)],
    );
    assert!(
        dependency_violations(&graph, &["pi-sdk"])
            .unwrap()
            .is_empty()
    );
    graph.resolve.as_mut().unwrap().nodes[0].deps[0]
        .dep_kinds
        .push(DependencyUse {
            kind: None,
            target: None,
        });
    assert_eq!(dependency_violations(&graph, &["pi-sdk"]).unwrap().len(), 1);
}

#[test]
fn rejects_product_directories_only_inside_this_repository() {
    let mut graph = synthetic_graph(
        vec![dependency(
            "product",
            "product-id",
            Value::Null,
            Value::Null,
        )],
        Vec::new(),
    );
    for directory in PRODUCT_DIRECTORIES {
        graph.packages[2].manifest_path = format!("/repo/{directory}/product/Cargo.toml").into();
        assert_eq!(dependency_violations(&graph, &["pi-sdk"]).unwrap().len(), 1);
    }
    for allowed in [
        "/external/plugins/product/Cargo.toml",
        "/repo/plugins-support/Cargo.toml",
        "/repo/crates/product/Cargo.toml",
    ] {
        graph.packages[2].manifest_path = allowed.into();
        assert!(
            dependency_violations(&graph, &["pi-sdk"])
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn discovers_future_crate_roots_without_a_manual_allowlist() {
    let mut graph = synthetic_graph(
        Vec::new(),
        vec![dependency("coding", "product-id", Value::Null, Value::Null)],
    );
    graph.packages[1].name = "future-library".into();
    graph.packages[1].manifest_path = "/repo/crates/future-library/Cargo.toml".into();
    // This new crate is unrelated to the manually listed core roots.
    assert!(
        dependency_violations(&graph, &["pi-sdk"])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        coding_dependency_violations(&graph).unwrap(),
        ["future-library --[coding; normal]--> pi-coding (domains/coding/Cargo.toml)"]
    );
}

#[test]
fn rejects_both_coding_products_in_root_dev_dependencies() {
    for (id, name, manifest) in [
        ("product-id", "pi-coding", "domains/coding/Cargo.toml"),
        (
            "product-eval-id",
            "pi-coding-eval",
            "domains/coding/eval/Cargo.toml",
        ),
    ] {
        let graph = synthetic_graph(
            vec![dependency("fixture", id, json!("dev"), Value::Null)],
            Vec::new(),
        );
        assert_eq!(
            coding_dependency_violations(&graph).unwrap(),
            [format!("pi-sdk --[fixture; dev]--> {name} ({manifest})")]
        );
    }
}

#[test]
fn follows_root_dev_dependencies_through_target_specific_build_helpers() {
    let graph = synthetic_graph(
        vec![dependency(
            "fixture",
            "adapter-id",
            json!("dev"),
            Value::Null,
        )],
        vec![dependency(
            "renamed_eval",
            "product-eval-id",
            json!("build"),
            json!("cfg(target_os = \"windows\")"),
        )],
    );
    let violations = coding_dependency_violations(&graph).unwrap();
    assert!(violations.iter().any(|path| path == "pi-sdk --[fixture; dev]--> adapter --[renamed_eval; build @ cfg(target_os = \"windows\")]--> pi-coding-eval (domains/coding/eval/Cargo.toml)"));
}

#[test]
fn does_not_propagate_downstream_dev_dependencies() {
    let mut graph = synthetic_graph(
        vec![dependency(
            "adapter",
            "adapter-id",
            Value::Null,
            Value::Null,
        )],
        vec![dependency(
            "coding",
            "product-id",
            json!("dev"),
            Value::Null,
        )],
    );
    // The adapter's own tests violate the boundary; pi-sdk does not consume those tests.
    assert_eq!(
        coding_dependency_violations(&graph).unwrap(),
        ["adapter --[coding; dev]--> pi-coding (domains/coding/Cargo.toml)"]
    );
    graph.workspace_members.retain(|id| id != "adapter-id");
    assert!(coding_dependency_violations(&graph).unwrap().is_empty());
}

#[test]
fn distinguishes_workspace_products_from_external_packages_with_the_same_name() {
    for name in CODING_PRODUCTS {
        let mut graph = synthetic_graph(
            vec![dependency(
                "external",
                "external-id",
                Value::Null,
                Value::Null,
            )],
            Vec::new(),
        );
        graph.packages.push(Package {
            id: "external-id".into(),
            name: (*name).into(),
            manifest_path: format!("/registry/{name}/Cargo.toml").into(),
        });
        graph.resolve.as_mut().unwrap().nodes.push(Node {
            id: "external-id".into(),
            deps: Vec::new(),
        });
        assert!(coding_dependency_violations(&graph).unwrap().is_empty());
    }
}

#[test]
fn incomplete_metadata_cannot_silently_pass() {
    let mut graph = synthetic_graph(Vec::new(), Vec::new());
    assert!(dependency_violations(&graph, &["missing-root"]).is_err());
    graph.resolve.as_mut().unwrap().nodes.clear();
    assert!(dependency_violations(&graph, &["pi-sdk"]).is_err());
    graph.resolve = None;
    assert!(dependency_violations(&graph, &["pi-sdk"]).is_err());
}

#[test]
fn coding_boundary_requires_workspace_product_identities_and_crate_roots() {
    let mut graph = synthetic_graph(Vec::new(), Vec::new());
    graph.workspace_members.retain(|id| id != "product-eval-id");
    assert!(coding_dependency_violations(&graph).is_err());
    graph.workspace_members = vec!["product-id".into(), "product-eval-id".into()];
    assert!(coding_dependency_violations(&graph).is_err());
}
