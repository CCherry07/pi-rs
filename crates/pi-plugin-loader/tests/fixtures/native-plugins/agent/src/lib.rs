use pi_plugin_sdk::agent::prelude::*;

#[derive(Default)]
pub struct FixtureAgentPlugin;

#[pi_plugin_sdk::agent]
impl AgentPlugin for FixtureAgentPlugin {}
