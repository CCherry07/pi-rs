use std::path::Path;
use std::process::Command;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/HEAD").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/refs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/packed-refs").display()
    );
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=PI_PLUGIN_SOURCE_REVISION={revision}");
    let rustc = std::env::var_os("RUSTC").expect("Cargo supplies RUSTC");
    let output = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("rustc --version");
    let version = String::from_utf8(output.stdout).expect("rustc version is UTF-8");
    let version = version
        .split_whitespace()
        .nth(1)
        .expect("rustc release version");
    println!("cargo:rustc-env=PI_PLUGIN_RUST_VERSION={version}");
}
