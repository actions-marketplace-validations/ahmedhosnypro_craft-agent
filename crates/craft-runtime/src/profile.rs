//! The profile registry rule, shared by the CLI selection and the
//! subagent preparer so the two cannot drift: an absent `tools`
//! field inherits every registered tool, an allowlist keeps only its
//! members, and `disallowedTools` removes ids from whatever remains.
//! The spawn pair is a candidate only when a hub wiring is supplied
//! and the depth gate admits one more level.

use craft_core::agents::AgentFile;
use craft_tools::bash::BashTool;
use craft_tools::edit::EditFileTool;
use craft_tools::gh::GhTool;
use craft_tools::glob::GlobTool;
use craft_tools::grep::GrepTool;
use craft_tools::read::ReadFileTool;
use craft_tools::registry::Registry;
use craft_tools::tool::{Tool, ToolLimits};
use craft_tools::write::WriteFileTool;

use crate::agent_tool::SpawnAgentTool;
use crate::agents_tool::SpawnAgentsTool;
use crate::polls_tool::PollAgentsTool;
use crate::subagent::SubagentWiring;

/// The registry for the selected (or absent) profile. `spawn` carries
/// the hub wiring the pair needs; `None` builds a registry without
/// the spawn pair at all.
pub fn build_registry(
    agent: Option<&AgentFile>,
    limits: ToolLimits,
    spawn: Option<SubagentWiring>,
) -> Registry {
    let Some(agent) = agent else {
        return with_spawn(Registry::with_builtins(limits), spawn);
    };
    let allow = agent.tools.as_deref();
    let deny = agent.disallowed_tools.as_slice();
    if allow.is_none() && deny.is_empty() {
        return with_spawn(Registry::with_builtins(limits), spawn);
    }
    let mut registry = Registry::new(limits);
    let spawn_ids: &[&str] = if spawn.is_some() { &SPAWN_IDS } else { &[] };
    for id in BUILTIN_IDS.iter().chain(spawn_ids) {
        let allowed = allow.is_none_or(|list| list.iter().any(|tool| tool == id));
        let denied = deny.iter().any(|tool| tool == id);
        if allowed && !denied {
            register_one(&mut registry, id, limits, spawn.clone());
        }
    }
    registry
}

/// The runtime's builtin vocabulary; the agent-file gate's id list
/// grows in lockstep with it.
const BUILTIN_IDS: [&str; 7] = [
    "bash",
    "read_file",
    "write_file",
    "edit_file",
    "glob",
    "grep",
    "gh",
];
const SPAWN_IDS: [&str; 3] = ["spawn_agent", "spawn_agents", "poll_agents"];

fn with_spawn(mut registry: Registry, spawn: Option<SubagentWiring>) -> Registry {
    if let Some(wiring) = spawn {
        registry.register(Box::new(SpawnAgentTool::new(wiring.clone())));
        registry.register(Box::new(SpawnAgentsTool::new(wiring.clone())));
        registry.register(Box::new(PollAgentsTool::new(wiring)));
    }
    registry
}

fn register_one(
    registry: &mut Registry,
    id: &str,
    limits: ToolLimits,
    spawn: Option<SubagentWiring>,
) {
    let tool: Box<dyn Tool> = match id {
        "bash" => Box::new(BashTool::new(limits)),
        "read_file" => Box::new(ReadFileTool::new(limits)),
        "write_file" => Box::new(WriteFileTool::new(limits)),
        "edit_file" => Box::new(EditFileTool::new(limits)),
        "glob" => Box::new(GlobTool::new(limits)),
        "grep" => Box::new(GrepTool::new(limits)),
        "gh" => Box::new(GhTool::new(limits)),
        "spawn_agent" => match spawn {
            Some(wiring) => Box::new(SpawnAgentTool::new(wiring)),
            None => return,
        },
        "spawn_agents" => match spawn {
            Some(wiring) => Box::new(SpawnAgentsTool::new(wiring)),
            None => return,
        },
        "poll_agents" => match spawn {
            Some(wiring) => Box::new(PollAgentsTool::new(wiring)),
            None => return,
        },
        _ => return,
    };
    registry.register(tool);
}
