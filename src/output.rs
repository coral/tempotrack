//! Output plugins run independently of capture, analysis, and display refresh.
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
/// A reset invalidates all scheduled events, including after a queue sequence gap.
pub trait OutputDriver: Send + 'static {
    fn name(&self) -> &str;
    fn reset(&mut self);
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError>;
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError>;
    fn shutdown(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

pub const OUTPUT_NAMES: &[&str] = &["stdout", "none"];
pub fn from_name(name: &str) -> Result<Option<Box<dyn OutputDriver>>, crate::Error> {
    match name {
        "stdout" => Ok(Some(Box::new(TextOutput::new()))),
        "none" => Ok(None),
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
    thread: Option<JoinHandle<()>>,
    pub name: String,
}
impl OutputRunner {
    pub fn spawn(mut driver: Box<dyn OutputDriver>) -> Result<Self, std::io::Error> {
        let (producer, mut consumer) = RingBuffer::<RhythmSnapshot>::new(128);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(OutputStats::default());
        let name = driver.name().to_owned();
        let worker_stop = stop.clone();
        let worker_stats = stats.clone();
        let thread = thread::Builder::new()
            .name(format!("output-{name}"))
            .spawn(move || {
                let mut previous = None;
                let result = (|| -> Result<(), OutputError> {
                    while !worker_stop.load(Ordering::Acquire) {
                        // Coalesce to the newest timeline. Events are projected from it,
                        // never replayed from a queue of historical beat triggers.
                        let mut latest = None;
                        while let Ok(snapshot) = consumer.pop() {
                            latest = Some(snapshot);
                        }
                        if let Some(snapshot) = latest {
                            if previous.is_none_or(|(generation, sequence)| {
                                generation != snapshot.generation
                                    || sequence + 1 != snapshot.sequence
                            }) {
                                driver.reset();
                            }
                            previous = Some((snapshot.generation, snapshot.sequence));
                            driver.on_timeline(snapshot)?;
                        }
                        let now = Instant::now();
                        let next = driver.poll(now)?;
                        let wait = next.map_or(Duration::from_millis(5), |deadline| {
                            deadline.saturating_duration_since(Instant::now())
                        });
                        thread::sleep(
                            wait.clamp(Duration::from_micros(100), Duration::from_millis(5)),
                        );
                    }
                    Ok(())
                })();
                let shutdown = driver.shutdown();
                if let Err(error) = result.and(shutdown) {
                    worker_stats.failed.store(true, Ordering::Release);
                    eprintln!("Output {} failed: {error}", driver.name());
                }
            })?;
        Ok(Self {
            producer,
            stop,
            stats,
            thread: Some(thread),
            name,
        })
    }
    /// Never blocks the analysis worker, even when an output is slow or disconnected.
    pub fn publish(&mut self, snapshot: RhythmSnapshot) {
        if self.producer.push(snapshot).is_err() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl Drop for OutputRunner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
