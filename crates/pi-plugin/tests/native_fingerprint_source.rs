// Exercise the actual build-script collector/hash, not a duplicate fingerprint algorithm.
#[allow(dead_code)]
mod fingerprint {
    include!("../build.rs");

    pub fn provider_source_is_covered() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let provider = workspace.join("crates/pi-plugin/src/provider.rs");
        let mut inputs = Vec::new();
        for root in ABI_SOURCE_ROOTS {
            collect_abi_inputs(&workspace.join(root), &mut inputs);
        }
        assert!(
            inputs.contains(&provider),
            "ProviderRequest layout must invalidate native builds"
        );
        let current = std::fs::read_to_string(&provider).unwrap();
        let field = "    pub cache_retention: Option<CacheRetention>,\n";
        assert!(current.contains(field));
        let old_layout = current.replace(field, "");
        let hash = |source: &str| {
            let mut hash = Fnv1a64::new();
            hash.field("path", b"crates/pi-plugin/src/provider.rs");
            hash.field("contents", source.as_bytes());
            hash.finish()
        };
        assert_ne!(hash(&current), hash(&old_layout));
    }
}

#[test]
fn provider_request_layout_changes_invalidate_native_fingerprint() {
    fingerprint::provider_source_is_covered();
}
