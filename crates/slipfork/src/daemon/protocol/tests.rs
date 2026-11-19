//! Retained-log fan-in tests: restart dividers, replay dedupe, and prompt reaping.

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use slipfork_world::{LogStream, ServiceLogs, StreamChunk, StreamSource, epoch_millis_now};

use super::super::types::{HookLog, LogBus};
use super::fan_in::{self, FanIn, LogSource};

/// Wait for the next follower chunk, failing the test rather than hanging forever.
fn next(receiver: &mpsc::Receiver<slipfork_world::LogChunk>) -> slipfork_world::LogChunk {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the follower must stay attached")
}

#[test]
fn divider_appears_on_restart_and_down_up_while_a_follower_stays_attached() {
    let bus = Arc::new(LogBus::default());
    bus.boot(epoch_millis_now());
    let epochs: Arc<Mutex<Vec<ServiceLogs>>> = Arc::new(Mutex::new(vec![ServiceLogs::new("api")]));

    let (sender, receiver) = mpsc::channel();
    let follower_bus = Arc::clone(&bus);
    let follower_epochs = Arc::clone(&epochs);
    let aborted = Arc::new(AtomicBool::new(false));
    let follower = thread::spawn(move || {
        let _ = fan_in::follow_bus(
            &follower_bus,
            || {
                Ok(follower_epochs
                    .lock()
                    .expect("epochs")
                    .last()
                    .map(|log| vec![LogSource::Service(log.clone())])
                    .unwrap_or_default())
            },
            false,
            None,
            &aborted,
            &mut |chunk| {
                let _ = sender.send(chunk);
                ControlFlow::Continue(())
            },
        );
    });

    // Epoch 0: the first boot streams its output.
    epochs.lock().expect("epochs")[0].push(StreamSource::Stdout, b"before\n".to_vec());
    assert_eq!(next(&receiver).bytes, b"before\n");

    // `restart`: the old window closes and the daemon publishes one epoch.
    epochs.lock().expect("epochs")[0].close();
    let second = ServiceLogs::new("api");
    second.push(StreamSource::Stdout, b"after\n".to_vec());
    epochs.lock().expect("epochs").push(second.clone());
    bus.boot(epoch_millis_now());

    let divider = next(&receiver);
    assert!(
        divider.is_event(),
        "restart must emit a divider: {divider:?}"
    );
    assert_eq!(next(&receiver).bytes, b"after\n");

    // `down`: the world record and its windows disappear; the bus and follower do not.
    second.close();
    epochs.lock().expect("epochs").clear();
    let third = ServiceLogs::new("api");
    third.push(StreamSource::Stdout, b"after-up\n".to_vec());
    epochs.lock().expect("epochs").push(third.clone());
    bus.boot(epoch_millis_now());

    assert!(
        next(&receiver).is_event(),
        "down + up must emit its own divider"
    );
    assert_eq!(next(&receiver).bytes, b"after-up\n");

    third.close();
    bus.shutdown();
    let started = Instant::now();
    follower.join().expect("the follower thread reaped");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_reused_hook_window_emits_only_new_lines_across_two_epochs() {
    let bus = Arc::new(LogBus::default());
    bus.boot(epoch_millis_now());
    let hook = Arc::new(HookLog::new(
        "api:pre-deploy".to_owned(),
        PathBuf::from("/var/tmp/slipfork-replay.log"),
    ));
    hook.push(StreamChunk {
        source: StreamSource::Stdout,
        bytes: b"before\n".to_vec(),
    });

    let (sender, receiver) = mpsc::channel();
    let follower_bus = Arc::clone(&bus);
    let follower_hook = Arc::clone(&hook);
    let aborted = Arc::new(AtomicBool::new(false));
    let follower = thread::spawn(move || {
        let _ = fan_in::follow_bus(
            &follower_bus,
            || Ok(vec![LogSource::Hook(Arc::clone(&follower_hook))]),
            false,
            None,
            &aborted,
            &mut |chunk| {
                let _ = sender.send(chunk);
                ControlFlow::Continue(())
            },
        );
    });

    assert_eq!(next(&receiver).bytes, b"before\n");

    // Epoch 2 reuses the same window: only the newly appended line may be emitted.
    hook.push(StreamChunk {
        source: StreamSource::Stdout,
        bytes: b"after\n".to_vec(),
    });
    bus.boot(epoch_millis_now());
    hook.close();
    bus.shutdown();
    follower.join().expect("the follower thread reaped");

    // The first `next` above consumed the one legitimate history line; nothing after it may
    // repeat that history.
    let chunks: Vec<_> = receiver.into_iter().collect();
    assert!(
        chunks.iter().all(|chunk| chunk.bytes != b"before\n"),
        "history must never be re-emitted: {chunks:?}"
    );
    let afters = chunks
        .iter()
        .filter(|chunk| chunk.bytes == b"after\n")
        .count();
    assert_eq!(
        afters, 1,
        "new lines must be emitted exactly once: {chunks:?}"
    );
}

#[test]
fn dropping_the_consumer_aborts_the_reader_threads_promptly() {
    let bus = Arc::new(LogBus::default());
    bus.boot(epoch_millis_now());
    let log = ServiceLogs::new("api");
    log.push(StreamSource::Stdout, b"first\n".to_vec());
    let aborted = AtomicBool::new(false);
    let started = Instant::now();
    let mut emitted = 0;

    fan_in::follow_bus(
        &bus,
        || Ok(vec![LogSource::Service(log.clone())]),
        false,
        None,
        &aborted,
        &mut |_chunk| {
            emitted += 1;
            ControlFlow::Break(())
        },
    )
    .expect("an aborted follower is not an error");

    assert_eq!(emitted, 1, "the client saw exactly the chunk it broke on");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the source reader must be reaped, not left blocked on the log condvar"
    );
}

#[test]
fn tail_preserves_each_lines_stream_and_capture_time() {
    let log = ServiceLogs::new("api");
    log.push_at(10, StreamSource::Stdout, b"out1\nout2\n".to_vec());
    log.push_at(20, StreamSource::Stderr, b"err1\n".to_vec());
    let mut fan = FanIn::new(vec![LogSource::Service(log)], false);
    let tailed = fan.tail(2);

    assert_eq!(tailed.len(), 2, "two lines are kept: {tailed:?}");
    assert_eq!(tailed[0].bytes, b"out2\n");
    assert_eq!(tailed[0].stream, LogStream::Stdout);
    assert_eq!(tailed[0].ts_ms, 10);
    assert_eq!(tailed[1].bytes, b"err1\n");
    assert_eq!(tailed[1].stream, LogStream::Stderr);
    assert_eq!(tailed[1].ts_ms, 20);
}

#[test]
fn collect_merges_stdout_and_stderr_by_capture_time() {
    let log = ServiceLogs::new("api");
    log.push_at(30, StreamSource::Stderr, b"third\n".to_vec());
    log.push_at(10, StreamSource::Stdout, b"first\n".to_vec());
    log.push_at(20, StreamSource::Stdout, b"second\n".to_vec());
    let merged = FanIn::new(vec![LogSource::Service(log)], false).merged_snapshot();
    let text: String = merged
        .iter()
        .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
        .collect();
    assert_eq!(text, "first\nsecond\nthird\n");
    assert!(merged.iter().all(|chunk| !chunk.is_event()));
}
