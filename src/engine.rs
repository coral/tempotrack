use crate::{
    Error,
    audio::{self, AudioBlock, BLOCK, Capture},
    backend::{self, Estimate, TrackingBackend},
    config::{Config, LiveControls, Tracking},
    output::{OutputDriver, OutputPublisher, OutputService, OutputStatus, config::OutputConfig},
    rhythm::{RhythmSnapshot, Transport},
};
use cpal::traits::StreamTrait;
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub enum Command {
    Start(Box<Config>),
    Stop,
    Reset,
    Adjust(LiveControls),
}
#[derive(Debug)]
pub enum Event {
    Started {
        input: String,
        rate: u32,
        channels: u16,
    },
    Error(String),
    OutputError(String),
}

pub struct Engine {
    commands: Producer<Command>,
    snapshots: Consumer<RhythmSnapshot>,
    events: Consumer<Event>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    outputs: OutputService,
}
impl Engine {
    pub fn spawn(drivers: Vec<Box<dyn OutputDriver>>) -> Result<Self, Error> {
        let (commands, command_rx) = RingBuffer::new(32);
        let (snapshot_tx, snapshots) = RingBuffer::new(128);
        let (event_tx, events) = RingBuffer::new(32);
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let (outputs, publisher) = OutputService::spawn(drivers)?;
        let thread = thread::Builder::new()
            .name("tempo-analysis".into())
            .spawn(move || worker(command_rx, snapshot_tx, event_tx, signal, publisher))?;
        Ok(Self {
            commands,
            snapshots,
            events,
            stop,
            thread: Some(thread),
            outputs,
        })
    }
    pub fn configure_outputs(&mut self, config: OutputConfig) -> Result<(), Error> {
        self.outputs.configure(config)
    }
    pub fn output_status(&self) -> Vec<OutputStatus> {
        self.outputs.status()
    }
    pub fn send(&mut self, command: Command) -> Result<(), Error> {
        if self.thread.as_ref().is_none_or(|t| t.is_finished()) {
            return Err(Error::WorkerStopped);
        }
        self.commands.push(command).map_err(|_| Error::Busy)
    }
    pub fn start(&mut self, config: Config) -> Result<(), Error> {
        config.validate()?;
        self.send(Command::Start(Box::new(config)))
    }
    pub fn latest(&mut self) -> Option<RhythmSnapshot> {
        let mut latest = None;
        while let Ok(snapshot) = self.snapshots.pop() {
            latest = Some(snapshot);
        }
        latest
    }
    pub fn event(&mut self) -> Option<Event> {
        self.events.pop().ok()
    }
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
    pub fn shutdown(&mut self) -> Result<(), Error> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            self.outputs.shutdown();
            result.map_err(|_| Error::WorkerStopped)?;
        }
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Silence is measured over fixed source blocks, independent of GUI polling.
pub struct SilenceGate {
    quiet_frames: u64,
    pub silent: bool,
}
impl Default for SilenceGate {
    fn default() -> Self {
        Self {
            quiet_frames: 0,
            silent: true,
        }
    }
}
impl SilenceGate {
    pub fn update(
        &mut self,
        level_db: f32,
        frames: usize,
        rate: u32,
        controls: LiveControls,
    ) -> bool {
        let was_silent = self.silent;
        if level_db > controls.silence_threshold_db {
            self.quiet_frames = 0;
            self.silent = false;
        } else {
            self.quiet_frames += frames as u64;
            if self.quiet_frames * 1000 >= u64::from(rate) * u64::from(controls.silence_hold_ms) {
                self.silent = true;
            }
        }
        was_silent != self.silent
    }
}

struct Session {
    capture: Capture,
    analysis: AnalysisState,
    discontinuities: u64,
}

