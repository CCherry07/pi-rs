use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ABI_SOURCE_ROOTS: &[&str] = &[
    "crates/pi-core",
    "crates/pi-plugin-macros",
    "crates/pi-plugin-sdk",
    "crates/pi-session",
];

fn main() {
    for variable in [
        "RUSTC",
        "TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_CFG_PANIC",
        "CARGO_CFG_TARGET_FEATURE",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("pi-plugin-sdk must live under the pi-rs workspace");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(rustc)
        .arg("-vV")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_else(|| "unknown-rustc".to_string());

    let mut hash = Fnv1a64::new();
    hash.field("sdk-version", env!("CARGO_PKG_VERSION").as_bytes());
    hash.field("rustc", rustc_version.as_bytes());
    for variable in [
        "TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_CFG_PANIC",
        "CARGO_CFG_TARGET_FEATURE",
    ] {
        hash.field(
            variable,
            std::env::var(variable).unwrap_or_default().as_bytes(),
        );
    }

    let mut inputs = [workspace.join("Cargo.toml"), workspace.join("Cargo.lock")]
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    for root in ABI_SOURCE_ROOTS {
        collect_abi_inputs(&workspace.join(root), &mut inputs);
    }
    inputs.sort();
    inputs.dedup();
    for path in inputs {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(workspace).unwrap_or(&path);
        hash.field("path", relative.to_string_lossy().as_bytes());
        hash.field(
            "contents",
            &fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "failed to read ABI fingerprint input {}: {error}",
                    path.display()
                )
            }),
        );
    }

    println!(
        "cargo:rustc-env=PI_PLUGIN_BUILD_FINGERPRINT={:016x}",
        hash.finish()
    );
}

fn collect_abi_inputs(root: &Path, inputs: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    if root.is_file() {
        inputs.push(root.to_path_buf());
        return;
    }
    let mut entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("failed to scan ABI source {}: {error}", root.display()))
        .map(|entry| {
            entry
                .expect("ABI source directory entry must be readable")
                .path()
        })
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("target")
            ) {
                collect_abi_inputs(&path, inputs);
            }
        } else if matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("Cargo.toml" | "build.rs")
        ) || path.extension().is_some_and(|extension| extension == "rs")
        {
            inputs.push(path);
        }
    }
}

struct Fnv1a64(u64);

impl Fnv1a64 {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn field(&mut self, name: &str, value: &[u8]) {
        self.write(name.as_bytes());
        self.write(&[0]);
        self.write(&(value.len() as u64).to_le_bytes());
        self.write(value);
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}
