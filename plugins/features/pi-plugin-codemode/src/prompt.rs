use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pi_plugin::{ToolExposure, ToolInfo};

use crate::CODEMODE_TOOL_NAME;
use crate::declarations::{character_count, describe_script_call, identifier, sample};

pub const DEFAULT_CODEMODE_INLINE_BUDGET: usize = 3000;

/// How codemode presents callable tools to the model while it is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodemodeMode {
    /// Keep direct declarations and append their script call and return type.
    #[default]
    On,
    /// Present direct tools inside codemode instead of as separate declarations.
    Only,
}

#[derive(Debug, Clone)]
pub struct CodemodeOptions {
    pub mode: CodemodeMode,
    pub inline_budget: usize,
    pub docs_path: PathBuf,
}

impl Default for CodemodeOptions {
    fn default() -> Self {
        Self {
            mode: CodemodeMode::On,
            inline_budget: DEFAULT_CODEMODE_INLINE_BUDGET,
            docs_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("codemode.md"),
        }
    }
}

const DESCRIPTION_INTRO: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.\n- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. A tool whose declaration returns an object resolves on failure too when it returned that object; check its fields. Calls still running when the script ends are cancelled.\n- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`";

pub(crate) const PROMPT_SNIPPET: &str = "Run JavaScript that calls other tools";
pub(crate) const PROMPT_GUIDELINE: &str = "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.";

fn describe_globals(options: &CodemodeOptions) -> String {
    format!(
        "Globals:\n- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script. With several text items, each starts with a `==> text N/M <==` line, and `console` lines follow the other output in one `<console_output>` block. `image()` also saves the image to a temp file and the result names its path.\n- `store(key, value)` and `load(key)` keep JSON values across codemode calls.\n- `ALL_TOOLS`, `await searchTools(query, {{ limit?, namespace? }})`, `await describeTool(name)`, `await describeNamespace(name)`: find unlisted tools, such as MCP tools.\n- `models`: classifiers and image generation. Read {} first.",
        options.docs_path.to_string_lossy().replace('\\', "/")
    )
}

struct CatalogEntry {
    name: String,
    section: String,
    cost: usize,
}

/// Render against the original scoped catalog, never previously rewritten descriptions.
pub(crate) fn description(tools: &[ToolInfo], options: &CodemodeOptions) -> String {
    let mut sections = vec![DESCRIPTION_INTRO.into(), describe_globals(options)];
    let mut groups = BTreeMap::<Option<String>, (Option<String>, Vec<CatalogEntry>)>::new();
    for tool in tools.iter().filter(|tool| {
        tool.spec.name != CODEMODE_TOOL_NAME && tool.discovery.exposure != ToolExposure::Deferred
    }) {
        let id = identifier(&tool.spec.name);
        let heading = if id == tool.spec.name {
            format!("### `{id}`")
        } else {
            format!("### `{id}` (`{}`)", tool.spec.name)
        };
        let section = format!("{heading}\n{}", sample(&tool.spec).trim());
        let namespace = tool.discovery.namespace.as_ref();
        let group = groups
            .entry(namespace.map(|namespace| namespace.name.clone()))
            .or_insert_with(|| {
                (
                    namespace.and_then(|namespace| namespace.description.clone()),
                    Vec::new(),
                )
            });
        group.1.push(CatalogEntry {
            name: tool.spec.name.clone(),
            cost: character_count(&section).div_ceil(4),
            section,
        });
    }
    if groups.is_empty() {
        return sections.join("\n\n");
    }
    let shown = select_catalog(
        groups.values().map(|(_, entries)| entries.as_slice()),
        options.inline_budget,
    );
    let mut tool_sections = vec!["Nested tools:".to_string()];
    for (namespace, (description, entries)) in groups {
        let visible = entries
            .iter()
            .filter(|entry| shown.contains(&entry.name))
            .collect::<Vec<_>>();
        if let Some(namespace) = namespace {
            let listing = if visible.len() == entries.len() {
                ""
            } else if visible.is_empty() {
                " (tools not listed)"
            } else {
                " (some tools not listed)"
            };
            let description = description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map_or_else(String::new, |description| format!("\n{description}"));
            tool_sections.push(format!("## {namespace}{listing}{description}"));
        }
        tool_sections.extend(visible.into_iter().map(|entry| entry.section.clone()));
    }
    sections.push(tool_sections.join("\n\n"));
    sections.join("\n\n")
}

fn select_catalog<'a>(
    groups: impl IntoIterator<Item = &'a [CatalogEntry]>,
    budget: usize,
) -> HashSet<String> {
    let mut queues = groups
        .into_iter()
        .map(|entries| {
            let mut entries = entries.iter().collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.cost);
            entries.into_iter().peekable()
        })
        .filter(|queue| queue.len() > 0)
        .collect::<Vec<_>>();
    let mut shown = HashSet::new();
    let mut remaining = budget;
    while !queues.is_empty() {
        queues.retain_mut(|queue| {
            let Some(next) = queue.peek() else {
                return false;
            };
            if next.cost > remaining {
                return false;
            }
            remaining -= next.cost;
            shown.insert(next.name.clone());
            queue.next();
            queue.peek().is_some()
        });
    }
    shown
}

pub(crate) fn prepare_descriptions(
    declared: &[ToolInfo],
    callable: &[ToolInfo],
    options: &CodemodeOptions,
) -> (BTreeMap<String, String>, Vec<String>) {
    let callable_names = callable
        .iter()
        .map(|tool| tool.spec.name.as_str())
        .collect::<HashSet<_>>();
    let mut descriptions = BTreeMap::new();
    if options.mode == CodemodeMode::On {
        for tool in declared {
            if callable_names.contains(tool.spec.name.as_str()) {
                descriptions.insert(tool.spec.name.clone(), describe_script_call(&tool.spec));
            }
        }
    }
    let listed = callable
        .iter()
        .filter(|tool| {
            options.mode == CodemodeMode::Only || tool.discovery.exposure != ToolExposure::Direct
        })
        .cloned()
        .collect::<Vec<_>>();
    descriptions.insert(CODEMODE_TOOL_NAME.into(), description(&listed, options));
    let hidden = if options.mode == CodemodeMode::Only {
        let declared_names = declared
            .iter()
            .map(|tool| tool.spec.name.as_str())
            .collect::<HashSet<_>>();
        callable
            .iter()
            .filter(|tool| {
                tool.discovery.exposure == ToolExposure::Direct
                    && declared_names.contains(tool.spec.name.as_str())
            })
            .map(|tool| tool.spec.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    (descriptions, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{ToolExecutionMode, ToolSpec};
    use pi_plugin::{ToolDiscovery, ToolNamespace};
    use serde_json::json;

    fn tool(name: &str, namespace: Option<&str>) -> ToolInfo {
        ToolInfo {
            spec: ToolSpec {
                name: name.into(),
                label: name.into(),
                description: format!("Run {name}."),
                parameters: json!({"type":"object","properties":{"path":{"type":"string","description":"Path to inspect."}},"required":["path"]}),
                output_schema: None,
                constrained_sampling: None,
                execution_mode: ToolExecutionMode::Parallel,
                prompt_snippet: None,
                prompt_guidelines: vec!["Read the instructions first.".into()],
            },
            discovery: ToolDiscovery {
                namespace: namespace.map(|name| ToolNamespace {
                    name: name.into(),
                    description: Some(format!("{name} tools")),
                    instructions: Some("Namespace instructions".into()),
                }),
                ..ToolDiscovery::default()
            },
        }
    }

    #[test]
    fn on_mode_augments_direct_calls_and_keeps_deferred_metadata_out_of_description() {
        let direct = tool("read", None);
        let mut deferred = tool("private-name", Some("server"));
        deferred.discovery.exposure = ToolExposure::Deferred;
        let (descriptions, hidden) = prepare_descriptions(
            std::slice::from_ref(&direct),
            &[direct.clone(), deferred],
            &CodemodeOptions::default(),
        );
        assert!(
            descriptions["read"].ends_with("Codemode: `tools.read(args)` resolves to a string.")
        );
        assert!(!descriptions[CODEMODE_TOOL_NAME].contains("private-name"));
        assert!(!descriptions[CODEMODE_TOOL_NAME].contains("Nested tools:"));
        assert_eq!(direct.spec.description, "Run read.");
        assert!(hidden.is_empty());
    }

    #[test]
    fn only_mode_hides_direct_tools_and_inlines_declarations_with_guidelines() {
        let tools = [tool("read", None), tool("search-files", Some("files"))];
        let options = CodemodeOptions {
            mode: CodemodeMode::Only,
            ..CodemodeOptions::default()
        };
        let (descriptions, hidden) = prepare_descriptions(&tools, &tools, &options);
        assert_eq!(hidden, ["read", "search-files"]);
        let description = &descriptions[CODEMODE_TOOL_NAME];
        assert!(description.contains("## files\nfiles tools"));
        assert!(description.contains("### `search_files` (`search-files`)"));
        assert!(description.contains("// Path to inspect."));
        assert!(description.contains("- Read the instructions first."));
        assert!(description.contains("Promise<string>"));
    }

    #[test]
    fn inline_budget_selects_one_small_tool_per_namespace_before_filling_it() {
        let first = tool("a", Some("alpha"));
        let second = tool("b", Some("alpha"));
        let third = tool("c", Some("bravo"));
        let cost = character_count(&format!("### `a`\n{}", sample(&first.spec))).div_ceil(4);
        let options = CodemodeOptions {
            mode: CodemodeMode::Only,
            inline_budget: cost * 2,
            ..CodemodeOptions::default()
        };
        let description = description(&[first, second, third], &options);
        assert!(description.contains("### `a`"));
        assert!(!description.contains("### `b`"));
        assert!(description.contains("### `c`"));
        assert!(description.contains("## alpha (some tools not listed)"));
    }
}
