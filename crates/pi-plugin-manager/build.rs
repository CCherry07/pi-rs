fn main() {
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    println!("cargo:rustc-env=PI_PLUGIN_HOST_TARGET={target}");
}
