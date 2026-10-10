//! The buffered-terminal lane driver shared by the decode lanes: one
//! SSE reader fold that buffers the terminal, merges usage, drains
//! pending deltas, and flushes exactly one TurnFinished when the wire
//! ends — each dialect supplies only the per-event decode.

use std::collections::VecDeque;
use std::pin::Pin;

use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::{ModelEventStream, boxed};
use craft_core::sse::{SseError, SseEvent, SseOptions, SseReader};
use craft_core::types::{StopReason, Usage};
use futures::{Stream, StreamExt, stream};

use crate::transport;

type Reader = Pin<Box<dyn Stream<Item = Result<SseEvent, SseError<reqwest::Error>>> + Send>>;

/// The state every decode lane shares: pending model events, the
/// buffered terminal, and the merged usage.
#[derive(Debug, Default)]
pub(crate) struct LaneCommon {
    pending: VecDeque<ModelEvent>,
    terminal: Option<StopReason>,
    usage: Option<Usage>,
}

impl LaneCommon {
    pub(crate) fn pop_pending(&mut self) -> Option<ModelEvent> {
        self.pending.pop_front()
    }

    pub(crate) fn push_pending(&mut self, event: ModelEvent) {
        self.pending.push_back(event);
    }

    pub(crate) fn terminal(&self) -> Option<StopReason> {
        self.terminal.clone()
    }

    pub(crate) fn set_terminal(&mut self, stop_reason: StopReason) {
        self.terminal = Some(stop_reason);
    }

    pub(crate) fn record_usage(&mut self, usage: Usage) {
        self.usage = Some(self.usage.map_or(usage, |known| known.merge(usage)));
    }

    pub(crate) fn take_usage(&mut self) -> Usage {
        self.usage.take().unwrap_or_else(Usage::omitted)
    }

    /// A malformed frame after the terminal stands as the recorded turn;
    /// one before it is a typed transport failure.
    pub(crate) fn anomaly(&self, detail: String) -> Result<bool, ProviderError> {
        if self.terminal.is_some() {
            return Ok(true);
        }
        Err(ProviderError::Transport { detail })
    }
}

/// The per-dialect decode: one wire event onto the lane state; `Ok(true)`
/// ends the lane.
pub(crate) trait LaneDialect {
    fn consume(&mut self, common: &mut LaneCommon, event: SseEvent) -> Result<bool, ProviderError>;
}

/// One pass over the wire: the shared state, the per-dialect decode, and
/// the ended flag that closes the stream.
struct Lane<D> {
    reader: Reader,
    common: LaneCommon,
    dialect: D,
    ended: bool,
}

/// Mounts the lane driver over one connected response and dialect.
pub(crate) fn mount<D>(response: reqwest::Response, sse: SseOptions, dialect: D) -> ModelEventStream
where
    D: LaneDialect + Send + 'static,
{
    let reader: Reader = Box::pin(SseReader::new(response.bytes_stream(), sse));
    boxed(stream::unfold(
        Lane {
            reader,
            common: LaneCommon::default(),
            dialect,
            ended: false,
        },
        step,
    ))
}

async fn step<D>(lane: Lane<D>) -> Option<(Result<ModelEvent, ProviderError>, Lane<D>)>
where
    D: LaneDialect + Send + 'static,
{
    let mut lane = lane;
    loop {
        if lane.ended {
            return None;
        }
        if let Some(emit) = advance(&mut lane).await {
            return Some((emit, lane));
        }
    }
}

/// One loop pass: pending deltas flush first, then one wire frame is
/// decoded; `None` keeps the lane rolling.
async fn advance<D>(lane: &mut Lane<D>) -> Option<Result<ModelEvent, ProviderError>>
where
    D: LaneDialect,
{
    if let Some(event) = lane.common.pop_pending() {
        return Some(Ok(event));
    }
    match lane.reader.next().await {
        None => end(lane),
        Some(Err(err)) => fail_wire(lane, err),
        Some(Ok(event)) => feed(lane, event),
    }
}

/// A wire failure before the terminal is a typed transport error; one
/// after it stands as the recorded turn.
fn fail_wire<D>(
    lane: &mut Lane<D>,
    err: SseError<reqwest::Error>,
) -> Option<Result<ModelEvent, ProviderError>>
where
    D: LaneDialect,
{
    if lane.common.terminal().is_some() {
        return end(lane);
    }
    lane.ended = true;
    Some(Err(from_sse(err)))
}

/// Feeds one wire frame to the dialect: an ended decode closes the lane,
/// a failed decode surfaces as the lane's error.
fn feed<D>(lane: &mut Lane<D>, event: SseEvent) -> Option<Result<ModelEvent, ProviderError>>
where
    D: LaneDialect,
{
    match lane.dialect.consume(&mut lane.common, event) {
        Ok(ended) if ended => end(lane),
        Ok(_) => None,
        Err(err) => {
            lane.ended = true;
            Some(Err(err))
        }
    }
}

/// Ends the lane: the buffered stop reason and merged usage flush as the
/// single TurnFinished; a wire end without one surfaces StreamClosed.
fn end<D>(lane: &mut Lane<D>) -> Option<Result<ModelEvent, ProviderError>>
where
    D: LaneDialect,
{
    lane.ended = true;
    let Some(stop_reason) = lane.common.terminal() else {
        return Some(Err(ProviderError::StreamClosed));
    };
    let usage = lane.common.take_usage();
    Some(Ok(ModelEvent::TurnFinished { stop_reason, usage }))
}

fn from_sse(err: SseError<reqwest::Error>) -> ProviderError {
    match err {
        SseError::Transport(err) => transport::classify(err),
        SseError::Malformed { detail } => ProviderError::Transport { detail },
        SseError::EventTooLarge { cap } => ProviderError::Transport {
            detail: format!("SSE event exceeded {cap} bytes"),
        },
        SseError::Idle { .. } => ProviderError::IdleTimeout,
    }
}