struct AnalysisState {
    backend: Box<dyn TrackingBackend>,
    controls: LiveControls,
    rate: u32,
    expected: u64,
    gate: SilenceGate,
    estimate: Estimate,
    snapshot: RhythmSnapshot,
    last_audio: Instant,
    last_estimate: Instant,
    reset_on_resume: bool,
}
impl Session {
    fn new(config: &Config, generation: u64) -> Result<(Self, Event), Error> {
        let input = audio::prepare(config)?;
        let rate = input.config.sample_rate;
        let backend = backend::create(config, rate)?;
        let capture = audio::build(&input, config.channel)?;
        let event = Event::Started {
            input: input.name,
            rate,
            channels: input.config.channels,
        };
        let now = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(now, Tracking::Assisted);
        snapshot.generation = generation;
        snapshot.sample_rate = rate;
        snapshot.transport = Transport::Listening;
        capture.stream.play()?;
        Ok((
            Self {
                capture,
                discontinuities: 0,
                analysis: AnalysisState {
                    backend,
                    controls: config.live,
                    rate,
                    expected: 0,
                    gate: SilenceGate::default(),
                    estimate: Estimate::default(),
                    snapshot,
                    last_audio: now,
                    last_estimate: now,
                    reset_on_resume: false,
                },
            },
            event,
        ))
    }
}

impl AnalysisState {
    fn reset(&mut self, frame: u64) {
        self.backend.reset(frame);
        self.expected = frame;
        self.estimate = Estimate::default();
        self.snapshot.generation += 1;
        self.snapshot.grids = [None; 3];
        self.snapshot.bpm = None;
        self.snapshot.meter = None;
        self.snapshot.quality = crate::rhythm::Quality::Unknown;
        self.last_estimate = Instant::now();
        self.reset_on_resume = false;
    }
    fn process(&mut self, mut block: AudioBlock) -> Result<(), Error> {
        if block.source_frame != self.expected {
            self.reset(block.source_frame);
        }
        self.expected = block.source_frame + BLOCK as u64;
        let gain = 10_f32.powf(self.controls.gain_db / 20.);
        let mut energy = 0.;
        let mut peak = 0_f32;
        for sample in &mut block.samples {
            *sample = (*sample * gain).clamp(-1., 1.);
            energy += *sample * *sample;
            peak = peak.max(sample.abs());
        }
        let level_db = (10. * (energy / BLOCK as f32).max(1e-10).log10()).max(-96.);
        let changed = self.gate.update(level_db, BLOCK, self.rate, self.controls);
        if changed && self.gate.silent && self.controls.stop_on_silence {
            self.reset_on_resume = true;
        }
        if !self.gate.silent && self.reset_on_resume {
            self.reset(block.source_frame);
            self.expected = block.source_frame + BLOCK as u64;
        }
        if !self.gate.silent {
            if let Some(estimate) = self.backend.process(&block.samples)? {
                self.estimate = estimate;
                self.last_estimate = Instant::now();
            }
        } else {
            // Resume at the correct source position; a silent gap must never be
            // concatenated to the next audible PCM as though time had stopped.
            self.reset_on_resume = true;
        }
        self.last_audio = Instant::now();
        self.snapshot.source_frame = self.expected;
        self.snapshot.source_time = self.expected as f64 / self.rate as f64;
        self.snapshot.reference_time =
            block.captured_at + Duration::from_secs_f64(BLOCK as f64 / self.rate as f64);
        self.snapshot.valid_until = self.last_audio + Duration::from_millis(250);
        self.snapshot.level_db = level_db;
        self.snapshot.peak = peak;
        self.refresh();
        Ok(())
    }
    fn refresh(&mut self) {
        self.snapshot.offset_seconds = self.controls.offset_ms / 1000.;
        let stale = Instant::now().duration_since(self.last_estimate) > Duration::from_secs(2)
            && !self.gate.silent;
        let has_tempo = self.estimate.bpm.is_some() && !stale;
        self.snapshot.transport = if self.gate.silent {
            if self.controls.stop_on_silence || !has_tempo {
                Transport::Silence
            } else {
                Transport::Holdover
            }
        } else if has_tempo && self.estimate.holding {
            Transport::Holdover
        } else if has_tempo {
            Transport::Tracking
        } else {
            Transport::Listening
        };
        self.snapshot.bpm = has_tempo.then_some(self.estimate.bpm).flatten();
        self.snapshot.grids = if has_tempo {
            self.estimate.grids
        } else {
            [None; 3]
        };
        self.snapshot.meter = self.estimate.meter;
        self.snapshot.quality = self.estimate.quality;
    }
}

