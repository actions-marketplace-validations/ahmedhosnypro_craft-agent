//! The scripted provider double with delayed replies and
//! live-stream concurrency instrumentation.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::{ModelEventStream, Provider, boxed};
use craft_core::types::{ModelRequest, StopReason, Usage};
use futures::Stream;
use futures::stream;

/// One scripted provider call.
#[derive(Clone)]
pub enum Reply {
    /// Stream these events in order, then end the stream.
    Events(Vec<Result<ModelEvent, ProviderError>>),
    /// `stream()` itself fails before producing any event.
    Fail(ProviderError),
    /// Sleeps before every event: makes a child observably
    /// in-flight (the concurrency row) or past its wall clock.
    Delayed(Duration, Vec<Result<ModelEvent, ProviderError>>),
}

/// Decrements the live-stream count when the stream ends.
struct ActiveGuard {
    active: Arc<AtomicI32>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Guarded {
    inner: ModelEventStream,
    _guard: ActiveGuard,
}

impl Stream for Guarded {
    type Item = Result<ModelEvent, ProviderError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Scripted provider: plays replies in order, then repeats the last
/// reply forever, recording every request; the live-stream counter
/// observes real concurrency.
pub struct FakeProvider {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<ModelRequest>>,
    active: Arc<AtomicI32>,
    max_active: Arc<AtomicI32>,
}

impl FakeProvider {
    pub fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            active: Arc::new(AtomicI32::new(0)),
            max_active: Arc::new(AtomicI32::new(0)),
        })
    }

    /// The requests the provider received, in call order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// How many streams were live at once during the run.
    pub fn max_concurrency(&self) -> i32 {
        self.max_active.load(Ordering::SeqCst)
    }

    fn next(&self) -> Reply {
        let mut replies = self.replies.lock().unwrap_or_else(|held| held.into_inner());
        match replies.len() {
            0 => Reply::Events(Vec::new()),
            1 => replies[0].clone(),
            _ => replies
                .pop_front()
                .unwrap_or(Reply::Events(Vec::new()))
                .clone(),
        }
    }

    fn guarded(&self, inner: ModelEventStream) -> ModelEventStream {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        boxed(Guarded {
            inner,
            _guard: ActiveGuard {
                active: self.active.clone(),
            },
        })
    }
}

impl Provider for FakeProvider {
    async fn stream(&self, request: ModelRequest) -> Result<ModelEventStream, ProviderError> {
        self.seen
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(request);
        let reply = self.next();
        let stream: ModelEventStream = match reply {
            Reply::Fail(err) => boxed(stream::iter(vec![Err(err)])),
            Reply::Events(events) => boxed(stream::iter(events)),
            Reply::Delayed(delay, events) => boxed(stream::unfold(
                (delay, VecDeque::from(events)),
                |(delay, mut queue)| async move {
                    tokio::time::sleep(delay).await;
                    queue.pop_front().map(|item| (item, (delay, queue)))
                },
            )),
        };
        Ok(self.guarded(stream))
    }
}

pub fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        incomplete: false,
    }
}

/// One streamed event followed by the turn's finish.
fn finish_after(
    head: ModelEvent,
    stop: StopReason,
    used: Usage,
) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(head),
        Ok(ModelEvent::TurnFinished {
            stop_reason: stop,
            usage: used,
        }),
    ]
}

/// A turn that streams text and finishes the turn.
pub fn text_reply(text: &str, used: Usage) -> Reply {
    Reply::Events(finish_after(
        ModelEvent::TextDelta {
            text: text.to_string(),
        },
        StopReason::EndTurn,
        used,
    ))
}

/// A turn that requests one tool call and finishes for tool use.
pub fn tool_reply(id: &str, name: &str, args: &str, used: Usage) -> Reply {
    Reply::Events(finish_after(
        ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            arguments: args.to_string(),
        },
        StopReason::ToolUse,
        used,
    ))
}

/// Slows a scripted reply: every event waits `delay` before streaming.
/// A reply without events rides along undelayed.
pub fn delayed(reply: Reply, delay: Duration) -> Reply {
    match reply {
        Reply::Events(events) => Reply::Delayed(delay, events),
        Reply::Delayed(_, events) => Reply::Delayed(delay, events),
        other => other,
    }
}
