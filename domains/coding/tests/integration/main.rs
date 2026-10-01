// Keep the product integration suites in one binary so they share linking and
// process initialization, and libtest can schedule their isolated fixtures together.
mod features;
mod mcp_configuration;
mod mcp_reload;
mod plugin_history_reload;
mod reload_state;
