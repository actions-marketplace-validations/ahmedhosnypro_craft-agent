//! RED corpus: the terminal stream line — guaranteed even when the
//! event queue is full or the run never starts.

use std::sync::mpsc;

use craft_app::cli::OutputFormat;
use craft_app::event_writer::{EVENT_QUEUE_CAP, EventSink};
use craft_app::stream_json::write_terminal_line;
use craft_app::{CancelCategory, RuntimeEvent};

#[test]
fn terminal_line_is_a_single_run_finished_event() {
    for format in [OutputFormat::StreamJson, OutputFormat::Plain] {
        let mut out = Vec::new();
        write_terminal_line(&mut out, format, "boom".to_string());
        let text = String::from_utf8(out).expect("valid utf-8");
        assert_eq!(text.lines().count(), 1, "{format:?}");
        if format == OutputFormat::StreamJson {
            let parsed: RuntimeEvent =
                serde_json::from_str(text.trim_end()).expect("line parses as schema");
            assert!(matches!(
                parsed,
                RuntimeEvent::RunFinished {
                    exit_code: 1,
                    category: CancelCategory::Error,
                    turns: 0,
                    ..
                }
            ));
        }
    }
}

struct Broken;

impl std::io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("pipe closed"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("pipe closed"))
    }
}

#[test]
fn terminal_line_survives_a_broken_stdout() {
    write_terminal_line(Broken, OutputFormat::StreamJson, "boom".to_string());
}

#[test]
fn full_queue_drops_events_without_aborting() {
    let (tx, _events) = mpsc::sync_channel(2);
    let (closed, closed_rx) = tokio::sync::watch::channel(false);
    let sink = EventSink::from_raw_parts(tx, closed);
    for turn in 1..=5 {
        sink.emit(RuntimeEvent::TurnStarted { turn });
    }
    assert!(
        !*closed_rx.borrow(),
        "a stalled consumer must not abort the run"
    );
}

#[test]
fn disconnected_queue_flags_the_run_for_abort() {
    let (tx, events) = mpsc::sync_channel(2);
    let (closed, closed_rx) = tokio::sync::watch::channel(false);
    let sink = EventSink::from_raw_parts(tx, closed);
    drop(events);
    sink.emit(RuntimeEvent::TurnStarted { turn: 1 });
    assert!(*closed_rx.borrow(), "a closed consumer must flag the run");
}

#[test]
fn finish_waits_out_a_full_queue_instead_of_dropping_the_terminal_event() {
    let (tx, events) = mpsc::sync_channel(EVENT_QUEUE_CAP);
    let (closed, _closed_rx) = tokio::sync::watch::channel(false);
    let sink = EventSink::from_raw_parts(tx, closed);
    for _ in 0..EVENT_QUEUE_CAP {
        sink.emit(RuntimeEvent::TurnStarted { turn: 1 });
    }
    let parked = std::thread::spawn(move || {
        sink.finish(RuntimeEvent::RunFinished {
            exit_code: 0,
            category: CancelCategory::Completed,
            detail: "done".to_string(),
            usage: craft_app::Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                incomplete: false,
            },
            turns: 1,
        });
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !parked.is_finished(),
        "finish must wait for queue room, not drop"
    );
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        for _ in 0..=EVENT_QUEUE_CAP {
            drop(events.recv());
        }
        done_tx.send(()).expect("drain reported");
    });
    parked.join().expect("finish thread joins once room opens");
    done_rx.recv().expect("drain emptied the queue");
    drain.join().expect("drain thread joins");
}
