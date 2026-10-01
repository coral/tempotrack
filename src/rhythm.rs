//! Protocol-neutral estimates and pure projection helpers. No wall clock reads here.
use std::time::{Duration, Instant};

use crate::config::Tracking;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Provenance {
    Detected,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PulseGrid {
    /// Source seconds at pulse zero. May be negative during resampler startup.
    pub anchor: f64,
    pub period: f64,
    pub provenance: Provenance,
}
impl PulseGrid {
    pub fn valid(self) -> bool {
        self.anchor.is_finite() && self.period.is_finite() && self.period > 0.
    }
    pub fn position(self, source_time: f64) -> f64 {
        (source_time - self.anchor) / self.period
    }
    pub fn phase(self, source_time: f64) -> f64 {
        self.position(source_time).rem_euclid(1.)
    }
    pub fn next_boundary(self, source_time: f64) -> f64 {
        self.anchor + (self.position(source_time).floor() + 1.) * self.period
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Quality {
    Unknown,
    BeatNetConfidence(f32),
    PulseweaveBeatyness(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Initializing,
    Listening,
    Tracking,
    Silence,
    Holdover,
    Stopped,
    Error,
}
impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RhythmSnapshot {
    pub generation: u64,
    pub sequence: u64,
    pub source_frame: u64,
    pub sample_rate: u32,
    pub reference_time: Instant,
    pub source_time: f64,
    pub valid_until: Instant,
    pub backend: Tracking,
    pub transport: Transport,
    pub bpm: Option<f64>,
    pub meter: Option<u8>,
    /// Bar, beat, atom. Each grid has an independent anchor and period.
    pub grids: [Option<PulseGrid>; 3],
    pub quality: Quality,
    pub level_db: f32,
    pub peak: f32,
    pub offset_seconds: f64,
    pub dropped_frames: u64,
}
impl RhythmSnapshot {
    pub fn empty(now: Instant, backend: Tracking) -> Self {
        Self {
            generation: 0,
            sequence: 0,
            source_frame: 0,
            sample_rate: 0,
            reference_time: now,
            source_time: 0.,
            valid_until: now,
            backend,
            transport: Transport::Stopped,
            bpm: None,
            meter: None,
            grids: [None; 3],
            quality: Quality::Unknown,
            level_db: -96.,
            peak: 0.,
            offset_seconds: 0.,
            dropped_frames: 0,
        }
    }
    pub fn active(&self, now: Instant) -> bool {
        now <= self.valid_until
            && matches!(self.transport, Transport::Tracking | Transport::Holdover)
    }
    pub fn time_at(&self, now: Instant) -> f64 {
        let elapsed = if now >= self.reference_time {
            now.duration_since(self.reference_time).as_secs_f64()
        } else {
            -self.reference_time.duration_since(now).as_secs_f64()
        };
        self.source_time + elapsed - self.offset_seconds
    }
    pub fn phase(&self, level: usize, now: Instant) -> Option<f64> {
        if !self.active(now) {
            return None;
        }
        self.grids
            .get(level)
            .copied()
            .flatten()
            .filter(|g| g.valid())
            .map(|g| g.phase(self.time_at(now)))
    }
    pub fn beat_position(&self, now: Instant) -> Option<f64> {
        if !self.active(now) {
            return None;
        }
        self.grids[1]
            .filter(|g| g.valid())
            .map(|g| g.position(self.time_at(now)))
    }
}

/// A lossy live pulse cursor: never catches up historical events after a stall.
#[derive(Debug, Default)]
pub struct PulseCursor {
    generation: Option<u64>,
    previous: [Option<f64>; 3],
    emitted: [Option<f64>; 3],
}
impl PulseCursor {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn poll(&mut self, snapshot: &RhythmSnapshot, now: Instant) -> [bool; 3] {
        if !snapshot.active(now) {
            self.reset();
            return [false; 3];
        }
        if self.generation != Some(snapshot.generation) {
            self.reset();
            self.generation = Some(snapshot.generation);
        }
        let time = snapshot.time_at(now);
        std::array::from_fn(|i| {
            let Some(grid) = snapshot.grids[i].filter(|g| g.valid()) else {
                self.previous[i] = None;
                return false;
            };
            let boundary = grid.anchor + grid.position(time).floor() * grid.period;
            let previous = self.previous[i].replace(time);
            // Small phase corrections cannot retrigger the same physical pulse.
            let new = self.emitted[i].is_none_or(|last| boundary - last >= grid.period * 0.5);
            let crossed = previous.is_some_and(|last| boundary > last);
            let recent = time - boundary <= 0.08_f64.min(grid.period * 0.4);
            if crossed && new && recent {
                self.emitted[i] = Some(boundary);
                true
            } else {
                false
            }
        })
    }
    pub fn next_deadline(snapshot: &RhythmSnapshot, now: Instant) -> Option<Instant> {
        if !snapshot.active(now) {
            return None;
        }
        let time = snapshot.time_at(now);
        snapshot
            .grids
            .iter()
            .flatten()
            .filter(|g| g.valid())
            .map(|g| now + Duration::from_secs_f64((g.next_boundary(time) - time).max(0.0001)))
            .min()
    }
}
