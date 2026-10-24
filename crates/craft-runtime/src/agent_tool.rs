//! The `spawn_agent` tool: one child on one self-contained prompt,
//! awaited to completion. The result is the child's envelope —
//! status, stop reason, usage, transcript pointer, and its final
//! message; a failed child is a typed error result the parent run
//! survives.

use serde_json::Value;

use craft_tools::ToolError;
use craft_tools::event::{self, ToolStream};
use craft_tools::schema::{Prop, Schema};
use craft_tools::tool::{Tool, ToolCtx};

use crate::spawn;
use crate::spawn_args::plan_single;
use crate::subagent::SubagentWiring;

const DESCRIPTION: &str = "Runs one named subagent profile on a \
self-contained prompt to completion and returns its envelope: \
status, stop reason, turns, usage, transcript path, and the child's \
final message. The child has its own context, budget, and \
transcript; the parent sees only the envelope. Pass detached: true to \
return at admission and collect the envelope later with poll_agents.";

const PROPS: &[Prop] = &[
    Prop {
        name: "agent",
        ty: Schema::Str,
        required: true,
        desc: "Profile name from the agents catalog.",
    },
    Prop {
        name: "prompt",
        ty: Schema::Str,
        required: true,
        desc: "The child's whole context: one self-contained task.",
    },
    Prop {
        name: "maxTurns",
        ty: Schema::Int,
        required: false,
        desc: "Child turn cap, 1 to 10000; default is the profile's.",
    },
    Prop {
        name: "maxTokens",
        ty: Schema::Int,
        required: false,
        desc: "Child token slice; default is the remaining run budget.",
    },
    Prop {
        name: "timeoutSecs",
        ty: Schema::Int,
        required: false,
        desc: "Wall-clock cap in seconds, 0 (none) to 21600.",
    },
    Prop {
        name: "detached",
        ty: Schema::Bool,
        required: false,
        desc: "Return at admission without awaiting the child; collect its envelope with poll_agents.",
    },
];

pub(crate) struct SpawnAgentTool {
    wiring: SubagentWiring,
}

impl SpawnAgentTool {
    pub(crate) fn new(wiring: SubagentWiring) -> Self {
        Self { wiring }
    }
}

impl Tool for SpawnAgentTool {
    fn name(&self) -> &'static str {
        "spawn_agent"
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Runs one subagent to completion.", PROPS)
    }

    fn execute(&self, args: Value, _ctx: &ToolCtx) -> ToolStream {
        match plan_single(&self.wiring, &args) {
            Ok(request) if request.detached => crate::detached::drive(request, &self.wiring),
            Ok(request) => spawn::drive(request, &self.wiring),
            Err(msg) => event::error_stream(ToolError::InvalidArgs {
                tool: self.name().to_string(),
                msg,
            }),
        }
    }
}
