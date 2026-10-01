use tempotrack::{
    backend::{Estimate, Evidence},
    clock::BeatClock,
    rhythm::{Provenance, PulseGrid, Quality},
};

fn observation(time: f64, position: f64, raw_bpm: f64, audible: bool) -> Estimate {
    let distance = (position + 0.5).rem_euclid(1.) - 0.5;
    let strength = if audible {
        (-0.5 * (distance / 0.04).powi(2)).exp() as f32
    } else {
        0.
    };
    let grid = PulseGrid {
        anchor: time - position.fract() * 60. / raw_bpm,
        period: 60. / raw_bpm,
        provenance: Provenance::Detected,
    };
    Estimate {
        bpm: Some(raw_bpm),
        grids: [
            Some(PulseGrid {
                period: grid.period * 4.,
                ..grid
            }),
            Some(grid),
            None,
        ],
        meter: Some(4),
        quality: Quality::BeatNetConfidence(0.8),
        evidence: Some(Evidence {
            time,
            beat: strength,
            downbeat: 0.,
            event: strength > 0.5,
        }),
        holding: false,
    }
}

#[test]
fn noisy_tempo_states_do_not_move_a_locked_clock_and_drop_holds() {
    let mut clock = BeatClock::new(4);
    let mut previous: Option<PulseGrid> = None;
    let mut last_bpm = 0.;
    for frame in 0..6000 {
        let time = frame as f64 * 0.02;
        let audible = !(40. ..55.).contains(&time);
        let raw = observation(
            time,
            time / 0.48,
            if frame % 3 == 0 { 120. } else { 130.4348 },
            audible,
        );
        let result = clock.update(raw, time + 0.04, None);
        if time > 10. {
            let beat = result.grids[1].expect("must have acquired");
            assert!(
                (result.bpm.unwrap() - 125.).abs() < 0.15,
                "{time}: {:?}",
                result.bpm
            );
            if let Some(old) = previous {
                assert!((beat.position(time + 0.04) - old.position(time + 0.04)).abs() < 1e-9);
            }
            let bar = result.grids[0].unwrap();
            let atom = result.grids[2].unwrap();
            assert!((bar.period / beat.period - 4.).abs() < 1e-9);
            assert!((beat.period / atom.period - 4.).abs() < 1e-9);
            if (45. ..54.).contains(&time) {
                assert!((result.bpm.unwrap() - last_bpm).abs() < 1e-9);
            }
            last_bpm = result.bpm.unwrap();
            previous = Some(beat);
        }
    }
}

#[test]
fn fractional_tempo_is_recovered_beyond_detector_frame_resolution() {
    let mut clock = BeatClock::new(4);
    let mut last = Estimate::default();
    for frame in 0..3000 {
        let time = frame as f64 * 0.02;
        last = clock.update(
            observation(
                time,
                time * 131.7 / 60.,
                if frame % 2 == 0 { 130.4348 } else { 136.3636 },
                true,
            ),
            time + 0.04,
            None,
        );
    }
    assert!((last.bpm.unwrap() - 131.7).abs() < 0.05);
    let grid = last.grids[1].unwrap();
    let phase_error = ((grid.position(60.) - 60. * 131.7 / 60. + 0.5).rem_euclid(1.) - 0.5).abs();
    assert!(
        phase_error * grid.period < 0.005,
        "phase error {phase_error}"
    );
}

#[test]
fn stronger_subdivisions_do_not_double_an_established_tempo() {
    let mut clock = BeatClock::new(4);
    for frame in 0..4000 {
        let time = frame as f64 * 0.02;
        let doubled = time > 20.;
        let raw = observation(
            time,
            time * if doubled { 4. } else { 2. },
            if doubled { 240. } else { 120. },
            true,
        );
        let result = clock.update(raw, time + 0.04, None);
        if time > 10. {
            assert!(
                (result.bpm.unwrap() - 120.).abs() < 0.1,
                "{time}: {:?}",
                result.bpm
            );
        }
    }
}

#[test]
fn a_sustained_tempo_change_can_replace_the_lock() {
    let mut clock = BeatClock::new(4);
    for frame in 0..5000 {
        let time = frame as f64 * 0.02;
        let (position, bpm) = if time < 48. {
            (time / 0.48, 125.)
        } else {
            (100. + (time - 48.) * 140. / 60., 140.)
        };
        let result = clock.update(observation(time, position, bpm, true), time + 0.04, None);
        if time > 70. {
            assert!(
                (result.bpm.unwrap() - 140.).abs() < 0.15,
                "{time}: {:?}",
                result.bpm
            );
            let g = result.grids[1].unwrap();
            let error = (g.position(time) - position + 0.5).rem_euclid(1.) - 0.5;
            assert!(error.abs() < 0.1, "phase at {time}: {error}");
        }
    }
}

#[test]
fn weak_evidence_does_not_acquire_a_clock() {
    let mut clock = BeatClock::new(4);
    for frame in 0..1000 {
        let time = frame as f64 * 0.02;
        assert!(
            clock
                .update(observation(time, time * 2., 120., false), time, None)
                .bpm
                .is_none()
        );
    }
}

