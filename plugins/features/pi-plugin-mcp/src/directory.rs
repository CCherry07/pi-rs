//! Bounded discovery hints for servers whose schemas are loaded on demand.

const INTRO: &str = "MCP servers whose tools are not declared to you. Load the tools of `tool_search` servers with `tool_search`.";
const MAX_SECTION: usize = 4096;
const MAX_SUMMARY: usize = 250;

pub(crate) fn render_servers_section<'a>(
    servers: impl Iterator<Item = (&'a str, Option<&'a str>, Option<&'a str>, &'a str)>,
) -> Option<String> {
    let mut servers = servers.collect::<Vec<_>>();
    servers.sort_by(|left, right| left.0.cmp(right.0));
    if servers.is_empty() {
        return None;
    }
    let intro = if servers.iter().any(|(_, _, _, reach)| *reach == "codemode") {
        format!(
            "{INTRO} Call tools of `codemode` servers from codemode scripts; discover them with searchTools() and read instructions with describeNamespace()."
        )
    } else {
        INTRO.to_string()
    };
    let heads = servers
        .iter()
        .map(|(name, _, _, reach)| format!("- mcp__{} ({reach})", name.replace('-', "_")))
        .collect::<Vec<_>>();
    let omitted = |count: usize| {
        if count == 0 {
            String::new()
        } else {
            format!(
                "\n- … {count} more server{}; use tool_search or codemode searchTools()",
                if count == 1 { "" } else { "s" }
            )
        }
    };
    let size = |kept: usize| {
        utf16_len(&intro)
            + heads[..kept]
                .iter()
                .map(|line| utf16_len(line) + 1)
                .sum::<usize>()
            + utf16_len(&omitted(servers.len() - kept))
    };
    let mut kept = servers.len();
    while kept > 0 && size(kept) > MAX_SECTION {
        kept -= 1;
    }
    let per_server = (MAX_SECTION - size(kept))
        .checked_div(kept)
        .unwrap_or_default()
        .saturating_sub(2)
        .min(MAX_SUMMARY);
    let mut section = intro;
    for (index, (_, description, instructions, _)) in servers.iter().take(kept).enumerate() {
        let summary = description
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| instructions.map(str::trim))
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .trim();
        let summary = truncate(summary, per_server);
        section.push('\n');
        section.push_str(&heads[index]);
        if !summary.is_empty() {
            section.push_str(": ");
            section.push_str(&summary);
        }
    }
    section.push_str(&omitted(servers.len() - kept));
    Some(section)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn truncate(text: &str, limit: usize) -> String {
    if utf16_len(text) <= limit {
        return text.to_string();
    }
    if limit <= 1 {
        return String::new();
    }
    let mut remaining = limit - 1;
    let head = text
        .chars()
        .take_while(|ch| {
            if ch.len_utf16() > remaining {
                return false;
            }
            remaining -= ch.len_utf16();
            true
        })
        .collect::<String>();
    format!("{}…", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_use_description_then_instructions_and_normalized_names() {
        let section = render_servers_section(
            [
                (
                    "z-docs",
                    None,
                    Some("  Documentation server\nsecret second line"),
                    "tool_search",
                ),
                (
                    "a",
                    Some("  Configured summary\nlong description"),
                    Some("fallback"),
                    "tool_search",
                ),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(
            section,
            format!(
                "{INTRO}\n- mcp__a (tool_search): Configured summary\n- mcp__z_docs (tool_search): Documentation server"
            )
        );
        assert!(render_servers_section(std::iter::empty()).is_none());
    }

    #[test]
    fn codemode_servers_describe_the_script_discovery_path() {
        let section = render_servers_section(
            [("docs", Some("Reference library"), None, "codemode")].into_iter(),
        )
        .unwrap();
        assert!(section.contains("mcp__docs (codemode): Reference library"));
        assert!(section.contains("searchTools()"));
        assert!(section.contains("describeNamespace()"));
    }

    #[test]
    fn catalog_and_unicode_summaries_are_bounded() {
        let description = "😀".repeat(300);
        let section = render_servers_section(std::iter::once((
            "docs",
            Some(description.as_str()),
            None,
            "tool_search",
        )))
        .unwrap();
        assert!(utf16_len(section.split(": ").last().unwrap()) <= MAX_SUMMARY);
        let names = (0..400)
            .map(|i| format!("server-{i:04}"))
            .collect::<Vec<_>>();
        let section = render_servers_section(names.iter().map(|name| {
            (
                name.as_str(),
                Some(description.as_str()),
                None,
                "tool_search",
            )
        }))
        .unwrap();
        assert!(utf16_len(&section) <= MAX_SECTION);
        assert!(section.contains("more servers; use tool_search or codemode searchTools()"));
    }
}
