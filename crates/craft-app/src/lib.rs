//! craft-agent library surface: the CLI logic the binary and the test
//! corpora share. `main.rs` stays a thin parser-and-dispatch shell.

pub mod agent_select;
pub mod cli;
pub mod config;
pub mod digest_cmd;
pub mod error;
pub mod event_writer;
pub mod prompt;
pub(crate) mod redaction;
pub mod run_cmd;
pub mod stream_json;
pub mod subagent_setup;
pub mod summary;
pub mod workflow_cmd;
pub mod workflow_exec;
pub mod workflow_locate;

pub use craft_core::types::Usage;
pub use craft_runtime::{CancelCategory, ExitStatus, RunOutcome, RuntimeEvent, ToolStreamName};

pub use error::AppError;
