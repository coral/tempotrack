//! Adapters own backend state; backend callbacks never cross a thread boundary.
use crate::{
    Error,
    config::{Config, Tracking},
    rhythm::{Provenance, PulseGrid, Quality},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub detected_atom: bool,
    pub tempo_bounds: bool,
    pub meter_bounds: bool,
    pub model_selection: bool,
}
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    pub time: f64,
    pub beat: f32,
    pub downbeat: f32,
    pub event: bool,
}
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Estimate {
    pub grids: [Option<PulseGrid>; 3],
    pub bpm: Option<f64>,
    pub meter: Option<u8>,
    pub quality: Quality,
    pub evidence: Option<Evidence>,
    /// A clock exists, but current observations no longer support corrections.
    #[serde(default)]
    pub holding: bool,
}
impl Default for Estimate {
    fn default() -> Self {
        Self {
            grids: [None; 3],
            bpm: None,
            meter: None,
            quality: Quality::Unknown,
            evidence: None,
            holding: false,
        }
    }
}
impl Estimate {
    /// Keep pulse zero stable as backends report newer phase anchors. This makes
    /// beat_position continuous across beats and tempo corrections (until reset).
    pub fn align_to(&mut self, previous: &Self) {
        for (grid, old) in self.grids.iter_mut().zip(previous.grids) {
            if let (Some(grid), Some(old)) = (grid, old)
                && grid.valid()
                && old.valid()
            {
                grid.anchor -= old.position(grid.anchor).round() * grid.period;
            }
        }
    }
}
/// Processing is synchronous and returns the latest estimate. Every backend event
/// is incorporated by the adapter before returning; no event queue is required.
pub trait TrackingBackend: Send {
    fn capabilities(&self) -> Capabilities;
    fn process(&mut self, mono: &[f32]) -> Result<Option<Estimate>, Error>;
    fn reset(&mut self, source_frame: u64);
    fn process_each(&mut self, mono: &[f32], emit: &mut dyn FnMut(Estimate)) -> Result<(), Error> {
        if let Some(estimate) = self.process(mono)? {
            emit(estimate);
        }
        Ok(())
    }
}
/// Stable application timeline. The raw adapters remain available for diagnostics.
pub fn create(config: &Config, rate: u32) -> Result<Box<dyn TrackingBackend>, Error> {
    create_for_evaluation(config, rate, Tracking::Assisted)
}

/// Detector comparison for the offline harness and tests. Production uses `create`.
pub fn create_for_evaluation(
    config: &Config,
    rate: u32,
    tracking: Tracking,
) -> Result<Box<dyn TrackingBackend>, Error> {
    let raw = create_raw(config, rate, tracking)?;
    let advisor = if tracking == Tracking::Assisted {
        Some(create_raw(config, rate, Tracking::Pulseweave)?)
    } else {
        None
    };
    Ok(Box::new(StableBackend {
        raw,
        advisor,
        clock: if tracking == Tracking::Pulseweave {
            crate::clock::BeatClock::new(crate::config::ATOM_SUBDIVISION)
        } else {
            crate::clock::BeatClock::for_config(config)
        },
        advisory: TempoAdvisor::new(crate::config::ATOM_SUBDIVISION),
        subdivision: crate::config::ATOM_SUBDIVISION,
        rate,
        frame: 0,
        buffer: [0.; 128],
        filled: 0,
    }))
}

/// Unstabilized detector estimates; do not schedule output pulses from these.
pub fn create_raw(
    config: &Config,
    rate: u32,
    tracking: Tracking,
) -> Result<Box<dyn TrackingBackend>, Error> {
    create_raw_with_model(config, rate, tracking, crate::config::BEATNET_MODEL)
}

/// Model override reserved for offline comparisons.
#[cfg(feature = "offline")]
pub fn create_raw_for_model(
    config: &Config,
    rate: u32,
    tracking: Tracking,
    model: u8,
) -> Result<Box<dyn TrackingBackend>, Error> {
    create_raw_with_model(config, rate, tracking, model)
}

