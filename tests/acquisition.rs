use tempotrack::{
    backend::{Estimate, Evidence},
    clock::BeatClock,
    rhythm::{Provenance, PulseGrid, Quality},
};

const PERIOD: f64 = 0.3;
const ANCHOR: f64 = 0.1;

fn grid(period: f64) -> PulseGrid {
    PulseGrid {
        anchor: ANCHOR,
        period,
        provenance: Provenance::Detected,
    }
}

fn pulse(time: f64, center: f64) -> f64 {
    (-0.5 * ((time - center) / 0.008).powi(2)).exp()
}

fn strength(time: f64, jitter: bool) -> f64 {
    let beat = ((time - ANCHOR) / PERIOD).round() as i32;
    let displacement = if jitter {
        [0.06, -0.07, 0.1, -0.12][beat.rem_euclid(4) as usize] * PERIOD
    } else {
        0.
    };
    let primary = 0.5 * pulse(time, ANCHOR + f64::from(beat) * PERIOD + displacement);
    let subdivision = ((time - ANCHOR - PERIOD * 0.5) / PERIOD).round();
    // Extra weak onsets reduce confidence without making the primary beats
    // temporally imprecise. A clean isolated 0.5 peak train already passes the
    // ordinary score threshold and would not exercise provisional acquisition.
    let competing = 0.42 * pulse(time, ANCHOR + (subdivision + 0.5) * PERIOD);
    primary.max(competing)
}

fn observation(time: f64, strength: f64) -> Estimate {
    Estimate {
        grids: [None, Some(grid(PERIOD)), None],
        bpm: Some(200.),
        quality: Quality::BeatNetConfidence(0.5),
        evidence: Some(Evidence {
            time,
            beat: strength as f32,
            downbeat: 0.,
            event: strength > 0.4,
        }),
        ..Estimate::default()
    }
}

fn update(clock: &mut BeatClock, time: f64, strength: f64) -> Estimate {
    // The independent advisor hears half-time. It must not block a precise
    // primary 200 BPM interpretation or provide its phase.
    clock.update(observation(time, strength), time + 0.04, Some(grid(0.6)))
}

#[test]
fn quiet_precise_fast_beats_acquire_despite_half_time_advisor() {
    let mut clock = BeatClock::new(4);
    let mut acquired = None;
    for frame in 0..1600 {
        let time = f64::from(frame) * 0.01;
        let result = update(&mut clock, time, strength(time, false));
        if let Some(beat) = result.grids[1] {
            acquired.get_or_insert(time);
            assert!((result.bpm.unwrap() - 200.).abs() < 0.2);
            assert!((60. / beat.period - 200.).abs() < 0.2);
            let error =
                (beat.position(time) - grid(PERIOD).position(time) + 0.5).rem_euclid(1.) - 0.5;
            assert!(error.abs() < 0.02, "wrong beat phase at {time}: {error}");
        }
        if time >= 12. {
            assert!(
                result.grids[1].is_some(),
                "quiet precise beats never acquired"
            );
        }
    }
    assert!(acquired.unwrap() <= 12.);
}

#[test]
fn transient_or_imprecise_weak_onsets_do_not_establish_a_clock() {
    for jitter in [false, true] {
        let mut clock = BeatClock::new(4);
        for frame in 0..1600 {
            let time = f64::from(frame) * 0.01;
            let amplitude = if !jitter && time >= 2. {
                0.
            } else {
                strength(time, jitter)
            };
            let result = update(&mut clock, time, amplitude);
            assert!(
                result.grids[1].is_none(),
                "unsupported onsets established a clock at {time}, jitter={jitter}"
            );
        }
    }
}

#[test]
fn reset_discards_provisional_acquisition_history() {
    let mut reset_clock = BeatClock::new(4);
    // Six precise beats have arrived, but the extra persistence requirement
    // should still keep this below the point of publishing a clock.
    for frame in 0..210 {
        let time = f64::from(frame) * 0.01;
        let result = update(&mut reset_clock, time, strength(time, false));
        assert!(result.grids[1].is_none());
    }
    reset_clock.reset();
    let mut fresh_clock = BeatClock::new(4);
    let mut acquired = false;
    for frame in 210..1400 {
        let time = f64::from(frame) * 0.01;
        let amplitude = strength(time, false);
        let reset = update(&mut reset_clock, time, amplitude);
        let fresh = update(&mut fresh_clock, time, amplitude);
        assert_eq!(reset.bpm, fresh.bpm, "reset retained provisional state");
        assert_eq!(reset.grids, fresh.grids, "reset retained phase/history");
        acquired |= reset.grids[1].is_some();
    }
    assert!(
        acquired,
        "a reset clock must still be able to acquire again"
    );
}

#[test]
fn a_new_double_tempo_lock_recovers_promptly_with_independent_agreement() {
    let mut clock = BeatClock::new(4);
    let mut first_fast = None;
    let mut first_correct = None;
    for frame in 0..3600 {
        let time = f64::from(frame) * 0.01;
        let period = if time < 2.5 { PERIOD } else { PERIOD * 2. };
        let center = ANCHOR + ((time - ANCHOR) / period).round() * period;
        let mut raw = observation(time, pulse(time, center));
        raw.bpm = Some(60. / period);
        raw.grids[1] = Some(grid(period));
        let result = clock.update(raw, time + 0.04, Some(grid(PERIOD * 2.)));
        if result.bpm.is_some_and(|bpm| (bpm - 200.).abs() < 0.2) {
            first_fast.get_or_insert(time);
        }
        if result.bpm.is_some_and(|bpm| (bpm - 100.).abs() < 0.2) {
            first_correct.get_or_insert(time);
        }
        if time >= 30. {
            let beat = result.grids[1].expect("the corrected clock should remain available");
            assert!((result.bpm.unwrap() - 100.).abs() < 0.2);
            assert!((60. / beat.period - 100.).abs() < 0.2);
            let error =
                (beat.position(time) - grid(PERIOD * 2.).position(time) + 0.5).rem_euclid(1.) - 0.5;
            assert!(error.abs() < 0.025, "corrected phase at {time}: {error}");
        }
    }
    assert!(
        first_fast.is_some(),
        "the fixture must first acquire double tempo"
    );
    let acquired = first_correct.expect("sustained slower beats should correct double tempo");
    // This allows over seven seconds of actual slower evidence. Requiring the
    // mature-lock six-second dwell instead delays this fixture past 12 seconds.
    assert!(acquired <= 10., "startup correction took until {acquired}");
}
