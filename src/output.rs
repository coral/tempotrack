//! Output plugins run independently of capture, analysis, and display refresh.
pub mod config;
pub mod link;
pub mod midi;
pub mod osc;
pub mod rtp;
pub mod schedule;
mod service;
pub use service::{OutputPublisher, OutputService};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStatus {
    pub id: String,
    pub detail: String,
    pub failed: bool,
}
use crate::rhythm::{PulseCursor, RhythmSnapshot};
use rtrb::{Producer, RingBuffer};
use std::{
    collections::VecDeque,
    io::{IsTerminal, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Driver(String),
}

/// Implementations may do I/O: each driver belongs to its own ordinary thread.
/// A reset invalidates scheduled events after an audio generation change.
/// Coalesced snapshots do not reset a running clock.
pub trait OutputDriver: Send + 'static {
    fn name(&self) -> &str;
    fn reset(&mut self);
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError>;
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError>;
    fn status(&self) -> String {
        String::new()
    }
    fn shutdown(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

pub const OUTPUT_NAMES: &[&str] = &["stdout", "none", "link", "osc", "midi", "rtpmidi"];
pub fn from_name(name: &str) -> Result<Option<Box<dyn OutputDriver>>, crate::Error> {
    match name {
        "stdout" => Ok(Some(Box::new(TextOutput::new()))),
        "none" => Ok(None),
        "link" | "osc" | "midi" | "rtpmidi" => Err(crate::Error::Config(format!(
            "output {name:?} requires OutputService::configure and its destination settings"
        ))),
        _ => Err(crate::Error::Config(format!(
            "unknown output {name:?}; available: {}",
            OUTPUT_NAMES.join(", ")
        ))),
    }
}
#[derive(Default)]
pub struct OutputStats {
    pub dropped: AtomicU64,
    pub failed: AtomicBool,
}
pub struct OutputRunner {
    producer: Producer<RhythmSnapshot>,
    stop: Arc<AtomicBool>,
    pub stats: Arc<OutputStats>,
    status: Arc<Mutex<OutputStatus>>,
    thread: Option<JoinHandle<()>>,
    pub name: String,
}
type Factory = Box<dyn FnMut() -> Result<Box<dyn OutputDriver>, OutputError> + Send>;
impl OutputRunner {
    pub fn spawn(driver: Box<dyn OutputDriver>) -> Result<Self, std::io::Error> {
        let name = driver.name().to_owned();
        let mut initial = Some(driver);
        Self::spawn_factory(
            name,
            Box::new(move || {
                initial
                    .take()
                    .ok_or_else(|| OutputError::Driver("driver stopped".into()))
            }),
            false,
        )
    }
    fn spawn_factory(
        name: String,
        mut factory: Factory,
        retry: bool,
    ) -> Result<Self, std::io::Error> {
        let (producer, mut consumer) = RingBuffer::<RhythmSnapshot>::new(128);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(OutputStats::default());
        let status = Arc::new(Mutex::new(OutputStatus {
            id: name.clone(),
            detail: "Starting…".into(),
            failed: false,
        }));
        let worker_stop = stop.clone();
        let worker_stats = stats.clone();
        let worker_status = status.clone();
        let thread = thread::Builder::new()
            .name(format!("output-{}", name.replace('\0', "�")))
            .spawn(move || {
                let set_status = |detail: String, failed: bool| {
                    let mut status = worker_status.lock().unwrap_or_else(|e| e.into_inner());
                    status.detail = detail;
                    status.failed = failed;
                    worker_stats.failed.store(failed, Ordering::Release);
                };
                let mut failures = 0_u32;
                while !worker_stop.load(Ordering::Acquire) {
                    let started = Instant::now();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                        || -> Result<(), OutputError> {
                            let mut driver = factory()?;
                            let mut generation = None;
                            let mut next_status = Instant::now();
                            let result = (|| -> Result<(), OutputError> {
                                while !worker_stop.load(Ordering::Acquire) {
                                    let mut latest = None;
                                    while let Ok(snapshot) = consumer.pop() {
                                        latest = Some(snapshot);
                                    }
                                    if let Some(snapshot) = latest {
                                        // Sequence gaps from normal coalescing aren't discontinuities.
                                        if generation != Some(snapshot.generation) {
                                            driver.reset();
                                        }
                                        generation = Some(snapshot.generation);
                                        driver.on_timeline(snapshot)?;
                                    }
                                    let now = Instant::now();
                                    let next = driver.poll(now)?;
                                    if now >= next_status {
                                        let detail = driver.status();
                                        set_status(
                                            if detail.is_empty() {
                                                "Ready".into()
                                            } else {
                                                detail
                                            },
                                            false,
                                        );
                                        next_status = now + Duration::from_millis(250);
                                    }
                                    let wait = next.map_or(Duration::from_millis(2), |deadline| {
                                        deadline.saturating_duration_since(Instant::now())
                                    });
                                    thread::park_timeout(wait.clamp(
                                        Duration::from_micros(100),
                                        Duration::from_millis(2),
                                    ));
                                }
                                Ok(())
                            })();
                            let shutdown = driver.shutdown();
                            result.and(shutdown)
                        },
                    ));
                    let error = match result {
                        Ok(Ok(())) => break,
                        Ok(Err(error)) => error.to_string(),
                        Err(payload) => format!(
                            "driver panicked: {}",
                            payload
                                .downcast_ref::<String>()
                                .map(String::as_str)
                                .or_else(|| payload.downcast_ref::<&str>().copied())
                                .unwrap_or("unknown error")
                        ),
                    };
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if started.elapsed() >= Duration::from_secs(10) {
                        failures = 0;
                    }
                    let delay = [1, 2, 5][failures.min(2) as usize];
                    failures = failures.saturating_add(1);
                    set_status(
                        if retry {
                            format!("{error} · retrying in {delay}s")
                        } else {
                            error
                        },
                        true,
                    );
                    if !retry {
                        break;
                    }
                    let retry_at = Instant::now() + Duration::from_secs(delay);
                    while !worker_stop.load(Ordering::Acquire) && Instant::now() < retry_at {
                        thread::park_timeout(
                            retry_at
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(50)),
                        );
                    }
                }
            })?;
        Ok(Self {
            producer,
            stop,
            stats,
            status,
            thread: Some(thread),
            name,
        })
    }
    pub fn status(&self) -> OutputStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    /// Used by the output manager; the analysis worker has a separate ring publisher.
    pub fn publish(&mut self, snapshot: RhythmSnapshot) {
        if self.producer.push(snapshot).is_err() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}