fn create_raw_with_model(
    config: &Config,
    rate: u32,
    tracking: Tracking,
    model: u8,
) -> Result<Box<dyn TrackingBackend>, Error> {
    if !(1..=3).contains(&model) {
        return Err(Error::Config("model must be 1, 2, or 3".into()));
    }
    match tracking {
        Tracking::Pulseweave => Ok(Box::new(Pulseweave {
            stream: pulseweave::stream::MeterStream::new(rate as usize)?,
            origin: 0.,
            rate,
        })),
        Tracking::Beatnet | Tracking::Assisted => {
            let tracker = beatnet_rs::BeatNet::new(beatnet_rs::BeatNetConfig {
                sample_rate: rate,
                channels: 1,
                model: match model {
                    1 => beatnet_rs::Model::One,
                    2 => beatnet_rs::Model::Two,
                    _ => beatnet_rs::Model::Three,
                },
                tracker: beatnet_rs::TrackerConfig {
                    min_bpm: config.min_bpm,
                    max_bpm: config.max_bpm,
                    min_meter: config.min_meter,
                    max_meter: config.max_meter,
                    ..Default::default()
                },
            })?;
            Ok(Box::new(BeatNet {
                tracker,
                downbeat: None,
                subdivision: crate::config::ATOM_SUBDIVISION,
            }))
        }
    }
}
struct Pulseweave {
    stream: pulseweave::stream::MeterStream,
    origin: f64,
    rate: u32,
}
impl TrackingBackend for Pulseweave {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            detected_atom: true,
            ..Default::default()
        }
    }
    fn reset(&mut self, frame: u64) {
        self.origin = frame as f64 / self.rate as f64;
        self.stream.reset();
    }
    fn process(&mut self, mono: &[f32]) -> Result<Option<Estimate>, Error> {
        let mut result = None;
        self.process_each(mono, &mut |estimate| result = Some(estimate))?;
        Ok(result)
    }
    fn process_each(&mut self, mono: &[f32], emit: &mut dyn FnMut(Estimate)) -> Result<(), Error> {
        let origin = self.origin - self.stream.resampler_delay() as f64 / 44_100.;
        self.stream.process(mono, |update| {
            let m = update.meter;
            let grid = |anchor, period| PulseGrid {
                anchor: origin + anchor,
                period,
                provenance: Provenance::Detected,
            };
            let grids = [
                grid(m.bar_time, m.periods.bar),
                grid(m.beat_time, m.periods.beat),
                grid(m.tatum_time, m.periods.tatum),
            ];
            // Empty evidence produces fallback periods; those are not a tempo lock.
            if update.beatyness.is_finite()
                && update.beatyness > 0.
                && grids.iter().all(|g| g.valid())
            {
                emit(Estimate {
                    grids: grids.map(Some),
                    bpm: Some(60. / m.periods.beat),
                    meter: None,
                    quality: Quality::PulseweaveBeatyness(update.beatyness),
                    evidence: None,
                    holding: false,
                });
            }
        });
        Ok(())
    }
}
struct BeatNet {
    tracker: beatnet_rs::BeatNet,
    downbeat: Option<f64>,
    subdivision: u8,
}
impl TrackingBackend for BeatNet {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tempo_bounds: true,
            meter_bounds: true,
            model_selection: false,
            ..Default::default()
        }
    }
    fn reset(&mut self, frame: u64) {
        self.tracker.discontinuity(frame);
        self.downbeat = None;
    }
    fn process(&mut self, mono: &[f32]) -> Result<Option<Estimate>, Error> {
        let mut result = None;
        self.process_each(mono, &mut |estimate| result = Some(estimate))?;
        Ok(result)
    }
    fn process_each(&mut self, mono: &[f32], emit: &mut dyn FnMut(Estimate)) -> Result<(), Error> {
        self.tracker.process(mono, |frame| {
            if let Some(event) = frame.event.filter(|event| event.downbeat) {
                self.downbeat = Some(event.time);
            }
            if let (Some(bpm), Some(phase)) = (frame.state.bpm, frame.state.phase) {
                let period = 60. / f64::from(bpm);
                let anchor = frame.state.time - f64::from(phase) * period;
                let beat = PulseGrid {
                    anchor,
                    period,
                    provenance: Provenance::Detected,
                };
                let atom = PulseGrid {
                    period: period / f64::from(self.subdivision),
                    provenance: Provenance::Derived,
                    ..beat
                };
                let bar = self
                    .downbeat
                    .zip(frame.state.meter)
                    .map(|(anchor, meter)| PulseGrid {
                        anchor,
                        period: period * f64::from(meter),
                        provenance: Provenance::Derived,
                    });
                emit(Estimate {
                    grids: [bar, Some(beat), Some(atom)],
                    bpm: Some(f64::from(bpm)),
                    meter: frame.state.meter,
                    quality: Quality::BeatNetConfidence(frame.state.confidence),
                    holding: false,
                    evidence: Some(Evidence {
                        time: frame.state.time,
                        beat: frame.state.probabilities[0],
                        downbeat: frame.state.probabilities[1],
                        event: frame.event.is_some(),
                    }),
                });
            }
        })?;
        Ok(())
    }
}

