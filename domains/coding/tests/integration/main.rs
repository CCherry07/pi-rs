// Keep the product integration suites in one binary so they share linking and
// process initialization, and libtest can schedule their isolated fixtures together.
mod codemode;
mod codemode_models;
mod features;
mod mcp_auth;
mod mcp_configuration;
mod mcp_deferred;
mod mcp_lifecycle;
mod mcp_notifications;
mod mcp_recovery;
mod mcp_reload;
mod mcp_resources;
mod mcp_test_support;
mod plugin_history_reload;
mod reload_state;
mod responses_history;
mod tool_result_images;