impl Drop for OutputRunner {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct TextOutput {
    tty: bool,
    snapshot: Option<RhythmSnapshot>,
    cursor: PulseCursor,
    flashes: [Option<Instant>; 3],
    next_print: Instant,
}
impl TextOutput {
    fn new() -> Self {
        Self {
            tty: std::io::stdout().is_terminal(),
            snapshot: None,
            cursor: PulseCursor::default(),
            flashes: [None; 3],
            next_print: Instant::now(),
        }
    }
}
impl OutputDriver for TextOutput {
    fn name(&self) -> &str {
        "stdout"
    }
    fn reset(&mut self) {
        self.cursor.reset();
        self.flashes = [None; 3];
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        let Some(snapshot) = self.snapshot else {
            return Ok(None);
        };
        for (i, pulse) in self.cursor.poll(&snapshot, now).into_iter().enumerate() {
            if pulse {
                self.flashes[i] = Some(now);
            }
        }
        if now >= self.next_print {
            let bpm = snapshot
                .bpm
                .map_or_else(|| "—".into(), |bpm| format!("{bpm:.1}"));
            let lamps = self.flashes.map(|flash| {
                if snapshot.active(now)
                    && flash.is_some_and(|f| now.duration_since(f) < Duration::from_millis(120))
                {
                    '*'
                } else {
                    '.'
                }
            });
            let mut stdout = std::io::stdout().lock();
            if self.tty {
                write!(stdout, "\r\x1b[2K")?;
            }
            write!(
                stdout,
                "{} | {} | {:>6} BPM | {:>5.1} dBFS | BAR {} BEAT {} ATOM {} | dropped {}",
                snapshot.backend,
                snapshot.transport,
                bpm,
                snapshot.level_db,
                lamps[0],
                lamps[1],
                lamps[2],
                snapshot.dropped_frames
            )?;
            if !self.tty {
                writeln!(stdout)?;
            }
            stdout.flush()?;
            self.next_print = now
                + if self.tty {
                    Duration::from_millis(80)
                } else {
                    Duration::from_secs(1)
                };
        }
        Ok(Some(
            PulseCursor::next_deadline(&snapshot, now)
                .map_or(self.next_print, |next| next.min(self.next_print)),
        ))
    }
    fn shutdown(&mut self) -> Result<(), OutputError> {
        if self.tty {
            writeln!(std::io::stdout())?;
        }
        Ok(())
    }
}

/// Bounded recording driver for integration tests and embedding.
#[derive(Clone, Default)]
pub struct RecordingOutput {
    snapshots: Arc<Mutex<VecDeque<RhythmSnapshot>>>,
}
impl RecordingOutput {
    pub fn snapshots(&self) -> Vec<RhythmSnapshot> {
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }
}
impl OutputDriver for RecordingOutput {
    fn name(&self) -> &str {
        "recording"
    }
    fn reset(&mut self) {}
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|_| OutputError::Driver("recording lock poisoned".into()))?;
        if snapshots.len() == 256 {
            snapshots.pop_front();
        }
        snapshots.push_back(snapshot);
        Ok(())
    }
    fn poll(&mut self, _now: Instant) -> Result<Option<Instant>, OutputError> {
        Ok(None)
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;
    use crate::config::Tracking;
    use std::sync::{atomic::AtomicUsize, mpsc};

    fn snapshot(generation: u64, sequence: u64) -> RhythmSnapshot {
        let mut snapshot = RhythmSnapshot::empty(Instant::now(), Tracking::Assisted);
        snapshot.generation = generation;
        snapshot.sequence = sequence;
        snapshot
    }
    fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) {
        let end = Instant::now() + timeout;
        while !predicate() {
            assert!(
                Instant::now() < end,
                "output worker did not reach the expected state"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[derive(Debug, PartialEq)]
    enum Event {
        Reset,
        Timeline(u64, u64),
        Poll,
        Shutdown,
    }
    struct GatedDriver {
        events: mpsc::Sender<Event>,
        gate: mpsc::Receiver<()>,
    }
    impl OutputDriver for GatedDriver {
        fn name(&self) -> &str {
            "gated"
        }
        fn reset(&mut self) {
            let _ = self.events.send(Event::Reset);
        }
        fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
            let _ = self
                .events
                .send(Event::Timeline(snapshot.generation, snapshot.sequence));
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, OutputError> {
            let _ = self.events.send(Event::Poll);
            // A timeout ensures failed assertions cannot strand a worker in Drop.
            let _ = self.gate.recv_timeout(Duration::from_secs(2));
            Ok(None)
        }
        fn shutdown(&mut self) -> Result<(), OutputError> {
            let _ = self.events.send(Event::Shutdown);
            Ok(())
        }
    }
    #[test]
    fn coalescing_skipped_sequences_preserves_clock_until_generation_changes() {
        let (events, received) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut runner = OutputRunner::spawn(Box::new(GatedDriver { events, gate })).unwrap();
        let next = || received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(next(), Event::Poll);
        runner.publish(snapshot(1, 1));
        release.send(()).unwrap();
        assert_eq!(next(), Event::Reset);
        assert_eq!(next(), Event::Timeline(1, 1));
        assert_eq!(next(), Event::Poll);

        for sequence in 2..=50 {
            runner.publish(snapshot(1, sequence));
        }
        release.send(()).unwrap();
        assert_eq!(next(), Event::Timeline(1, 50));
        assert_eq!(next(), Event::Poll);

        runner.publish(snapshot(2, 51));
        runner.publish(snapshot(2, 52));
        release.send(()).unwrap();
        assert_eq!(next(), Event::Reset);
        assert_eq!(next(), Event::Timeline(2, 52));
        assert_eq!(next(), Event::Poll);
        runner.request_stop();
        release.send(()).unwrap();
        drop(runner);
        assert_eq!(next(), Event::Shutdown);
    }

    struct RecoveredDriver {
        received: mpsc::Sender<u64>,
        shutdowns: Arc<AtomicUsize>,
    }
    impl OutputDriver for RecoveredDriver {
        fn name(&self) -> &str {
            "recovered"
        }
        fn reset(&mut self) {}
        fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
            let _ = self.received.send(snapshot.sequence);
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, OutputError> {
            Ok(None)
        }
        fn shutdown(&mut self) -> Result<(), OutputError> {
            self.shutdowns.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    #[test]
    fn failed_factory_retries_recovers_timeline_and_clears_error() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let (received, events) = mpsc::channel();
        let factory_attempts = attempts.clone();
        let factory_shutdowns = shutdowns.clone();
        let mut runner = OutputRunner::spawn_factory(
            "retry-test".into(),
            Box::new(move || {
                if factory_attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(OutputError::Driver(
                        "endpoint temporarily unavailable".into(),
                    ));
                }
                Ok(Box::new(RecoveredDriver {
                    received: received.clone(),
                    shutdowns: factory_shutdowns.clone(),
                }))
            }),
            true,
        )
        .unwrap();
        wait_until(|| runner.status().failed, Duration::from_secs(2));
        let status = runner.status();
        assert!(status.detail.contains("endpoint temporarily unavailable"));
        assert!(status.detail.contains("retrying in 1s"));
        assert!(runner.stats.failed.load(Ordering::Acquire));
        runner.publish(snapshot(1, 42));
        assert_eq!(events.recv_timeout(Duration::from_secs(3)).unwrap(), 42);
        wait_until(|| !runner.status().failed, Duration::from_secs(2));
        assert!(!runner.stats.failed.load(Ordering::Acquire));
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        drop(runner);
        assert_eq!(shutdowns.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn stopping_during_backoff_does_not_wait_for_the_next_retry() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let factory_attempts = attempts.clone();
        let runner = OutputRunner::spawn_factory(
            "stop-retry".into(),
            Box::new(move || {
                factory_attempts.fetch_add(1, Ordering::Relaxed);
                Err(OutputError::Driver("offline".into()))
            }),
            true,
        )
        .unwrap();
        wait_until(|| runner.status().failed, Duration::from_secs(2));
        runner.request_stop();
        wait_until(|| runner.is_finished(), Duration::from_millis(500));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn factory_panic_is_retained_as_destination_error() {
        let runner = OutputRunner::spawn_factory(
            "panic-isolated".into(),
            Box::new(|| {
                panic!("simulated driver initialization panic");
            }),
            false,
        )
        .unwrap();
        wait_until(|| runner.is_finished(), Duration::from_secs(2));
        let status = runner.status();
        assert!(status.failed);
        assert!(
            status
                .detail
                .contains("simulated driver initialization panic")
        );
    }
}
