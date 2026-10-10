//! The subagent hub: one per run, owned by the parent's tool pair and
//! the runner. It keeps the shared token ledger (parent spend,
//! settled child spend, in-flight reserves), resolves profiles from
//! the loaded catalog, allocates child ids, and hands prepared child
//! runs to the spawn tools. Admission and settle live beside it in
//! `subagent_admit`; child preparation in `child`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_core::agents::{AgentCatalog, AgentFile, CatalogHit};
use craft_core::types::Usage;
use craft_tools::ToolLimits;

use crate::child::{ChildFactory, ChildFuture, ChildRun};
use crate::envelope::LISTING_CAP;
use crate::events::RuntimeEvent;

/// Depth of subagents a run may admit; 0 disables spawning entirely.
pub const ENV_MAX_DEPTH: &str = "CRAFT_AGENT_SUBAGENT_MAX_DEPTH";
/// Total subagents one run may admit (the id-churn bound).
pub const ENV_MAX_TOTAL: &str = "CRAFT_AGENT_SUBAGENT_MAX_TOTAL";
/// Subagents that may run at once, tree-wide.
pub const ENV_MAX_CONCURRENCY: &str = "CRAFT_AGENT_SUBAGENT_MAX_CONCURRENCY";

/// Run-wide spawn caps; defaults 2 / 32 / 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentCaps {
    pub max_depth: u8,
    pub max_total: u32,
    pub max_concurrency: u32,
}

impl Default for SubagentCaps {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_total: 32,
            max_concurrency: 4,
        }
    }
}

impl SubagentCaps {
    /// Reads the three cap knobs from the process environment.
    pub fn from_env() -> Result<Self, String> {
        Self::resolve(
            std::env::var(ENV_MAX_DEPTH).ok(),
            std::env::var(ENV_MAX_TOTAL).ok(),
            std::env::var(ENV_MAX_CONCURRENCY).ok(),
        )
    }

    /// Resolves raw knob values; a missing knob keeps its default, a
    /// hostile value is a typed error naming the knob and its range.
    pub fn resolve(
        depth: Option<String>,
        total: Option<String>,
        concurrency: Option<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            max_depth: u8::try_from(parse_cap(CapSpec::DEPTH, depth)?)
                .map_err(|_| CapSpec::DEPTH.range_error())?,
            max_total: parse_cap(CapSpec::TOTAL, total)?,
            max_concurrency: parse_cap(CapSpec::CONCURRENCY, concurrency)?,
        })
    }
}

/// One cap knob: its environment name plus its default and bounds.
struct CapSpec {
    knob: &'static str,
    default: u64,
    min: u64,
    max: u64,
}

impl CapSpec {
    const DEPTH: Self = Self {
        knob: ENV_MAX_DEPTH,
        default: 2,
        min: 0,
        max: 4,
    };
    const TOTAL: Self = Self {
        knob: ENV_MAX_TOTAL,
        default: 32,
        min: 1,
        max: 256,
    };
    const CONCURRENCY: Self = Self {
        knob: ENV_MAX_CONCURRENCY,
        default: 4,
        min: 1,
        max: 16,
    };

    fn range_error(&self) -> String {
        format!(
            "{} must be a whole number between {} and {}",
            self.knob, self.min, self.max
        )
    }
}

fn parse_cap(spec: CapSpec, raw: Option<String>) -> Result<u32, String> {
    let Some(raw) = raw else {
        return Ok(u32::try_from(spec.default).unwrap_or_default());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(spec.range_error());
    }
    let value: u64 = trimmed
        .parse()
        .map_err(|_| format!("{}; got `{trimmed}`", spec.range_error()))?;
    if value < spec.min || value > spec.max {
        return Err(format!("{}; got `{trimmed}`", spec.range_error()));
    }
    Ok(u32::try_from(value).unwrap_or_default())
}

