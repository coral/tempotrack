use tempotrack::{
    backend::{Estimate, Evidence},
    clock::BeatClock,
    rhythm::{Provenance, PulseGrid, Quality},
};

fn phase_difference(a: f64, b: f64) -> f64 {
    (a - b + 0.5).rem_euclid(1.) - 0.5
}

#[test]
fn a_temporary_offbeat_only_passage_does_not_replace_an_established_phase() {
    let mut clock = BeatClock::new(4);
    let mut previous = None;
    for frame in 0..3000 {
        let time = f64::from(frame) * 0.02;
        let position = time * 2.;
        // Seven bars of offbeats outlast the original observations in the fit,
        // but the established musical beat returns unchanged afterward.
        let offbeat_only = (24. ..38.).contains(&time);
        let raw_position = position - if offbeat_only { 0.5 } else { 0. };
        let now = time + 0.04;
        let result = clock.update(
            observation(time, raw_position, 120., peak(raw_position)),
            now,
            None,
        );
        if time >= 12. {
            let beat = result.grids[1].expect("short offbeat passages should retain the clock");
            assert_continuous(previous, beat, now);
            assert!(
                phase_difference(beat.position(time), position).abs() < 0.04,
                "short offbeat passage changed phase at {time}"
            );
            assert!(
                (60. / beat.period - 120.).abs() < 0.3,
                "short offbeat passage caused a frequency correction at {time}"
            );
            previous = Some(beat);
        }
    }
}

fn peak(position: f64) -> f64 {
    (-0.5 * (phase_difference(position, 0.) / 0.045).powi(2)).exp()
}

fn observation(time: f64, position: f64, bpm: f64, strength: f64) -> Estimate {
    let period = 60. / bpm;
    Estimate {
        grids: [
            None,
            Some(PulseGrid {
                anchor: time - position.rem_euclid(1.) * period,
                period,
                provenance: Provenance::Detected,
            }),
            None,
        ],
        bpm: Some(bpm),
        quality: Quality::BeatNetConfidence(0.8),
        evidence: Some(Evidence {
            time,
            beat: strength as f32,
            downbeat: 0.,
            event: strength > 0.5,
        }),
        ..Estimate::default()
    }
}

fn assert_continuous(previous: Option<PulseGrid>, current: PulseGrid, now: f64) {
    if let Some(previous) = previous {
        assert!(
            (current.position(now) - previous.position(now)).abs() < 1e-8,
            "clock position jumped at {now}"
        );
    }
}

#[test]
fn stronger_syncopation_preserves_a_supported_beat_phase() {
    for displacement in [0.25, 0.5] {
        let mut clock = BeatClock::new(4);
        let mut previous = None;
        for frame in 0..4000 {
            let time = f64::from(frame) * 0.02;
            let position = time * 2.;
            let syncopated = time >= 24.;
            let original_strength = if !syncopated {
                0.8
            } else if position.round() as i64 % 2 == 0 {
                0.65
            } else {
                0.
            };
            let strength = (original_strength * peak(position)).max(if syncopated {
                peak(position - displacement)
            } else {
                0.
            });
            let raw_position = position - if syncopated { displacement } else { 0. };
            let now = time + 0.04;
            let result = clock.update(observation(time, raw_position, 120., strength), now, None);
            if time >= 12. {
                let grid = result.grids[1].expect("initial beats should establish a clock");
                assert_continuous(previous, grid, now);
                assert!(
                    phase_difference(grid.position(time), position).abs() < 0.04,
                    "stronger {displacement}-beat syncopation displaced phase at {time}"
                );
                assert!(
                    (60. / grid.period - 120.).abs() < 0.3,
                    "syncopation changed clock tempo at {time}: {}",
                    60. / grid.period
                );
                previous = Some(grid);
            }
        }
    }
}

#[test]
fn a_complete_phase_change_eventually_replaces_the_old_phase() {
    let mut clock = BeatClock::new(4);
    let mut previous = None;
    for frame in 0..4000 {
        let time = f64::from(frame) * 0.02;
        // The original beats disappear entirely; a continuing old phase would
        // now be incorrect, unlike the competing syncopation in the other test.
        let position = time * 2. - if time >= 24. { 0.4 } else { 0. };
        let now = time + 0.04;
        let result = clock.update(
            observation(time, position, 120., 0.9 * peak(position)),
            now,
            None,
        );
        if time >= 12. {
            let grid = result.grids[1].expect("clock should remain available during recovery");
            assert_continuous(previous, grid, now);
            previous = Some(grid);
            if time >= 55. {
                assert!(
                    phase_difference(grid.position(time), position).abs() < 0.025,
                    "clock did not recover the replacement phase at {time}"
                );
                assert!(
                    (60. / grid.period - 120.).abs() < 0.3,
                    "clock was still slewing at {time}: {}",
                    60. / grid.period
                );
            }
        }
    }
}

#[test]
fn gradual_tempo_drift_tracks_without_phase_resets() {
    let mut clock = BeatClock::new(4);
    let mut previous = None;
    for frame in 0..4000 {
        let time = f64::from(frame) * 0.02;
        let drifting = (time - 20.).max(0.);
        let bpm = 120. + drifting * 0.05;
        // Integrate the tempo ramp so the synthetic source itself is continuous.
        let position = time * 2. + 0.5 * 0.05 / 60. * drifting.powi(2);
        let now = time + 0.04;
        let result = clock.update(
            observation(time, position, bpm, 0.9 * peak(position)),
            now,
            None,
        );
        if time >= 12. {
            let grid = result.grids[1].expect("a gradual tempo ramp should retain the clock");
            assert_continuous(previous, grid, now);
            assert!(
                phase_difference(grid.position(time), position).abs() < 0.05,
                "phase stopped following gradual drift at {time}"
            );
            assert!(
                (60. / grid.period - bpm).abs() < 0.4,
                "clock tempo failed to follow gradual drift at {time}: {} versus {bpm}",
                60. / grid.period
            );
            previous = Some(grid);
        }
    }
}