impl Session {
    fn poll(&mut self) -> Result<(), Error> {
        if let Some(kind) = self.capture.stats.failure() {
            return Err(Error::StreamFailed(kind));
        }
        let discontinuities = self.capture.stats.discontinuities.load(Ordering::Relaxed);
        if discontinuities != self.discontinuities {
            self.discontinuities = discontinuities;
            let mut next_frame = self.analysis.expected;
            for _ in 0..self.capture.audio.slots() {
                if let Ok(block) = self.capture.audio.pop() {
                    next_frame = block.source_frame + BLOCK as u64;
                    self.capture
                        .stats
                        .dropped_frames
                        .fetch_add(BLOCK as u64, Ordering::Relaxed);
                }
            }
            self.analysis.reset(next_frame);
        }
        if self.analysis.last_audio.elapsed() > Duration::from_secs(2) {
            return Err(Error::InputStalled);
        }
        // Bounded work each pass lets control/shutdown messages interrupt backlog.
        let pending = self.capture.audio.slots();
        let max_blocks = (self.analysis.rate as usize / 10).div_ceil(BLOCK);
        if pending > max_blocks {
            for _ in 0..pending.saturating_sub(1) {
                if self.capture.audio.pop().is_ok() {
                    self.capture
                        .stats
                        .dropped_frames
                        .fetch_add(BLOCK as u64, Ordering::Relaxed);
                }
            }
        }
        for _ in 0..8 {
            let Ok(block) = self.capture.audio.pop() else {
                break;
            };
            self.analysis.process(block)?;
        }
        self.analysis.snapshot.dropped_frames =
            self.capture.stats.dropped_frames.load(Ordering::Relaxed);
        Ok(())
    }
}