#[test]
fn a_half_time_advisor_cannot_pull_a_supported_fast_clock() {
    let mut clock = BeatClock::new(4);
    for frame in 0..3000 {
        let time = frame as f64 * 0.02;
        let guide = PulseGrid {
            anchor: 0.,
            period: 0.6,
            provenance: Provenance::Detected,
        };
        let raw = observation(
            time,
            time / 0.3,
            if frame % 30 == 0 { 214.2857 } else { 200. },
            true,
        );
        let result = clock.update(raw, time + 0.04, Some(guide));
        if time > 10. {
            assert!((result.bpm.unwrap() - 200.).abs() < 0.2);
        }
    }
}

#[test]
fn advisor_can_offer_a_tempo_when_the_particle_hypothesis_is_wrong() {
    let mut clock = BeatClock::new(4);
    let mut last = Estimate::default();
    for frame in 0..2000 {
        let time = frame as f64 * 0.02;
        let guide = PulseGrid {
            anchor: 0.17,
            period: 0.48,
            provenance: Provenance::Detected,
        };
        last = clock.update(
            observation(time, time / 0.48, 155., true),
            time + 0.04,
            Some(guide),
        );
    }
    assert!((last.bpm.unwrap() - 125.).abs() < 0.15);
    let beat = last.grids[1].unwrap();
    // The advisor's arbitrary phase is never copied into the clock.
    assert!(((beat.position(40.) - 40. / 0.48 + 0.5).rem_euclid(1.) - 0.5).abs() < 0.02);
}

#[test]
fn a_new_clock_discards_the_old_phase_and_tempo_history() {
    let mut clock = BeatClock::new(4);
    for frame in 0..1000 {
        let time = frame as f64 * 0.02;
        clock.update(observation(time, time * 2., 120., true), time, None);
    }
    clock = BeatClock::new(4);
    assert!(
        clock
            .update(observation(200., 0., 150., true), 200., None)
            .bpm
            .is_none()
    );
}

#[test]
fn tempo_prior_checks_harmonics_without_clamping_strong_out_of_band_evidence() {
    for (actual, hypothesis) in [(150., 75.), (110., 220.), (90., 90.)] {
        let mut clock = BeatClock::new(4);
        let mut last = Estimate::default();
        for frame in 0..2500 {
            let time = frame as f64 * 0.02;
            last = clock.update(
                observation(time, time * actual / 60., hypothesis, true),
                time + 0.04,
                None,
            );
        }
        assert!(
            (last.bpm.unwrap() - actual).abs() < 0.2,
            "{actual}/{hypothesis}: {:?}",
            last.bpm
        );
    }
}

#[test]
fn independent_tempo_agreement_can_support_weak_but_periodic_neural_peaks() {
    let mut alone = BeatClock::new(4);
    let mut assisted = BeatClock::new(4);
    let guide = PulseGrid {
        anchor: 0.2,
        period: 0.48,
        provenance: Provenance::Detected,
    };
    let mut last = Estimate::default();
    for frame in 0..1500 {
        let time = frame as f64 * 0.02;
        let mut raw = observation(time, time / 0.48, 125., true);
        let offbeat = observation(time, time / 0.48 + 0.5, 125., true)
            .evidence
            .unwrap()
            .beat;
        let evidence = raw.evidence.as_mut().unwrap();
        evidence.beat = (evidence.beat * 0.5).max(offbeat * 0.45);
        assert!(alone.update(raw, time + 0.04, None).bpm.is_none());
        last = assisted.update(raw, time + 0.04, Some(guide));
    }
    assert!((last.bpm.unwrap() - 125.).abs() < 0.1);
}

#[test]
fn tempo_advisor_requires_consistency_and_expires_on_the_source_clock() {
    let mut advisor = tempotrack::backend::TempoAdvisor::new(4);
    for frame in 0..25 {
        let time = frame as f64 * 0.5;
        let grid = PulseGrid {
            anchor: time,
            period: 0.5,
            provenance: Provenance::Detected,
        };
        advisor.update(
            Estimate {
                bpm: Some(120.),
                grids: [None, Some(grid), None],
                ..Default::default()
            },
            time,
        );
        if time < 4. {
            assert!(advisor.grid(time).is_none());
        }
    }
    assert!(advisor.grid(12.).is_some());
    assert!(advisor.grid(15.).is_none());
    for second in 13..30 {
        advisor.update(Estimate::default(), f64::from(second));
    }
    assert!(advisor.grid(29.).is_none());
}

#[test]
fn explicit_detector_bounds_survive_reset_and_reject_an_out_of_range_advisor() {
    let config = tempotrack::config::Config {
        min_bpm: 280.,
        max_bpm: 320.,
        ..Default::default()
    };
    let mut clock = BeatClock::for_config(&config);
    let guide = PulseGrid {
        anchor: 0.,
        period: 0.4,
        provenance: Provenance::Detected,
    };
    for origin in [0., 100.] {
        clock.reset();
        let mut last = Estimate::default();
        for frame in 0..1000 {
            let time = origin + frame as f64 * 0.02;
            last = clock.update(
                observation(time, time * 5., 300., true),
                time + 0.04,
                Some(guide),
            );
        }
        assert!((last.bpm.unwrap() - 300.).abs() < 0.2);
    }
}