/// The fixed inputs every child inherits: caps, the parent run's
/// budget shape, the base prompt extend-mode bodies append to, the
/// loaded profile catalog, and the parent's session path (children
/// live under its stem).
#[derive(Debug, Clone)]
pub struct SubagentSettings {
    pub caps: SubagentCaps,
    pub run_token_cap: u64,
    pub per_request_tokens: u32,
    pub default_system_prompt: String,
    pub catalog: Arc<AgentCatalog>,
    pub limits: ToolLimits,
    pub workspace_root: PathBuf,
    pub parent_session: Option<PathBuf>,
    pub idle_timeout: Option<Duration>,
    pub step_attempts: Option<u32>,
}

/// One runner's view of the hub. Only the root wiring publishes
/// spend and drains events; child wirings exist so their registries
/// can carry the spawn pair with an honest depth the model cannot
/// influence.
#[derive(Debug, Clone)]
pub struct SubagentWiring {
    pub hub: Arc<SubagentHub>,
    pub depth: u8,
}

impl SubagentWiring {
    pub fn root(hub: Arc<SubagentHub>) -> Self {
        Self { hub, depth: 0 }
    }

    pub fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Root only: publish the parent's running total at the
    /// turn-usage seam; children never touch the run ledger.
    pub(crate) fn publish_parent_turn(&self, total: Usage) {
        if self.is_root() {
            self.hub.publish_parent(total);
        }
    }

    /// Root only, after a turn's tool batch: fold the settled child
    /// usage into the run total and drain the queued envelope events
    /// into the parent sink, stamped with the parent turn.
    pub(crate) fn after_tool_batch(
        &self,
        turn: u32,
        sink: &mut (dyn FnMut(RuntimeEvent) + Send),
    ) -> Usage {
        if !self.is_root() {
            return crate::budget::zero_usage();
        }
        let drained = self.hub.take_child_usage();
        for event in self.hub.take_events(turn) {
            sink(event);
        }
        drained
    }
}

/// The run-wide hub: id allocation, the shared ledger, the profile
/// catalog view, and the erased child-runner factory.
pub struct SubagentHub {
    settings: SubagentSettings,
    pub(crate) state: Mutex<HubState>,
    pub(crate) factory: Mutex<Option<ChildFactory>>,
}

pub(crate) struct HubState {
    pub(crate) parent_spent: Usage,
    pub(crate) child_settled: Usage,
    pub(crate) in_flight: u64,
    pub(crate) running: u32,
    pub(crate) spawned_total: u32,
    pub(crate) next_id: u32,
    pub(crate) events: std::collections::VecDeque<crate::subagent_admit::HubEvent>,
    pub(crate) admitted: HashMap<String, AdmittedChild>,
    pub(crate) settled: std::collections::HashSet<String>,
    pub(crate) child_usage: HashMap<String, Usage>,
    pub(crate) child_turn: HashMap<String, u32>,
}

pub(crate) struct AdmittedChild {
    pub(crate) slice: u64,
    pub(crate) profile: String,
}

/// Token total across every usage bucket, matching the budget
/// guard's spend.
pub(crate) fn tokens(usage: &Usage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens)
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.reasoning_tokens)
}

impl Default for HubState {
    fn default() -> Self {
        Self {
            parent_spent: crate::budget::zero_usage(),
            child_settled: crate::budget::zero_usage(),
            in_flight: 0,
            running: 0,
            spawned_total: 0,
            next_id: 0,
            events: std::collections::VecDeque::new(),
            admitted: HashMap::new(),
            settled: std::collections::HashSet::new(),
            child_usage: HashMap::new(),
            child_turn: HashMap::new(),
        }
    }
}

impl std::fmt::Debug for SubagentHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentHub")
            .field("settings", &self.settings)
            .finish()
    }
}