fn worker(
    mut commands: Consumer<Command>,
    mut snapshots: Producer<RhythmSnapshot>,
    mut events: Producer<Event>,
    stop: Arc<AtomicBool>,
    mut outputs: OutputPublisher,
) {
    let mut session: Option<Session> = None;
    let mut snapshot = RhythmSnapshot::empty(Instant::now(), Default::default());
    let mut generation = 0;
    let mut sequence = 0;
    let mut next_publish = Instant::now();
    while !stop.load(Ordering::Acquire) {
        while let Ok(command) = commands.pop() {
            match command {
                Command::Start(config) => {
                    if let Some(old) = session.take() {
                        generation = generation.max(old.analysis.snapshot.generation);
                        drop(old);
                    }
                    generation += 1;
                    snapshot = RhythmSnapshot::empty(Instant::now(), Tracking::Assisted);
                    snapshot.generation = generation;
                    snapshot.transport = Transport::Initializing;
                    sequence += 1;
                    snapshot.sequence = sequence;
                    let _ = snapshots.push(snapshot);
                    outputs.publish(snapshot);
                    match Session::new(&config, generation) {
                        Ok((new, event)) => {
                            session = Some(new);
                            let _ = events.push(event);
                        }
                        Err(error) => {
                            snapshot.transport = Transport::Error;
                            let _ = events.push(Event::Error(error.to_string()));
                        }
                    }
                }
                Command::Stop => {
                    if let Some(old) = session.take() {
                        generation = generation.max(old.analysis.snapshot.generation);
                    }
                    generation += 1;
                    snapshot.generation = generation;
                    snapshot.transport = Transport::Stopped;
                    snapshot.grids = [None; 3];
                    snapshot.bpm = None;
                }
                Command::Reset => {
                    if let Some(active) = &mut session {
                        active.analysis.reset(active.analysis.expected);
                    }
                }
                Command::Adjust(controls) => {
                    if let Err(error) = controls.validate() {
                        let _ = events.push(Event::Error(error.to_string()));
                    } else if let Some(active) = &mut session {
                        active.analysis.controls = controls;
                        active.analysis.refresh();
                    }
                }
            }
        }
        if let Some(active) = &mut session {
            match active.poll() {
                Ok(()) => snapshot = active.analysis.snapshot,
                Err(error) => {
                    generation = generation.max(active.analysis.snapshot.generation) + 1;
                    snapshot.generation = generation;
                    snapshot.transport = Transport::Error;
                    snapshot.grids = [None; 3];
                    snapshot.bpm = None;
                    session = None;
                    let _ = events.push(Event::Error(error.to_string()));
                }
            }
        }
        if Instant::now() >= next_publish {
            sequence += 1;
            snapshot.sequence = sequence;
            let _ = snapshots.push(snapshot);
            outputs.publish(snapshot);
            next_publish = Instant::now() + Duration::from_millis(20);
        }
        thread::sleep(Duration::from_micros(500));
    }
    drop(session);
    snapshot.transport = Transport::Stopped;
    snapshot.grids = [None; 3];
    snapshot.bpm = None;
    sequence += 1;
    snapshot.sequence = sequence;
    outputs.publish(snapshot);
    // Drivers receive explicit shutdown even if their timeline queue was full.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::Capabilities,
        rhythm::{Provenance, PulseGrid},
    };
    use std::sync::atomic::AtomicU64;

    struct TestBackend {
        reset_frame: Arc<AtomicU64>,
    }
    impl TrackingBackend for TestBackend {
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn reset(&mut self, frame: u64) {
            self.reset_frame.store(frame, Ordering::Relaxed);
        }
        fn process(&mut self, _: &[f32]) -> Result<Option<Estimate>, Error> {
            Ok(Some(Estimate {
                bpm: Some(120.),
                grids: [2., 0.5, 0.125].map(|period| {
                    Some(PulseGrid {
                        anchor: self.reset_frame.load(Ordering::Relaxed) as f64 / 8000.,
                        period,
                        provenance: Provenance::Detected,
                    })
                }),
                ..Default::default()
            }))
        }
    }
    fn analysis() -> (AnalysisState, Arc<AtomicU64>) {
        let now = Instant::now();
        let reset_frame = Arc::new(AtomicU64::new(0));
        (
            AnalysisState {
                backend: Box::new(TestBackend {
                    reset_frame: reset_frame.clone(),
                }),
                controls: LiveControls {
                    silence_hold_ms: 10,
                    ..Default::default()
                },
                rate: 8000,
                expected: 0,
                gate: SilenceGate::default(),
                estimate: Estimate::default(),
                snapshot: RhythmSnapshot::empty(now, Default::default()),
                last_audio: now,
                last_estimate: now,
                reset_on_resume: false,
            },
            reset_frame,
        )
    }
    fn block(frame: u64, sample: f32) -> AudioBlock {
        AudioBlock {
            source_frame: frame,
            captured_at: Instant::now(),
            samples: [sample; BLOCK],
        }
    }
    #[test]
    fn gap_resets_analysis_and_generation_before_new_audio() {
        let (mut analysis, resets) = analysis();
        analysis.process(block(0, 0.2)).unwrap();
        assert_eq!(analysis.snapshot.transport, Transport::Tracking);
        let generation = analysis.snapshot.generation;
        analysis.process(block(4096, 0.2)).unwrap();
        assert_eq!(resets.load(Ordering::Relaxed), 4096);
        assert_eq!(analysis.snapshot.generation, generation + 1);
        assert_eq!(analysis.snapshot.source_frame, 4096 + BLOCK as u64);
        assert!(analysis.snapshot.grids[1].unwrap().anchor >= 0.5);
    }
    #[test]
    fn silence_suppresses_pulses_holdover_is_explicit_and_resume_resets() {
        let (mut analysis, resets) = analysis();
        analysis.process(block(0, 0.2)).unwrap();
        analysis.process(block(BLOCK as u64, 0.)).unwrap();
        assert_eq!(analysis.snapshot.transport, Transport::Silence);
        assert!(!analysis.snapshot.active(Instant::now()));
        analysis.controls.stop_on_silence = false;
        analysis.refresh();
        assert_eq!(analysis.snapshot.transport, Transport::Holdover);
        assert!(analysis.snapshot.active(Instant::now()));
        analysis.process(block(BLOCK as u64 * 2, 0.2)).unwrap();
        assert_eq!(resets.load(Ordering::Relaxed), BLOCK as u64 * 2);
        assert_eq!(analysis.snapshot.generation, 1);
        assert_eq!(analysis.snapshot.transport, Transport::Tracking);
        assert!(
            !analysis
                .snapshot
                .active(Instant::now() + Duration::from_secs(1))
        );
    }
    #[test]
    fn gain_and_offset_changes_do_not_reset_the_tracker() {
        let (mut analysis, _) = analysis();
        analysis.controls.gain_db = -6.;
        analysis.controls.offset_ms = 125.;
        analysis.process(block(0, 0.5)).unwrap();
        assert!((analysis.snapshot.level_db - (-12.0206)).abs() < 0.001);
        assert_eq!(analysis.snapshot.offset_seconds, 0.125);
        assert_eq!(analysis.snapshot.generation, 0);
    }
}
