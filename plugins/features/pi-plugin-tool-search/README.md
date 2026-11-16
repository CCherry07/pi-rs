# pi-plugin-tool-search

Generic deferred-tool discovery for Pi. `ToolSearchPlugin` registers `tool_search`;
`tool_search_tool()` supplies the same tool to hosts that compose their own plugin. Product
composition chooses when it is active. The crate has no MCP, Coding, or Session dependency.

The tool accepts a required `query` and an optional positive-integer `limit` (default 8). It
searches the current run's visible, inactive deferred-tool catalog, returns the matched names
in `details.loaded`, and requests activation through `ToolResult.added_tool_names`. The agent
loop applies those additions before the next model request; ordinary session selection
recording makes them available on resume and branch navigation. Search never edits a live
registry or reads session capabilities, so it also works in an ephemeral run.

`Bm25Ranker`, `create_tool_search_document`, and `tokenize` are reusable independently of the
tool. Ranking follows the current
[Pi implementation](../../../legacy/pi/packages/coding-agent/src/extensions/tool-search/tool.ts):
exact tool names first, then query-term coverage, then BM25 relevance. Tool names have weight 3,
namespace metadata has weight 0.5, and description/schema text is length-normalized. Equal scores
preserve catalog order. Only local schema references are followed; recursive/shared references
are read once and unused definitions are not indexed.

Text uses NFC normalization and ICU dictionary word segmentation, including Chinese, Japanese,
Thai and mixed-script identifiers. English stop words and simple singular forms match Pi.
Compiled ICU data makes segmentation independent of the host's installed locale libraries;
dictionary updates can still differ from the ICU version used by a particular Node release.

Run focused validation with `cargo test -p pi-plugin-tool-search`.
