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
    candidate: Option<(PulseGrid, f64)>,
    last_good: f64,
    subdivision: u8,
    bar_offset: Option<f64>,
    meter: Option<u8>,
    atom_divisor: Option<f64>,
    outside_since: Option<(f64, f64)>,
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
            subdivision,
            bar_offset: None,
            meter: None,
            atom_divisor: None,
            outside_since: None,
            bounds: None,
        }
    }
    /// Respect explicit detector tempo bounds when ranking continuous clock fits.
    pub fn for_config(config: &crate::config::Config) -> Self {
        let mut clock = Self::new(crate::config::ATOM_SUBDIVISION);
        clock.bounds = Some((config.min_bpm, config.max_bpm));
        clock
    }
    pub fn reset(&mut self) {
        let bounds = self.bounds;
        *self = Self::new(self.subdivision);
        self.bounds = bounds;
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
        let mut count = 0;
        let mut spread = 0.;
        let mut score = 0.;
        let mut latest = f64::NEG_INFINITY;
        for _ in 0..3 {
            let (mut weight, mut sx, mut sy, mut sxx, mut sxy) = (0., 0., 0., 0., 0.);
            let (mut total, mut matched, mut residual) = (0., 0., 0.);
            let (mut first, mut last) = (f64::INFINITY, f64::NEG_INFINITY);
            count = 0;
            // Center the coordinates close to now for numerical stability on long sessions.
            grid.anchor += grid.position(now).round() * grid.period;
            for p in &self.points[..self.count] {
                if p.time < now - WINDOW || p.time > now {
                    continue;
                }
                total += p.weight;
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
                matched += p.weight;
                residual += w * error * error;
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
            latest = last;
            let anchor = grid.anchor + (sy - period * sx) / weight;
            spread = (residual / weight).sqrt();
            // Reward support on predicted beats as well as explaining observations.
            let coverage = (matched / ((last - first) / period + 1.)).min(1.);
            let bpm = 60. / period;
            let outside = if bpm < 100. {
                (100. / bpm).ln()
            } else if bpm > 200. {
                (bpm / 200.).ln()
            } else {
                0.
            };
            // A soft electronic-music prior, not a clamp: strong evidence may
            // still establish a tempo outside the preferred 100–200 BPM band.
            score = (0.7 * coverage + 0.3 * matched / total.max(1e-9))
                / (1. + 2. * outside).min(4. / 3.);
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
        Some(Fit {
            grid,
            score,
            count,
            spread,
            latest,
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
                } else if bpm >= 220. {
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
                })
            });
            if incumbent.is_none_or(|f| f.score < 0.45)
                && let Some(other) = advisory.and_then(|g| self.search(g, now))
                && challenger.is_none_or(|f| other.score > f.score + 0.05)
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
                f.score >= if corroborated { 0.4 } else { 0.55 }
                    && f.count >= 6
                    && f.spread < f.grid.period * 0.07
                    && now - f.latest < f.grid.period * 3.
            }) {
                let harmonic = self.model.is_some_and(|old| {
                    let ratio = old.period / fit.grid.period;
                    (ratio - 2.).abs() < 0.12 || (ratio - 0.5).abs() < 0.03
                });
                // Strong subdivisions do not prove that the musical beat doubled.
                // Preserve a supported metrical level until its evidence disappears.
                let ambiguous = harmonic && incumbent.is_some_and(|old| old.score >= 0.4);
                let better = !ambiguous && incumbent.is_none_or(|old| fit.score > old.score + 0.12);
                if self.model.is_none() {
                    self.model = Some(fit.grid);
                    self.last_good = now;
                } else if better {
                    let since = self
                        .candidate
                        .filter(|(old, _)| {
                            (old.period / fit.grid.period - 1.).abs() < 0.025
                                && phase_error(*old, fit.grid, now).abs() < 0.12
                        })
                        .map_or(now, |(_, since)| since);
                    self.candidate = Some((fit.grid, since));
                    if now - since >= 2. {
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
        // Keep a single coherent beat counter for bar and subdivisions. A noisy
        // meter/downbeat hypothesis must not move a running bar counter.
        if self.bar_offset.is_none()
            && let Some(bar) = raw.grids[0]
        {
            let meter = raw
                .meter
                .unwrap_or_else(|| (bar.period / model.period).round().clamp(2., 12.) as u8);
            self.meter = Some(meter);
            self.bar_offset = Some(
                beat.position(bar.anchor)
                    .round()
                    .rem_euclid(f64::from(meter)),
            );
        }
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
