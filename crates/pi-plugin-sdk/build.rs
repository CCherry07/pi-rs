use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=TARGET");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(rustc)
        .arg("-vV")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_else(|| "unknown-rustc".to_string());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    let fingerprint_input = format!(
        "sdk={}\nrustc={}\ntarget={target}\n",
        env!("CARGO_PKG_VERSION"),
        rustc_version
    );
    println!(
        "cargo:rustc-env=PI_PLUGIN_BUILD_FINGERPRINT={:016x}",
        fnv1a64(fingerprint_input.as_bytes())
    );
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
