//! The append-only NDJSON journal: dense seq, torn-tail repair, hostile-file
//! refusal, and caps enforced at both load and record.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WorkflowError;
use crate::plan::Limits;
use crate::status::{RunConclusion, SkipReason, StepConclusion, StepUsage};

pub const JOURNAL_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const JOURNAL_MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEvent {
    RunStarted {
        seq: u64,
        run_id: String,
        name: String,
        plan_hash: String,
        args_digest: String,
        limits: Limits,
    },
    StepStarted {
        seq: u64,
        id: String,
    },
    /// A failed attempt that will be retried; keeps the retry budget
    /// resumable across crashes.
    StepRetried {
        seq: u64,
        id: String,
        attempt: u32,
    },
    StepFinished {
        seq: u64,
        id: String,
        input_hash: String,
        conclusion: StepConclusion,
        usage: StepUsage,
        final_message: String,
    },
    StepSkipped {
        seq: u64,
        id: String,
        reason: SkipReason,
    },
    RunFinished {
        seq: u64,
        conclusion: RunConclusion,
        failed_step: Option<String>,
        total_usage: StepUsage,
    },
}

impl JournalEvent {
    pub fn seq(&self) -> u64 {
        match self {
            Self::RunStarted { seq, .. }
            | Self::StepStarted { seq, .. }
            | Self::StepRetried { seq, .. }
            | Self::StepFinished { seq, .. }
            | Self::StepSkipped { seq, .. }
            | Self::RunFinished { seq, .. } => *seq,
        }
    }
}

#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    entries: Vec<JournalEvent>,
    bytes: u64,
    max_bytes: u64,
    max_entries: usize,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self, WorkflowError> {
        Self::open_bounded(path, JOURNAL_MAX_BYTES, JOURNAL_MAX_ENTRIES)
    }

    /// Load-or-fresh with explicit caps; every failure is typed.
    pub fn open_bounded(
        path: &Path,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<Self, WorkflowError> {
        let shown = path.to_string_lossy().into_owned();
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    entries: Vec::new(),
                    bytes: 0,
                    max_bytes,
                    max_entries,
                });
            }
            Err(err) => {
                return Err(WorkflowError::JournalIo {
                    path: shown,
                    detail: err.to_string(),
                });
            }
        };
        if meta.file_type().is_symlink() {
            return Err(WorkflowError::JournalUnsafe {
                path: shown,
                reason: "is a symlink".to_string(),
            });
        }
        if !meta.is_file() {
            return Err(WorkflowError::JournalUnsafe {
                path: shown,
                reason: "is not a regular file".to_string(),
            });
        }
        if meta.len() > max_bytes {
            return Err(WorkflowError::JournalCap {
                path: shown,
                limit: max_bytes,
            });
        }
        let mut text = String::new();
        let handle = std::fs::File::open(path).map_err(|err| WorkflowError::JournalIo {
            path: shown.clone(),
            detail: err.to_string(),
        })?;
        handle
            .take(max_bytes + 1)
            .read_to_string(&mut text)
            .map_err(|err| WorkflowError::JournalIo {
                path: shown.clone(),
                detail: err.to_string(),
            })?;
        if text.len() as u64 > max_bytes {
            return Err(WorkflowError::JournalCap {
                path: shown,
                limit: max_bytes,
            });
        }
        Self::scan(path, text, max_bytes, max_entries)
    }
}

