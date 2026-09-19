//! Product inventory metadata derived from prepared plugins; no loading or activation.

use std::collections::HashSet;
use std::path::PathBuf;

use pi_core::PluginId;
use pi_js_package_manager::ResolvedExtensionIdentity;
use pi_plugin_loader::NativePlugins;
use pi_plugin_manager::PreparedPluginReconcile;

pub(crate) fn configured_native_plugin_ids(
    reconciliations: &[PreparedPluginReconcile],
    native_plugins: &NativePlugins,
) -> Vec<PluginId> {
    retain_loaded_configured_native_plugins(
        reconciliations
            .iter()
            .flat_map(PreparedPluginReconcile::installed)
            .map(|plugin| plugin.id),
        native_plugins
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id),
    )
}

fn retain_loaded_configured_native_plugins(
    configured: impl IntoIterator<Item = String>,
    loaded: impl IntoIterator<Item = String>,
) -> Vec<PluginId> {
    let loaded = loaded.into_iter().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    configured
        .into_iter()
        .filter(|id| loaded.contains(id) && seen.insert(id.clone()))
        .map(PluginId::new)
        .collect()
}

pub(crate) fn javascript_inventory_labels(identities: &[ResolvedExtensionIdentity]) -> Vec<String> {
    let paths = identities
        .iter()
        .filter_map(|identity| match identity {
            ResolvedExtensionIdentity::Package(_) => None,
            ResolvedExtensionIdentity::Path(path) => Some(path.clone()),
        })
        .collect::<Vec<_>>();
    let mut path_labels = compact_extension_labels(&paths).into_iter();
    let mut seen = HashSet::new();

    identities
        .iter()
        .filter_map(|identity| {
            let label = match identity {
                ResolvedExtensionIdentity::Package(source) => source.clone(),
                ResolvedExtensionIdentity::Path(_) => path_labels.next()?,
            };
            seen.insert(label.clone()).then_some(label)
        })
        .collect()
}

fn compact_extension_labels(paths: &[PathBuf]) -> Vec<String> {
    let segments = paths
        .iter()
        .map(|path| {
            let mut segments = path
                .iter()
                .map(|segment| segment.to_string_lossy().into_owned())
                .filter(|segment| !segment.is_empty() && segment != "/")
                .collect::<Vec<_>>();
            if segments.len() > 1
                && matches!(
                    segments.last().map(String::as_str),
                    Some("index.ts" | "index.js")
                )
            {
                segments.pop();
            }
            if segments.is_empty() {
                segments.push(path.display().to_string());
            }
            segments
        })
        .collect::<Vec<_>>();

    segments
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (1..=path.len())
                .find_map(|count| {
                    let candidate = &path[path.len() - count..];
                    segments
                        .iter()
                        .enumerate()
                        .all(|(other_index, other)| {
                            other_index == index
                                || other.len() < count
                                || !other.ends_with(candidate)
                        })
                        .then(|| candidate.join("/"))
                })
                .unwrap_or_else(|| path.join("/"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_inventory_only_keeps_loaded_plugins_from_plugins_json() {
        let plugins = retain_loaded_configured_native_plugins(
            [
                "configured".to_string(),
                "not-loaded".to_string(),
                "configured".to_string(),
            ],
            ["configured".to_string(), "explicit-path".to_string()],
        );

        assert_eq!(plugins, [PluginId::new("configured")]);
    }

    #[test]
    fn javascript_inventory_uses_package_sources_without_hiding_local_extensions() {
        let labels = javascript_inventory_labels(&[
            ResolvedExtensionIdentity::Package("npm:@counterposition/pi-web-search".to_string()),
            ResolvedExtensionIdentity::Package("npm:@narumitw/pi-lsp@0.49.5".to_string()),
            ResolvedExtensionIdentity::Path(PathBuf::from(
                "/workspace/.pi/extensions/clipboard.ts",
            )),
            ResolvedExtensionIdentity::Path(PathBuf::from(
                "/workspace/local/session-tools/index.ts",
            )),
        ]);

        assert_eq!(
            labels,
            [
                "npm:@counterposition/pi-web-search",
                "npm:@narumitw/pi-lsp@0.49.5",
                "clipboard.ts",
                "session-tools",
            ]
        );
    }
}
