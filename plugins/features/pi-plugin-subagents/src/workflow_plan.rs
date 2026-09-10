//! Compile the small public workflow grammar into a validated, static DAG.
use std::collections::{HashMap, HashSet};

use pi_core::ToolError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::SubagentCatalog;

pub(crate) const MAX_NODES: usize = 64;
pub(crate) const MAX_INPUT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_HANDOFF_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    stages: Vec<Stage>,
    context: Option<pi_core::IsolatedContextMode>,
    #[serde(default = "default_parallelism")]
    max_parallelism: usize,
    #[serde(default, rename = "async")]
    background: bool,
}

fn default_parallelism() -> usize {
    3
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    key: String,
    run: Option<Node>,
    all: Option<Vec<Node>>,
    lanes: Option<Vec<Lane>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lane {
    key: String,
    steps: Vec<Node>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    key: Option<String>,
    agent: String,
    task: String,
    context: Option<pi_core::IsolatedContextMode>,
    #[serde(default)]
    inputs: Vec<Handoff>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Handoff {
    pub from: String,
    #[serde(rename = "as")]
    pub alias: String,
}

pub(crate) struct PlannedNode {
    pub key: String,
    pub agent: String,
    pub task: String,
    pub context: Option<pi_core::IsolatedContextMode>,
    pub dependencies: Vec<usize>,
    pub inputs: Vec<Handoff>,
}

pub(crate) struct WorkflowPlan {
    pub nodes: Vec<PlannedNode>,
    pub max_parallelism: usize,
    pub background: bool,
    pub context: Option<pi_core::IsolatedContextMode>,
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments(message.into())
}

fn key(value: &str) -> Result<(), ToolError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err(invalid(
            "Workflow keys and input aliases must be 1–64 ASCII letters, digits, '_' or '-'.",
        ));
    }
    Ok(())
}

impl WorkflowPlan {
    pub fn parse(input: Value, catalog: &SubagentCatalog) -> Result<Self, ToolError> {
        if input.to_string().len() > 256 * 1024 {
            return Err(invalid("Workflow exceeds the 256 KiB submission limit."));
        }
        let input: Input = serde_json::from_value(input).map_err(|e| invalid(e.to_string()))?;
        if !(1..=20).contains(&input.max_parallelism) {
            return Err(invalid("maxParallelism must be between 1 and 20."));
        }
        if input.stages.is_empty() || input.stages.len() > MAX_NODES {
            return Err(invalid("Workflow requires 1–64 nonempty stages."));
        }
        let mut nodes = Vec::new();
        let mut previous = Vec::new();
        let mut stage_keys = HashSet::new();
        for stage in input.stages {
            key(&stage.key)?;
            if !stage_keys.insert(stage.key.clone()) {
                return Err(invalid(format!("Duplicate stage key {:?}.", stage.key)));
            }
            let mut tails = Vec::new();
            match (stage.run, stage.all, stage.lanes) {
                (Some(node), None, None) => {
                    if node.key.is_some() {
                        return Err(invalid(
                            "A run uses its stage key; do not supply a node key.",
                        ));
                    }
                    tails.push(push(
                        &mut nodes,
                        stage.key,
                        node,
                        previous.clone(),
                        catalog,
                    )?);
                }
                (None, Some(group), None) if !group.is_empty() => {
                    for node in group {
                        let path = keyed_path(&stage.key, &node)?;
                        tails.push(push(&mut nodes, path, node, previous.clone(), catalog)?);
                    }
                }
                (None, None, Some(lanes)) if !lanes.is_empty() => {
                    let mut lane_keys = HashSet::new();
                    for lane in lanes {
                        key(&lane.key)?;
                        if !lane_keys.insert(lane.key.clone()) || lane.steps.is_empty() {
                            return Err(invalid(
                                "Lanes must be nonempty and have unique keys within the stage.",
                            ));
                        }
                        let mut deps = previous.clone();
                        for node in lane.steps {
                            let path = keyed_path(&format!("{}/{}", stage.key, lane.key), &node)?;
                            deps = vec![push(&mut nodes, path, node, deps, catalog)?];
                        }
                        tails.extend(deps);
                    }
                }
                _ => {
                    return Err(invalid(
                        "Each stage requires exactly one nonempty run, all, or lanes.",
                    ));
                }
            }
            previous = tails;
        }
        let mut indices = HashMap::new();
        let mut ancestors: Vec<HashSet<usize>> = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            if indices.insert(node.key.as_str(), index).is_some() {
                return Err(invalid(format!("Duplicate node key {:?}.", node.key)));
            }
            let mut reachable = HashSet::new();
            for dep in &node.dependencies {
                reachable.insert(*dep);
                reachable.extend(ancestors[*dep].iter().copied());
            }
            let mut aliases = HashSet::new();
            for input in &node.inputs {
                key(&input.alias)?;
                if !aliases.insert(&input.alias) {
                    return Err(invalid(format!(
                        "Duplicate input alias {:?} on {}.",
                        input.alias, node.key
                    )));
                }
                if !indices
                    .get(input.from.as_str())
                    .is_some_and(|dep| reachable.contains(dep))
                {
                    return Err(invalid(format!(
                        "Input {:?} on {} must reference a control-flow ancestor; forward and parallel-sibling references are not allowed.",
                        input.from, node.key
                    )));
                }
            }
            ancestors.push(reachable);
        }
        Ok(Self {
            max_parallelism: input.max_parallelism.min(nodes.len()),
            nodes,
            background: input.background,
            context: input.context,
        })
    }
}