impl Journal {
    fn scan(
        path: &Path,
        text: String,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<Self, WorkflowError> {
        let mut journal = Self {
            path: path.to_path_buf(),
            entries: Vec::new(),
            bytes: 0,
            max_bytes,
            max_entries,
        };
        journal.load(text)?;
        Ok(journal)
    }

    /// Loads complete NDJSON lines; the trailing fragment goes to repair.
    fn load(&mut self, text: String) -> Result<(), WorkflowError> {
        let bytes = text.as_bytes();
        let mut pos = 0usize;
        let mut line_no: u64 = 0;
        loop {
            let Some(rel) = bytes[pos..].iter().position(|&b| b == b'\n') else {
                return self.heal_tail(&text[pos..], pos as u64);
            };
            let end = pos + rel;
            line_no += 1;
            let event: JournalEvent =
                serde_json::from_str(text[pos..end].trim()).map_err(|err| {
                    WorkflowError::JournalParse {
                        path: self.path_str(),
                        line: line_no,
                        detail: err.to_string(),
                    }
                })?;
            self.push(event)?;
            pos = end + 1;
            self.bytes = pos as u64;
        }
    }

    /// Torn-tail repair: a valid final line is kept and re-terminated
    /// with a newline; any other trailing damage is dropped at `pos`.
    fn heal_tail(&mut self, chunk: &str, pos: u64) -> Result<(), WorkflowError> {
        let Some(event) = Self::parsed_tail(chunk, self.entries.len() as u64) else {
            if !chunk.is_empty() {
                self.truncate_to(pos)?;
            }
            self.bytes = pos;
            return Ok(());
        };
        self.entry_cap()?;
        self.append_bytes(b"\n")?;
        self.bytes = pos.saturating_add(chunk.len() as u64).saturating_add(1);
        self.entries.push(event);
        Ok(())
    }

    /// The torn final line, when it parses with the next dense seq.
    fn parsed_tail(chunk: &str, expected: u64) -> Option<JournalEvent> {
        if chunk.trim().is_empty() {
            return None;
        }
        let event = serde_json::from_str::<JournalEvent>(chunk.trim()).ok()?;
        (event.seq() == expected).then_some(event)
    }

    /// Admits a replayed line: dense seq first, then the entry cap.
    fn push(&mut self, event: JournalEvent) -> Result<(), WorkflowError> {
        if event.seq() != self.entries.len() as u64 {
            return Err(WorkflowError::JournalSequence {
                path: self.path_str(),
                expected: self.entries.len() as u64,
                actual: event.seq(),
            });
        }
        self.entry_cap()?;
        self.entries.push(event);
        Ok(())
    }

    fn entry_cap(&self) -> Result<(), WorkflowError> {
        if self.entries.len() >= self.max_entries {
            return Err(WorkflowError::JournalCap {
                path: self.path_str(),
                limit: self.max_entries as u64,
            });
        }
        Ok(())
    }

    fn io_err(&self, err: impl std::fmt::Display) -> WorkflowError {
        WorkflowError::JournalIo {
            path: self.path_str(),
            detail: err.to_string(),
        }
    }

    fn append_bytes(&self, tail: &[u8]) -> Result<(), WorkflowError> {
        let mut handle = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(|err| self.io_err(err))?;
        handle.write_all(tail).map_err(|err| self.io_err(err))
    }

    /// Torn-tail repair on disk: keep the valid prefix, drop the damage.
    fn truncate_to(&self, offset: u64) -> Result<(), WorkflowError> {
        let handle = OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|err| self.io_err(err))?;
        handle.set_len(offset).map_err(|err| self.io_err(err))
    }

    pub fn next_seq(&self) -> u64 {
        self.entries.len() as u64
    }

    pub fn record(&mut self, event: JournalEvent) -> Result<u64, WorkflowError> {
        let shown = self.path.to_string_lossy().into_owned();
        let expected = self.entries.len() as u64;
        if event.seq() != expected {
            return Err(WorkflowError::JournalSequence {
                path: shown,
                expected,
                actual: event.seq(),
            });
        }
        if self.entries.len() >= self.max_entries {
            return Err(WorkflowError::JournalFull {
                path: shown,
                seq: expected,
                limit: self.max_entries,
            });
        }
        let mut line = serde_json::to_string(&event).map_err(|err| self.io_err(err))?;
        line.push('\n');
        if self.bytes + line.len() as u64 > self.max_bytes {
            return Err(WorkflowError::JournalCap {
                path: shown,
                limit: self.max_bytes,
            });
        }
        self.append_bytes(line.as_bytes())?;
        let seq = event.seq();
        self.entries.push(event);
        self.bytes += line.len() as u64;
        Ok(seq)
    }
}

impl Journal {
    pub fn entries(&self) -> &[JournalEvent] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}