impl SubagentHub {
    /// Builds the hub with the provider-mounted child factory: every
    /// child runs a full `Runner` over the same backend the parent
    /// drives.
    pub fn mounting<P: craft_core::provider::Provider + Send + Sync + 'static>(
        settings: SubagentSettings,
        provider: Arc<P>,
    ) -> Arc<Self> {
        let hub = Arc::new(Self {
            settings,
            state: Mutex::new(HubState::default()),
            factory: Mutex::new(None),
        });
        let factory = crate::child::runner_factory(provider, hub.clone());
        hub.factory
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .replace(factory);
        hub
    }

    pub fn settings(&self) -> &SubagentSettings {
        &self.settings
    }

    pub fn caps(&self) -> SubagentCaps {
        self.settings.caps
    }

    pub(crate) fn child_future(&self, child: ChildRun, prompt: String) -> Option<ChildFuture> {
        self.factory
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .as_ref()
            .map(|factory| factory(child, prompt))
    }

    /// Resolves a profile from the loaded catalog; an unknown or
    /// invalid name is a typed error listing what the catalog offers.
    pub fn profile(&self, name: &str) -> Result<AgentFile, String> {
        match self.settings.catalog.lookup(name) {
            CatalogHit::Found(agent) => Ok(agent.clone()),
            CatalogHit::Invalid(error) => Err(format!(
                "agent profile `{name}` is invalid: {error}; available: {}",
                self.listing()
            )),
            CatalogHit::Missing => Err(format!(
                "unknown agent profile `{name}`; available: {}",
                self.listing()
            )),
        }
    }

    fn listing(&self) -> String {
        let mut parts: Vec<String> = self
            .settings
            .catalog
            .agents
            .iter()
            .map(|agent| format!("{} ({})", agent.name, agent.description))
            .collect();
        if parts.is_empty() {
            return String::from("(none found)");
        }
        parts.sort();
        crate::envelope::cap_chars(&parts.join("; "), LISTING_CAP)
    }

    pub(crate) fn lock_state(&self) -> std::sync::MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Settled-but-undrained child spend plus in-flight reserves.
    pub(crate) fn committed(state: &HubState) -> u64 {
        tokens(&state.child_settled).saturating_add(state.in_flight)
    }

    /// The honest remaining run budget at this moment.
    pub fn budget_remaining(&self) -> u64 {
        let state = self.lock_state();
        self.settings
            .run_token_cap
            .saturating_sub(tokens(&state.parent_spent))
            .saturating_sub(Self::committed(&state))
    }

    /// The default per-member slice when `maxTokens` is absent: the
    /// remaining run budget, shared evenly by a batch.
    pub fn default_slice(&self, count: u32) -> u64 {
        let remaining = self.budget_remaining();
        if count > 1 {
            remaining / u64::from(count)
        } else {
            remaining
        }
    }

    pub fn publish_parent(&self, total: Usage) {
        self.lock_state().parent_spent = total;
    }

    /// Drains settled child usage into the runner's total; each
    /// child's spend is folded exactly once. The folded figures are
    /// complete per-turn reports, so a timed-out child's
    /// unverifiable marker — kept on its own event and envelope —
    /// never fail-closes the parent run.
    pub fn take_child_usage(&self) -> Usage {
        let mut state = self.lock_state();
        let mut drained = std::mem::replace(&mut state.child_settled, crate::budget::zero_usage());
        drained.incomplete = false;
        drained
    }
}

impl SubagentHub {
    /// Queued envelope events stamped with the parent turn that
    /// executed the spawn calls.
    pub fn take_events(&self, turn: u32) -> Vec<RuntimeEvent> {
        let events: Vec<crate::subagent_admit::HubEvent> =
            self.lock_state().events.drain(..).collect();
        events
            .into_iter()
            .map(|event| event.stamped(turn))
            .collect()
    }

    /// A pending child future, or `None` when no factory is mounted —
    /// a typed spawn error the caller reports.
    pub fn spawn_future(&self, child: ChildRun, prompt: String) -> Result<ChildFuture, String> {
        self.child_future(child, prompt)
            .ok_or_else(|| String::from("the subagent backend is not mounted"))
    }
}
