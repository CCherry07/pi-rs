//! Provider-safe MCP names, matching Pi's JavaScript normalization and hash suffix.

use sha2::{Digest, Sha256};

const MAX_TOOL_NAME_LENGTH: usize = 64;
const HASH_LENGTH: usize = 8;

/// Names are stable for an unsanitized server/tool pair. The caller marks every
/// member of a colliding group as taken before filtering tools by exposure.
pub(crate) fn qualified_tool_name(
    server: &str,
    tool: &str,
    is_taken: impl Fn(&str) -> bool,
) -> String {
    let name = format!(
        "mcp__{}__{}",
        identifier_component(server),
        identifier_component(tool)
    );
    if name.len() <= MAX_TOOL_NAME_LENGTH && !is_taken(&name) {
        return name;
    }

    let mut digest = Sha256::new();
    digest.update(server.as_bytes());
    digest.update([0]);
    digest.update(tool.as_bytes());
    let suffix: String = digest
        .finalize()
        .iter()
        .take(HASH_LENGTH / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let prefix: String = name
        .chars()
        .take(MAX_TOOL_NAME_LENGTH - HASH_LENGTH - 1)
        .collect();
    format!("{prefix}_{suffix}")
}

/// Pi's `/[^A-Za-z0-9_]/g` works on UTF-16 units, so an astral character becomes
/// two underscores. It preserves case, adjacent separators and edge underscores.
pub(crate) fn identifier_component(value: &str) -> String {
    value
        .encode_utf16()
        .map(|unit| match unit {
            0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f => {
                char::from_u32(u32::from(unit)).unwrap_or('_')
            }
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_pi_identifier_case_and_separator_positions() {
        assert_eq!(
            qualified_tool_name("My-server", "_Get..Item/V2_", |_| false),
            "mcp__My_server___Get__Item_V2_"
        );
        assert_eq!(identifier_component(".Å😀_"), "_____");
    }

    #[test]
    fn hashes_both_members_of_a_known_collision_using_unsanitized_names() {
        assert_eq!(
            qualified_tool_name("s", "a-b", |name| name == "mcp__s__a_b"),
            "mcp__s__a_b_ed3867d6"
        );
        assert_eq!(
            qualified_tool_name("s", "a_b", |name| name == "mcp__s__a_b"),
            "mcp__s__a_b_8d47d403"
        );
    }

    #[test]
    fn long_names_are_bounded_and_hash_the_complete_raw_identity() {
        let tool = "x".repeat(100);
        let name = qualified_tool_name("docs", &tool, |_| false);
        assert_eq!(name, format!("mcp__docs__{}_bb31f9f5", "x".repeat(44)));
        assert_eq!(name.len(), 64);
        assert_ne!(
            name,
            qualified_tool_name("docs", &format!("{tool}y"), |_| false)
        );
    }

    #[test]
    fn sixty_four_character_name_needs_no_suffix() {
        let tool = "x".repeat(56);
        let name = qualified_tool_name("s", &tool, |_| false);
        assert_eq!(name, format!("mcp__s__{tool}"));
        assert_eq!(name.len(), 64);
    }

    #[test]
    fn astral_names_use_two_underscores_but_hash_original_utf8() {
        assert_eq!(qualified_tool_name("s", "😀", |_| false), "mcp__s____");
        assert_eq!(
            qualified_tool_name("s", "😀", |_| true),
            "mcp__s_____5d9186a1"
        );
    }
}
