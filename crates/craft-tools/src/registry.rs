//! The tool registry: builtins, cached wire specs, and the
//! contract-wrapped execute path the runtime drives.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::bash::BashTool;
use crate::edit::EditFileTool;
use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::gh::GhTool;
use crate::glob::GlobTool;
use crate::grep::GrepTool;
use crate::read::ReadFileTool;
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::write::WriteFileTool;

/// One tool's wire-facing declaration: name, description, and the JSON
/// schema for its arguments. Built once at registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// A run-time executor: arguments plus the per-call context into one
/// contracted tool stream.
pub type DynamicExecutor = Arc<dyn Fn(Value, &ToolCtx) -> ToolStream + Send + Sync>;

/// A tool registered at run time — MCP discovery — carrying its wire
/// schema verbatim and executing through a closure. Dispatch, the
/// stream contract, and the UnknownTool path apply to it unchanged.
pub struct DynamicTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub execute: DynamicExecutor,
}

pub struct Registry {
    tools: HashMap<String, Box<dyn Tool>>,
    dynamic: HashMap<String, DynamicTool>,
    specs: HashMap<String, ToolSpec>,
    limits: ToolLimits,
}

impl Registry {
    pub fn new(limits: ToolLimits) -> Self {
        Self {
            tools: HashMap::new(),
            dynamic: HashMap::new(),
            specs: HashMap::new(),
            limits,
        }
    }

    /// Register a tool; its wire spec is computed once and cached.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.cache_spec(
            name.clone(),
            tool.description().to_string(),
            tool.params_schema().to_value(),
        );
        self.tools.insert(name, tool);
    }

    /// Register a run-time tool; its wire spec is cached the same way
    /// a builtin's is.
    pub fn register_dynamic(&mut self, tool: DynamicTool) {
        let name = tool.name.clone();
        self.cache_spec(name.clone(), tool.description.clone(), tool.schema.clone());
        self.dynamic.insert(name, tool);
    }

    fn cache_spec(&mut self, name: String, description: String, schema: Value) {
        self.specs.insert(
            name.clone(),
            ToolSpec {
                name,
                description,
                schema,
            },
        );
    }

    /// The full builtin vocabulary under the given limits.
    pub fn with_builtins(limits: ToolLimits) -> Self {
        let mut registry = Self::new(limits);
        registry.register(Box::new(BashTool::new(limits)));
        registry.register(Box::new(ReadFileTool::new(limits)));
        registry.register(Box::new(WriteFileTool::new(limits)));
        registry.register(Box::new(EditFileTool::new(limits)));
        registry.register(Box::new(GlobTool::new(limits)));
        registry.register(Box::new(GrepTool::new(limits)));
        registry.register(Box::new(GhTool::new(limits)));
        registry
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .tools
            .keys()
            .chain(self.dynamic.keys())
            .map(|k| k.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.specs.get(name)
    }

    pub fn limits(&self) -> ToolLimits {
        self.limits
    }

    /// Execute a tool by name with the progress/terminal contract
    /// enforced; unknown names surface as a typed error in the stream.
    pub fn execute(&self, name: &str, args: Value, ctx: &ToolCtx) -> ToolStream {
        if let Some(tool) = self.tools.get(name) {
            return event::enforce_contract(tool.execute(args, ctx));
        }
        match self.dynamic.get(name) {
            Some(tool) => event::enforce_contract((tool.execute)(args, ctx)),
            None => event::error_stream(ToolError::UnknownTool {
                name: name.to_string(),
            }),
        }
    }
}
