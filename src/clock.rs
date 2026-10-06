//! A causal beat clock. Detector hypotheses are observations, never clock resets.
//! All history is bounded and all timing is in source seconds.
use crate::{
    backend::{Estimate, Evidence},
    rhythm::{Provenance, PulseGrid},
};

const HISTORY: usize = 128;
const WINDOW: f64 = 12.;
#[derive(Clone, Copy, Default)]
struct Point {
    time: f64,
    weight: f64,
}
#[derive(Clone, Copy)]
struct Fit {
    grid: PulseGrid,
    score: f64,
    count: usize,
    spread: f64,
    latest: f64,
    recent_beats: u32,
}

#[derive(Clone, Copy)]
struct Candidate {
    grid: PulseGrid,
    since: f64,
    kind: Change,
}

#[derive(Clone, Copy, PartialEq)]
enum Change {
    Tempo,
    HalfTime,
    Phase,
}

#[derive(Clone, Copy)]
struct BarCandidate {
    offset: f64,
    meter: u8,
    last_downbeat: f64,
    confirmations: u8,
}

pub struct BeatClock {
    points: [Point; HISTORY],
    count: usize,
    next: usize,
    peaks: [Option<Evidence>; 2],
    last_peak: f64,
    last_fit: f64,
    model: Option<PulseGrid>,
    output: Option<PulseGrid>,
    candidate: Option<Candidate>,
    last_good: f64,
    acquired_at: f64,
    subdivision: u8,
    bar_offset: Option<f64>,
    meter: Option<u8>,
    meter_bounds: (u8, u8),
    last_downbeat: Option<f64>,
    bar_candidate: Option<BarCandidate>,
    atom_divisor: Option<f64>,
    outside_since: Option<(f64, f64)>,
    half_vote: Option<(PulseGrid, f64)>,
    bounds: Option<(f64, f64)>,
}
impl BeatClock {
    pub fn new(subdivision: u8) -> Self {
        Self {
            points: [Point::default(); HISTORY],
            count: 0,
            next: 0,
            peaks: [None; 2],
            last_peak: f64::NEG_INFINITY,
            last_fit: f64::NEG_INFINITY,
            model: None,
            output: None,
            candidate: None,
            last_good: 0.,
            acquired_at: 0.,
            subdivision,
            bar_offset: None,
            meter: None,
            meter_bounds: (2, 12),
            last_downbeat: None,
            bar_candidate: None,
            atom_divisor: None,
            outside_since: None,
            half_vote: None,
            bounds: None,
        }
    }
    /// Respect explicit detector tempo bounds when ranking continuous clock fits.
    pub fn for_config(config: &crate::config::Config) -> Self {
        let mut clock = Self::new(crate::config::ATOM_SUBDIVISION);
        clock.bounds = Some((config.min_bpm, config.max_bpm));
        clock.meter_bounds = (config.min_meter, config.max_meter);
        clock
    }
    pub fn reset(&mut self) {
        let bounds = self.bounds;
        let meter_bounds = self.meter_bounds;
        *self = Self::new(self.subdivision);
        self.bounds = bounds;
        self.meter_bounds = meter_bounds;
    }
    /// Correct the bar label only after distinct downbeats corroborate it.
    /// The continuous beat/atom clocks never move when beat one is relabeled.
    fn observe_bar(&mut self, raw: Estimate, beat: PulseGrid, now: f64) {
        let Some(bar) = raw.grids[0].filter(|g| g.valid()) else {
            return;
        };
        if bar.anchor > now
            || now - bar.anchor > beat.period * 1.5
            || self
                .last_downbeat
                .is_some_and(|last| bar.anchor <= last + beat.period * 0.5)
        {
            return;
        }
        self.last_downbeat = Some(bar.anchor);
        // A raw octave error must not reinterpret bar length on the stable clock.
        if raw.grids[1].is_none_or(|g| !g.valid() || (g.period / beat.period - 1.).abs() > 0.12) {
            self.bar_candidate = None;
            return;
        }
        let meter = raw
            .meter
            .unwrap_or_else(|| (bar.period / beat.period).round().clamp(2., 12.) as u8);
        let position = beat.position(bar.anchor);
        if !(self.meter_bounds.0..=self.meter_bounds.1).contains(&meter)
            || (position - position.round()).abs() > 0.2
        {
            self.bar_candidate = None;
            return;
        }
        let offset = position.round().rem_euclid(f64::from(meter));
        if self.bar_offset.is_none() {
            self.bar_offset = Some(offset);
            self.meter = Some(meter);
            return;
        }
        if self.meter == Some(meter) && self.bar_offset == Some(offset) {
            self.bar_candidate = None;
            return;
        }
        let confirmations = self
            .bar_candidate
            .filter(|candidate| {
                let elapsed_beats = (bar.anchor - candidate.last_downbeat) / beat.period;
                candidate.meter == meter
                    && candidate.offset == offset
                    && elapsed_beats >= f64::from(meter) - 0.25
                    && elapsed_beats <= f64::from(meter) * 2.5
            })
            .map_or(1, |candidate| candidate.confirmations + 1);
        if confirmations >= 3 {
            self.meter = Some(meter);
            self.bar_offset = Some(offset);
            self.bar_candidate = None;
        } else {
            self.bar_candidate = Some(BarCandidate {
                offset,
                meter,
                last_downbeat: bar.anchor,
                confirmations,
            });
        }
    }
    fn push(&mut self, time: f64, weight: f64) {
        if !time.is_finite() || !weight.is_finite() || weight <= 0. {
            return;
        }
        self.points[self.next] = Point { time, weight };
        self.next = (self.next + 1) % HISTORY;
        self.count = (self.count + 1).min(HISTORY);
    }
    fn observe(&mut self, raw: Estimate, now: f64) {
        if let Some(e) = raw.evidence {
            if let [Some(a), Some(b)] = self.peaks {
                let strength = |p: Evidence| f64::from(p.beat.max(p.downbeat));
                let (left, center, right) = (strength(a), strength(b), strength(e));
                if center >= 0.4
                    && center > left
                    && center >= right
                    && b.time - self.last_peak > 0.09
                {
                    // A three-frame parabolic peak removes the 20 ms tempo quantization.
                    // It uses only frames already received, adding one frame of observation latency.
                    let curvature = left - 2. * center + right;
                    let shift = if curvature.abs() > 1e-6 {
                        (0.5 * (left - right) / curvature).clamp(-0.5, 0.5)
                    } else {
                        0.
                    };
                    let time = b.time + shift * (e.time - b.time);
                    self.push(time, center);
                    self.last_peak = time;
                }
            }
            self.peaks = [self.peaks[1], Some(e)];
        } else if let Some(grid) = raw.grids[1].filter(|g| g.valid()) {
            // Resonator phase reports locate the most recent beat. Do not treat
            // repeated reports of that same beat as independent evidence.
            let time = grid.anchor + grid.position(now).floor() * grid.period;
            if time - self.last_peak > grid.period * 0.5 {
                self.push(time, 1.);
                self.last_peak = time;
            }
        }
    }
    fn fit(&self, mut grid: PulseGrid, now: f64) -> Option<Fit> {
        for _ in 0..3 {
            let (mut weight, mut sx, mut sy, mut sxx, mut sxy) = (0., 0., 0., 0., 0.);
            let (mut first, mut last) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut count = 0;
            // Center the coordinates close to now for numerical stability on long sessions.
            grid.anchor += grid.position(now).round() * grid.period;
            for p in &self.points[..self.count] {
                if p.time < now - WINDOW || p.time > now {
                    continue;
                }
                let x = grid.position(p.time).round();
                let y = p.time - grid.anchor;
                let error = y - x * grid.period;
                if error.abs() > grid.period * 0.14 {
                    continue;
                }
                let w = p.weight * (0.025 / error.abs().max(0.025));
                weight += w;
                sx += w * x;
                sy += w * y;
                sxx += w * x * x;
                sxy += w * x * y;
                first = first.min(p.time);
                last = last.max(p.time);
                count += 1;
            }
            let denominator = weight * sxx - sx * sx;
            if count < 6 || last - first < grid.period * 4. || denominator <= 1e-8 {
                return None;
            }
            let period = (weight * sxy - sx * sy) / denominator;
            if !(grid.period * 0.94..grid.period * 1.06).contains(&period) {
                return None;
            }
            let anchor = grid.anchor + (sy - period * sx) / weight;
            grid = PulseGrid {
                anchor,
                period,
                provenance: Provenance::Detected,
            };
        }
        if self
            .bounds
            .is_some_and(|(min, max)| !(min * 0.99..=max * 1.01).contains(&(60. / grid.period)))
        {
            return None;
        }
        // Evaluate support and timing error on the final grid, not on the
        // pre-regression seed. Otherwise seed choice changes confidence even
        // when two searches converge on the same beat positions.
        let (mut total, mut matched, mut residual, mut weight) = (0., 0., 0., 0.);
        let (mut first, mut latest) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut count = 0;
        let mut recent = 0_u8;
        let last = grid.position(now).floor();
        for point in &self.points[..self.count] {
            if point.time < now - WINDOW || point.time > now {
                continue;
            }
            total += point.weight;
            let position = grid.position(point.time);
            let error = (position - position.round()) * grid.period;
            if error.abs() > grid.period * 0.14 {
                continue;
            }
            let w = point.weight * (0.025 / error.abs().max(0.025));
            weight += w;
            matched += point.weight;
            residual += w * error * error;
            first = first.min(point.time);
            latest = latest.max(point.time);
            count += 1;
            // Distinct recent beat slots measure presence separately from
            // salience, so louder offbeats cannot erase quieter original beats.
            let slot = last - position.round();
            if (0. ..6.).contains(&slot) {
                recent |= 1 << slot as u8;
            }
        }
        if count < 6 || latest - first < grid.period * 4. {
            return None;
        }
        let spread = (residual / weight).sqrt();
        let coverage = (matched / ((latest - first) / grid.period + 1.)).min(1.);
        let bpm = 60. / grid.period;
        let outside = if bpm < 100. {
            (100. / bpm).ln()
        } else if bpm > 200. {
            (bpm / 200.).ln()
        } else {
            0.
        };
        // Soft prior, never a clamp: strong evidence can establish other tempos.
        let score =
            (0.7 * coverage + 0.3 * matched / total.max(1e-9)) / (1. + 2. * outside).min(4. / 3.);
        Some(Fit {
            grid,
            score,
            count,
            spread,
            latest,
            recent_beats: recent.count_ones(),
        })
    }
    fn search(&self, seed: PulseGrid, now: f64) -> Option<Fit> {
        let mut best: Option<Fit> = None;
        // Search nearby continuous tempos and several observed phases. A raw
        // detector's quantized period is only a prior, not the output period.
        for factor in [0.96, 0.98, 1., 1.02, 1.04] {
            for p in self.points[..self.count]
                .iter()
                .filter(|p| p.time > now - 4.)
                .step_by(2)
            {
                let grid = PulseGrid {
                    anchor: p.time,
                    period: seed.period * factor,
                    ..seed
                };
                if let Some(fit) = self.fit(grid, now)
                    && best.is_none_or(|b| fit.score > b.score)
                {
                    best = Some(fit);
                }
            }
        }
        best
    }
    /// `guide` is an optional independent tempo hypothesis. It affects candidate
    /// search only; phase and tempo still have to fit observed beats.
    pub fn update(&mut self, raw: Estimate, now: f64, guide: Option<PulseGrid>) -> Estimate {
        self.observe(raw, now);
        if now - self.last_fit >= 0.25 {
            self.last_fit = now;
            let incumbent = self.model.and_then(|g| self.fit(g, now));
            let mut challenger = raw.grids[1].and_then(|g| self.search(g, now));
            if let Some(seed) = raw.grids[1] {
                let bpm = 60. / seed.period;
                let factor = if bpm < 95. {
                    0.5
                } else if bpm >= 220. || (bpm > 200. && raw.evidence.is_some()) {
                    2.
                } else {
                    1.
                };
                let since = self
                    .outside_since
                    .filter(|(old, _)| *old == factor)
                    .map_or(now, |(_, time)| time);
                self.outside_since = (factor != 1.).then_some((factor, since));
                if factor != 1.
                    && now - since >= 3.
                    && let Some(fit) = self.search(
                        PulseGrid {
                            period: seed.period * factor,
                            ..seed
                        },
                        now,
                    )
                    // Near the upper edge of the soft prior, sparse fast music
                    // is still plausible. Require strong neural support before
                    // proposing half-time; keep resonator-only behavior unchanged.
                    && (bpm >= 220. || factor == 0.5 || fit.score >= 0.8)
                    && challenger.is_none_or(|old| fit.score > old.score)
                {
                    challenger = Some(fit);
                }
            }
            // An advisor supplies a hypothesis only when our own observations do
            // not already support a strong lock. Agreement is not independent beat
            // evidence, and an octave disagreement is not evidence of a tempo change.
            let reference = self.model.or(raw.grids[1]);
            let advisory = guide.filter(|g| {
                reference.is_some_and(|r| {
                    let ratio = r.period / g.period;
                    (0.72..1.38).contains(&ratio)
                }) && raw.grids[1].is_none_or(|r| {
                    let ratio = r.period / g.period;
                    (ratio - 0.5).abs() > 0.03 && (ratio - 2.).abs() > 0.12
                })
            });
            if incumbent.is_none_or(|f| f.score < 0.45)
                && let Some(other) = advisory.and_then(|g| self.search(g, now))
                && challenger.is_none_or(|f| other.score > f.score + 0.05)
            {
                challenger = Some(other);
            }
            // Remember brief particle endorsements so frame-to-frame octave
            // jitter does not erase otherwise consistent evidence. A half-time
            // advisor cannot overrule particles that keep supporting fast beats.
            if let Some(grid) = raw.grids[1]
                && self
                    .model
                    .is_some_and(|old| (old.period / grid.period - 0.5).abs() < 0.03)
            {
                self.half_vote = Some((grid, now));
            }
            let recent_vote = self.half_vote.filter(|(_, time)| now - time <= 2.);
            let half_advisory = guide.filter(|g| {
                raw.evidence.is_some()
                    && self
                        .model
                        .is_some_and(|old| (old.period / g.period - 0.5).abs() < 0.03)
                    && recent_vote
                        .is_some_and(|(vote, _)| (vote.period / g.period - 1.).abs() < 0.06)
            });
            if let Some(other) = half_advisory.and_then(|g| self.search(g, now))
                && other.score >= 0.8
                && incumbent.is_none_or(|old| other.score > old.score + 0.12)
                && challenger.is_none_or(|old| other.score > old.score + 0.05)
            {
                challenger = Some(other);
            }
            if let Some(fit) = incumbent.filter(|f| {
                f.score
                    >= if advisory.is_some_and(|g| (g.period / f.grid.period - 1.).abs() < 0.015) {
                        0.4
                    } else {
                        0.45
                    }
                    && f.spread < f.grid.period * 0.09
                    && now - f.latest < f.grid.period * 3.
            }) {
                self.model = Some(fit.grid);
                self.last_good = now;
            }
            if let Some(fit) = challenger.filter(|f| {
                // Independent tempo agreement lowers the support threshold only
                // while acquiring/recovering. It never supplies phase observations.
                let corroborated = incumbent.is_none_or(|old| old.score < 0.45)
                    && advisory.is_some_and(|g| (g.period / f.grid.period - 1.).abs() < 0.015);
                let precise_start = self.model.is_none()
                    && raw.evidence.is_some()
                    && raw.grids[1].is_some_and(|g| (g.period / f.grid.period - 1.).abs() < 0.025)
                    && f.spread < f.grid.period * 0.035
                    && f.recent_beats >= 3;
                f.score
                    >= if corroborated {
                        0.4
                    } else if precise_start {
                        0.45
                    } else {
                        0.55
                    }
                    && f.count >= 6
                    && f.spread < f.grid.period * 0.07
                    && now - f.latest < f.grid.period * 3.
            }) {
                let harmonic = self.model.is_some_and(|old| {
                    let ratio = old.period / fit.grid.period;
                    (ratio - 2.).abs() < 0.12 || (ratio - 0.5).abs() < 0.03
                });
                // A double-time lock can keep fitting every second tick even after
                // the detector has settled on the actual beat. Recover only when
                // the slower grid explains the neural peaks substantially better
                // and either the particles agree or strong neural support backs
                // an independent advisor. Agreement never supplies beat phase.
                let corroborated_half =
                    guide.is_some_and(|g| (g.period / fit.grid.period - 1.).abs() < 0.025);
                let half_time_correction = raw.evidence.is_some()
                    && self
                        .model
                        .is_some_and(|old| (old.period / fit.grid.period - 0.5).abs() < 0.03)
                    && (raw.grids[1]
                        .is_some_and(|g| (g.period / fit.grid.period - 1.).abs() < 0.06)
                        || (corroborated_half
                            && fit.score >= 0.8
                            && recent_vote.is_some_and(|(vote, _)| {
                                (vote.period / fit.grid.period - 1.).abs() < 0.06
                            })))
                    && incumbent.is_some_and(|old| {
                        old.score >= 0.4
                            && fit.score > old.score + if corroborated_half { 0.12 } else { 0.2 }
                    });
                let ambiguous = harmonic
                    && incumbent.is_some_and(|old| old.score >= 0.4)
                    && !half_time_correction;
                let phase_replacement = self.model.is_some_and(|old| {
                    (old.period / fit.grid.period - 1.).abs() < 0.03
                        && phase_error(old, fit.grid, now).abs() > 0.18
                });
                let supported_phase = phase_replacement
                    && incumbent.is_some_and(|old| {
                        (old.grid.period / fit.grid.period - 1.).abs() < 0.03
                            && phase_error(old.grid, fit.grid, now).abs() > 0.18
                            && old.recent_beats >= 3
                            && old.spread < old.grid.period * 0.07
                    });
                let better = !ambiguous
                    && !supported_phase
                    && incumbent.is_none_or(|old| fit.score > old.score + 0.12);
                if self.model.is_none() {
                    // Low-amplitude but precise neural beats can establish a
                    // tempo if they persist. This does not require the advisor
                    // to agree (it may hear half-time in genuine fast music).
                    let since = self
                        .candidate
                        .filter(|old| {
                            (old.grid.period / fit.grid.period - 1.).abs() < 0.025
                                && phase_error(old.grid, fit.grid, now).abs() < 0.12
                        })
                        .map_or(now, |old| old.since);
                    self.candidate = Some(Candidate {
                        grid: fit.grid,
                        since,
                        kind: Change::Tempo,
                    });
                    let corroborated =
                        advisory.is_some_and(|g| (g.period / fit.grid.period - 1.).abs() < 0.015);
                    if fit.score >= 0.55 || corroborated || now - since >= 2. {
                        self.model = Some(fit.grid);
                        self.last_good = now;
                        self.acquired_at = now;
                        self.candidate = None;
                    }
                } else if better {
                    let kind = if half_time_correction {
                        Change::HalfTime
                    } else if phase_replacement {
                        Change::Phase
                    } else {
                        Change::Tempo
                    };
                    let since = self
                        .candidate
                        .filter(|old| {
                            old.kind == kind
                                && (old.grid.period / fit.grid.period - 1.).abs() < 0.025
                                && phase_error(old.grid, fit.grid, now).abs() < 0.12
                        })
                        .map_or(now, |old| old.since);
                    self.candidate = Some(Candidate {
                        grid: fit.grid,
                        since,
                        kind,
                    });
                    // An offbeat-only passage need not change musical phase.
                    // Give the established phase sixteen beats to reappear;
                    // actual tempo changes retain the faster recovery path.
                    let confirmation = match kind {
                        // Before one full observation window, a cold-start lock
                        // is provisional. Strong neural evidence plus independent
                        // tempo agreement can correct its octave sooner. Mature
                        // locks retain the longer confirmation requirement.
                        Change::HalfTime
                            if since - self.acquired_at < WINDOW
                                && corroborated_half
                                && fit.score >= 0.8 =>
                        {
                            2.
                        }
                        Change::HalfTime => 6.,
                        Change::Phase => fit.grid.period * 16.,
                        Change::Tempo => 2.,
                    };
                    if now - since >= confirmation {
                        self.model = Some(fit.grid);
                        self.last_good = now;
                        self.candidate = None;
                    }
                } else {
                    self.candidate = None;
                }
            } else {
                self.candidate = None;
            }
        }
        let Some(model) = self.model else {
            return Estimate {
                quality: raw.quality,
                evidence: raw.evidence,
                ..Default::default()
            };
        };
        let beat = if let Some(old) = self.output {
            let position = old.position(now);
            // Change frequency around the current phase, so phase never jumps.
            // A phase disagreement is paid back over seconds, capped at 2% speed.
            let error = phase_error(old, model, now);
            let correction = if now - self.last_good <= WINDOW {
                (error / 3.).clamp(-0.02 / model.period, 0.02 / model.period)
            } else {
                0.
            };
            let period = 1. / (1. / model.period + correction);
            PulseGrid {
                anchor: now - position * period,
                period,
                ..model
            }
        } else {
            model
        };
        self.output = Some(beat);
        self.observe_bar(raw, beat, now);
        let bar = self
            .bar_offset
            .zip(self.meter)
            .map(|(offset, meter)| PulseGrid {
                anchor: beat.anchor + offset * beat.period,
                period: beat.period * f64::from(meter),
                provenance: Provenance::Derived,
            });
        let divisor = *self.atom_divisor.get_or_insert_with(|| {
            if raw.evidence.is_none() {
                raw.grids[1]
                    .zip(raw.grids[2])
                    .map_or(f64::from(self.subdivision), |(beat, atom)| {
                        (beat.period / atom.period).round().clamp(1., 8.)
                    })
            } else {
                f64::from(self.subdivision)
            }
        });
        let atom = PulseGrid {
            period: beat.period / divisor,
            provenance: Provenance::Derived,
            ..beat
        };
        Estimate {
            grids: [bar, Some(beat), Some(atom)],
            bpm: Some(60. / model.period),
            meter: self.meter,
            quality: raw.quality,
            evidence: raw.evidence,
            holding: now - self.last_good > 2.,
        }
    }
}
fn phase_error(old: PulseGrid, target: PulseGrid, now: f64) -> f64 {
    (target.position(now) - old.position(now) + 0.5).rem_euclid(1.) - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residual_is_measured_against_the_returned_grid() {
        let mut clock = BeatClock::new(4);
        for beat in 0..24 {
            let jitter = [0.055, -0.03, 0.012, -0.055, 0.025][beat % 5];
            clock.push(beat as f64 * 0.5 + jitter, 0.9);
        }
        for period in [0.499, 0.5, 0.501] {
            let fit = clock
                .fit(
                    PulseGrid {
                        anchor: 0.03,
                        period,
                        provenance: Provenance::Detected,
                    },
                    12.,
                )
                .expect("enough coherent beats for a fit");
            let (mut weight, mut residual) = (0., 0.);
            for point in &clock.points[..clock.count] {
                let position = fit.grid.position(point.time);
                let error = (position - position.round()) * fit.grid.period;
                if error.abs() <= fit.grid.period * 0.14 {
                    let w = point.weight * 0.025 / error.abs().max(0.025);
                    weight += w;
                    residual += w * error * error;
                }
            }
            assert!((fit.spread - (residual / weight).sqrt()).abs() < 1e-12);
        }
    }
}
