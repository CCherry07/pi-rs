#![cfg(feature = "loader")]

use pi_plugin::native::NATIVE_PLUGIN_ABI_VERSION;
use pi_plugin_manager::loader::{NativePluginError, NativePluginLoader, NativePluginLoaderOptions};

#[test]
fn same_abi_stale_fingerprint_is_rejected_before_rust_constructor_lookup() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("stale.rs");
    // Deliberately export only the stable C descriptor. Looking up the missing
    // Rust provider constructor would prove the fingerprint gate ran too late.
    std::fs::write(&source, format!(r#"
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bytes {{ ptr: *const u8, len: usize }}
unsafe impl Sync for Bytes {{}}
impl Bytes {{ const fn text(s: &'static str) -> Self {{ Self {{ ptr: s.as_ptr(), len: s.len() }} }} }}
#[repr(C)]
pub struct Descriptor {{ abi: u32, kind: u32, id: Bytes, version: Bytes, fingerprint: Bytes }}
static DESCRIPTOR: Descriptor = Descriptor {{
    abi: {NATIVE_PLUGIN_ABI_VERSION}, kind: 2,
    id: Bytes::text("stale-provider"), version: Bytes::text("0.1.0"),
    fingerprint: Bytes::text("before-provider-request-cache-retention"),
}};
#[unsafe(no_mangle)]
pub extern "C" fn pi_plugin_descriptor_v1() -> *const Descriptor {{ &DESCRIPTOR }}
"#)).unwrap();
    let artifact = temp.path().join(format!(
        "{}stale_provider{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let compiled =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "--crate-type=cdylib"])
            .arg(&source)
            .arg("-o")
            .arg(&artifact)
            .output()
            .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut options = NativePluginLoaderOptions::new(temp.path(), temp.path().join("agent"));
    options.explicit_paths.push(artifact);
    let error = match NativePluginLoader::new(options).discover() {
        Ok(_) => panic!("stale native provider loaded"),
        Err(error) => error,
    };
    assert!(
        matches!(error, NativePluginError::BuildMismatch { actual, expected, .. }
        if actual == "before-provider-request-cache-retention" && expected == pi_plugin::native::BUILD_FINGERPRINT)
    );
}
