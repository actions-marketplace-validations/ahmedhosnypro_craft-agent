//! The `spawn_agents` tool: the batch form — one profile, one prompt
//! template, 2 to 32 items, members running in parallel up to the
//! concurrency cap with the rest queued FIFO in completion order.
//! The result is one envelope per member plus an explicit tally;
//! per-member failures never make the tool an error.

use serde_json::Value;

use craft_tools::ToolError;
use craft_tools::event::{self, ToolStream};
use craft_tools::schema::{Prop, Schema};
use craft_tools::tool::{Tool, ToolCtx};

use crate::spawn;
use crate::spawn_args::plan_batch;
use crate::subagent::SubagentWiring;

const DESCRIPTION: &str = "Runs a batch of 2 to 32 same-profile \
subagents in parallel, filling one prompt template per item, and \
returns one envelope per member plus a completed/failed/timed_out \
tally. Use it when several independent subtasks would otherwise \
flood this context.";

const PROPS: &[Prop] = &[
    Prop {
        name: "agent",
        ty: Schema::Str,
        required: true,
        desc: "Profile name from the agents catalog, shared by every member.",
    },
    Prop {
        name: "promptTemplate",
        ty: Schema::Str,
        required: true,
        desc: "Prompt template containing the exact placeholder {{item}}.",
    },
    Prop {
        name: "items",
        ty: Schema::StrList,
        required: true,
        desc: "2 to 32 items, one per member; filled prompts must be distinct.",
    },
    Prop {
        name: "maxTurns",
        ty: Schema::Int,
        required: false,
        desc: "Per-member turn cap, 1 to 10000; default is the profile's.",
    },
    Prop {
        name: "maxTokens",
        ty: Schema::Int,
        required: false,
        desc: "Per-member token budget; omitted means unlimited.",
    },
    Prop {
        name: "timeoutSecs",
        ty: Schema::Int,
        required: false,
        desc: "Per-member wall-clock cap in seconds, 0 (none) to 21600.",
    },
];

pub(crate) struct SpawnAgentsTool {
    wiring: SubagentWiring,
}

impl SpawnAgentsTool {
    pub(crate) fn new(wiring: SubagentWiring) -> Self {
        Self { wiring }
    }
}

impl Tool for SpawnAgentsTool {
    fn name(&self) -> &'static str {
        "spawn_agents"
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Runs a batch of subagents in parallel.", PROPS)
    }

    fn execute(&self, args: Value, _ctx: &ToolCtx) -> ToolStream {
        match plan_batch(&self.wiring, &args) {
            Ok(request) => spawn::drive(request, &self.wiring),
            Err(msg) => event::error_stream(ToolError::InvalidArgs {
                tool: self.name().to_string(),
                msg,
            }),
        }
    }
}
