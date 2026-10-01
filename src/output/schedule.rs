//! Pure monotonic projection shared by all clock outputs. No sleeping or I/O.
use crate::rhythm::{PulseGrid, RhythmSnapshot};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub index: i64,
    pub deadline: Instant,
    /// Tick deadlines discarded since the previous emitted tick.
    pub missed: u64,
}
#[derive(Debug)]
pub struct ClockCursor {
    ppqn: u32,
    generation: Option<u64>,
    last: Option<i64>,
    missed: u64,
}
impl ClockCursor {
    pub fn new(ppqn: u32) -> Self {
        assert!(ppqn > 0);
        Self {
            ppqn,
            generation: None,
            last: None,
            missed: 0,
        }
    }
    pub fn reset(&mut self) {
        self.generation = None;
        self.last = None;
        self.missed = 0;
    }
    fn position(&self, snapshot: &RhythmSnapshot, now: Instant) -> Option<(PulseGrid, f64, i64)> {
        if !snapshot.active(now) {
            return None;
        }
        let grid = snapshot.grids[1].filter(|g| g.valid())?;
        let interval = grid.period / f64::from(self.ppqn);
        let time = snapshot.time_at(now);
        let position = grid.position(time) * f64::from(self.ppqn);
        if !(1e-6..=3600.).contains(&interval) || !position.is_finite() || position.abs() > 9e15 {
            return None;
        }
        Some((grid, time, (position + 1e-9).floor() as i64))
    }
    pub fn poll(&mut self, snapshot: &RhythmSnapshot, now: Instant) -> Option<Tick> {
        let Some((grid, time, index)) = self.position(snapshot, now) else {
            self.reset();
            return None;
        };
        if self.generation != Some(snapshot.generation) {
            self.reset();
            self.generation = Some(snapshot.generation);
        }
        let Some(last) = self.last else {
            self.last = Some(index);
            return None;
        };
        if index <= last {
            return None;
        }
        self.last = Some(index);
        self.missed = self.missed.saturating_add((index - last - 1) as u64);
        let interval = grid.period / f64::from(self.ppqn);
        let elapsed = (time - (grid.anchor + index as f64 * interval)).max(0.);
        // An old boundary isn't a reason to compress a burst into the next interval.
        if elapsed > interval * 0.5 {
            self.missed = self.missed.saturating_add(1);
            return None;
        }
        let deadline = now.checked_sub(Duration::from_secs_f64(elapsed))?;
        Some(Tick {
            index,
            deadline,
            missed: std::mem::take(&mut self.missed),
        })
    }
    pub fn next_deadline(&self, snapshot: &RhythmSnapshot, now: Instant) -> Option<Instant> {
        let (grid, time, index) = self.position(snapshot, now)?;
        let next_index = (index + 1).max(self.last.map_or(index + 1, |last| last + 1));
        let remaining = grid.anchor + next_index as f64 * grid.period / f64::from(self.ppqn) - time;
        if !remaining.is_finite() || remaining > 3600. {
            return None;
        }
        now.checked_add(Duration::from_secs_f64(remaining.max(1e-6)))
    }
}
