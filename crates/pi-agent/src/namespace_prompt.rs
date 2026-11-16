//! Request-local instructions derived from the tools actually declared to the provider.

use std::collections::HashSet;

use pi_plugin::FrozenRegistries;

pub(crate) fn with_instructions(
    base: &str,
    registries: &FrozenRegistries,
    active: &[String],
) -> String {
    let mut prompt = base.to_string();
    let mut seen = HashSet::new();
    for name in active {
        let Some(namespace) = registries
            .tool_discovery(name)
            .and_then(|discovery| discovery.namespace.as_ref())
        else {
            continue;
        };
        let Some(instructions) = namespace
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if seen.insert(namespace.name.as_str()) {
            prompt.push_str(&format!(
                "\n\nInstructions for tools in namespace `{}`:\n{instructions}",
                namespace.name
            ));
        }
    }
    prompt
}