fn keyed_path(prefix: &str, node: &Node) -> Result<String, ToolError> {
    let name = node
        .key
        .as_deref()
        .ok_or_else(|| invalid("Nodes in all and lanes require a key."))?;
    key(name)?;
    Ok(format!("{prefix}/{name}"))
}

fn push(
    nodes: &mut Vec<PlannedNode>,
    path: String,
    node: Node,
    dependencies: Vec<usize>,
    catalog: &SubagentCatalog,
) -> Result<usize, ToolError> {
    if nodes.len() == MAX_NODES {
        return Err(invalid("Workflow exceeds the 64-node limit."));
    }
    let agent = node.agent.trim();
    if catalog.profile(agent).is_none() {
        return Err(invalid(format!("Unknown subagent profile {agent:?}.")));
    }
    let task = node.task.trim();
    if task.is_empty() || task.len() > 64 * 1024 {
        return Err(invalid("Node task must be nonempty and at most 64 KiB."));
    }
    let index = nodes.len();
    nodes.push(PlannedNode {
        key: path,
        agent: agent.into(),
        task: task.into(),
        context: node.context,
        dependencies,
        inputs: node.inputs,
    });
    Ok(index)
}

pub(crate) fn schema(catalog: &SubagentCatalog) -> Value {
    let properties = json!({
        "agent":{"type":"string","enum":catalog.profile_names()},
        "task":{"type":"string","minLength":1},
        "context":{"type":"string","enum":["fresh","fork"],"description":"Node context override when the workflow has no explicit context. Omit for the role default."},
        "inputs":{"type":"array","items":{"type":"object","properties":{
            "from":{"type":"string","description":"Ancestor node key: stage, stage/node, or stage/lane/step."},
            "as":{"type":"string"}},"required":["from","as"],"additionalProperties":false}}
    });
    let single = json!({"type":"object","properties":properties,"required":["agent","task"],"additionalProperties":false});
    let mut keyed = single.clone();
    keyed["properties"]["key"] = json!({"type":"string"});
    keyed["required"] = json!(["key", "agent", "task"]);
    json!({"type":"object","properties":{
        "stages":{"type":"array","minItems":1,"maxItems":64,"items":{
            "type":"object","properties":{
                "key":{"type":"string"},"run":single,
                "all":{"type":"array","minItems":1,"items":keyed},
                "lanes":{"type":"array","minItems":1,"items":{"type":"object","properties":{
                    "key":{"type":"string"},"steps":{"type":"array","minItems":1,"items":keyed}
                },"required":["key","steps"],"additionalProperties":false}}
            },"required":["key"],"oneOf":[{"required":["run"]},{"required":["all"]},{"required":["lanes"]}],"additionalProperties":false}},
        "maxParallelism":{"type":"integer","minimum":1,"maximum":20,"default":3},
        "context":{"type":"string","enum":["fresh","fork"],"description":"Override every node's context. Omit for each node's explicit context or role default. Fork nodes share the parent's pre-submission fork point."},
        "async":{"type":"boolean","default":false}
    },"required":["stages"],"additionalProperties":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run() -> Value {
        json!({"agent":"reviewer","task":"Inspect"})
    }
    #[test]
    fn compiles_stage_barriers_and_lane_edges() {
        let plan = WorkflowPlan::parse(json!({"stages":[
            {"key":"a","run":run()},
            {"key":"b","lanes":[{"key":"x","steps":[
                {"key":"one","agent":"reviewer","task":"One"},
                {"key":"two","agent":"reviewer","task":"Two","inputs":[{"from":"a","as":"source"}]}]},
                {"key":"y","steps":[{"key":"one","agent":"reviewer","task":"Other"}]}]},
            {"key":"c","run":run()}]}), &SubagentCatalog::builtins()).unwrap();
        assert_eq!(
            plan.nodes
                .iter()
                .map(|n| n.dependencies.clone())
                .collect::<Vec<_>>(),
            vec![vec![], vec![0], vec![1], vec![0], vec![2, 3]]
        );
        assert_eq!(plan.nodes[2].key, "b/x/two");
    }
    #[test]
    fn rejects_ambiguous_graphs_and_unsupported_context() {
        for input in [
            json!({"stages":[]}),
            json!({"stages":[{"key":"a","run":run(),"all":[run()]}]}),
            json!({"stages":[{"key":"a","all":[]}]}),
            json!({"stages":[{"key":"a","run":run()},{"key":"a","run":run()}]}),
            json!({"stages":[{"key":"a","run":{"agent":"reviewer","task":"x","context":"resume"}}]}),
            json!({"stages":[{"key":"a","run":{"agent":"reviewer","task":"x","inputs":[{"from":"b","as":"x"}]}},{"key":"b","run":run()}]}),
            json!({"stages":[{"key":"a","all":[{"key":"x","agent":"reviewer","task":"x"},{"key":"y","agent":"reviewer","task":"y","inputs":[{"from":"a/x","as":"x"}]}]}]}),
            json!({"stages":[{"key":"a","run":run()}],"maxParallelism":0}),
        ] {
            assert!(
                WorkflowPlan::parse(input.clone(), &SubagentCatalog::builtins()).is_err(),
                "{input}"
            );
        }
    }
}