/// An independent, stabilized tempo advisor. Staleness is measured on the source
/// timeline, never wall time, so replay and live processing make the same decisions.
pub struct TempoAdvisor {
    clock: crate::clock::BeatClock,
    grid: Option<PulseGrid>,
    updated: f64,
    consistent_since: f64,
}
impl TempoAdvisor {
    pub fn new(subdivision: u8) -> Self {
        Self {
            clock: crate::clock::BeatClock::new(subdivision),
            grid: None,
            updated: f64::NEG_INFINITY,
            consistent_since: f64::INFINITY,
        }
    }
    pub fn update(&mut self, raw: Estimate, time: f64) {
        let result = self.clock.update(raw, time, None);
        let previous = self.grid;
        self.grid = result.grids[1].zip(result.bpm).map(|(g, bpm)| PulseGrid {
            period: 60. / bpm,
            ..g
        });
        if result.holding {
            self.grid = None;
        }
        if self
            .grid
            .zip(previous)
            .is_none_or(|(new, old)| (new.period / old.period - 1.).abs() > 0.02)
        {
            self.consistent_since = time;
        }
        self.updated = time;
    }
    pub fn grid(&self, time: f64) -> Option<PulseGrid> {
        if time - self.updated <= 2. && time - self.consistent_since >= 2. {
            self.grid
        } else {
            None
        }
    }
}

struct StableBackend {
    raw: Box<dyn TrackingBackend>,
    advisor: Option<Box<dyn TrackingBackend>>,
    clock: crate::clock::BeatClock,
    advisory: TempoAdvisor,
    subdivision: u8,
    rate: u32,
    frame: u64,
    buffer: [f32; 128],
    filled: usize,
}
impl TrackingBackend for StableBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            detected_atom: false,
            ..self.raw.capabilities()
        }
    }
    fn reset(&mut self, source_frame: u64) {
        self.raw.reset(source_frame);
        if let Some(advisor) = &mut self.advisor {
            advisor.reset(source_frame);
        }
        self.clock.reset();
        self.advisory = TempoAdvisor::new(self.subdivision);
        self.frame = source_frame;
        self.filled = 0;
    }
    fn process(&mut self, mono: &[f32]) -> Result<Option<Estimate>, Error> {
        let mut result = None;
        self.process_each(mono, &mut |estimate| result = Some(estimate))?;
        Ok(result)
    }
    fn process_each(
        &mut self,
        mut mono: &[f32],
        emit: &mut dyn FnMut(Estimate),
    ) -> Result<(), Error> {
        // Fixed subdivisions make decisions independent of caller chunk sizes and
        // retain every neural frame, even at the minimum supported input rate.
        while !mono.is_empty() {
            let count = mono.len().min(self.buffer.len() - self.filled);
            self.buffer[self.filled..self.filled + count].copy_from_slice(&mono[..count]);
            self.filled += count;
            mono = &mono[count..];
            if self.filled != self.buffer.len() {
                continue;
            }
            self.frame += self.buffer.len() as u64;
            self.filled = 0;
            let time = self.frame as f64 / self.rate as f64;
            if let Some(advisor) = &mut self.advisor {
                advisor.process_each(&self.buffer, &mut |raw| self.advisory.update(raw, time))?;
            }
            let guide = self.advisory.grid(time);
            let clock = &mut self.clock;
            self.raw.process_each(&self.buffer, &mut |raw| {
                let result = clock.update(raw, time, guide);
                if result.bpm.is_some() {
                    emit(result);
                }
            })?;
        }
        Ok(())
    }
}
