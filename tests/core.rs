use clap::Parser;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tempotrack::{
    audio::{BLOCK, CaptureWriter, unique_name},
    backend::{self, Estimate},
    cli::Args,
    config::{Config, LiveControls, Tracking},
    engine::{Command, Engine, SilenceGate},
    output::{OutputDriver, OutputRunner, RecordingOutput},
    rhythm::{Provenance, PulseCursor, PulseGrid, RhythmSnapshot, Transport},
};

fn snapshot(now: Instant) -> RhythmSnapshot {
    let mut snapshot = RhythmSnapshot::empty(now, Tracking::Pulseweave);
    snapshot.transport = Transport::Tracking;
    snapshot.valid_until = now + Duration::from_secs(60);
    snapshot.bpm = Some(120.);
    snapshot.grids = [2., 0.5, 0.125].map(|period| {
        Some(PulseGrid {
            anchor: 0.,
            period,
            provenance: Provenance::Detected,
        })
    });
    snapshot
}

#[test]
fn split_channels_and_blocks_preserve_pcm() {
    let now = Instant::now();
    let data: Vec<i16> = (0..BLOCK * 4)
        .map(|i| if i % 2 == 0 { 16384 } else { -8192 })
        .collect();
    for chunk in [1, 3, 257, data.len()] {
        let (mut writer, mut reader, stats) = CaptureWriter::new(48000, 2, None, 4).unwrap();
        for input in data.chunks(chunk) {
            writer.push(input, now);
        }
        for start in [0, BLOCK as u64] {
            let block = reader.pop().unwrap();
            assert_eq!(block.source_frame, start);
            assert!(
                block
                    .samples
                    .iter()
                    .all(|sample| (*sample - 0.125).abs() < 1e-6)
            );
        }
        assert_eq!(stats.dropped_frames.load(Ordering::Relaxed), 0);
    }
}
#[test]
fn selection_conversion_and_nonfinite_samples() {
    let (mut writer, mut reader, _) = CaptureWriter::new(44100, 2, Some(2), 4).unwrap();
    let input: Vec<f32> = (0..BLOCK * 2)
        .map(|i| if i % 2 == 0 { f32::NAN } else { 0.75 })
        .collect();
    writer.push(&input, Instant::now());
    assert_eq!(reader.pop().unwrap().samples, [0.75; BLOCK]);
    writer.push(&[f32::INFINITY; BLOCK * 2], Instant::now());
    assert_eq!(reader.pop().unwrap().samples, [0.; BLOCK]);
    assert!(CaptureWriter::new(44100, 2, Some(3), 4).is_err());
}
#[test]
fn overflow_does_not_hide_source_gaps() {
    let (mut writer, mut reader, stats) = CaptureWriter::new(48000, 1, None, 1).unwrap();
    writer.push(&[0.1; BLOCK * 3], Instant::now());
    assert_eq!(reader.pop().unwrap().source_frame, 0);
    writer.push(&[0.1; BLOCK], Instant::now());
    assert_eq!(reader.pop().unwrap().source_frame, (BLOCK * 3) as u64);
    assert_eq!(
        stats.dropped_frames.load(Ordering::Relaxed),
        (BLOCK * 2) as u64
    );
}
#[test]
fn recoverable_driver_notifications_do_not_end_capture() {
    let stats = tempotrack::audio::CaptureStats::default();
    stats.record_error(cpal::ErrorKind::Xrun);
    stats.record_error(cpal::ErrorKind::DeviceChanged);
    stats.record_error(cpal::ErrorKind::RealtimeDenied);
    assert!(stats.failure().is_none());
    assert_eq!(stats.discontinuities.load(Ordering::Relaxed), 2);
    assert_eq!(stats.scheduling_warnings.load(Ordering::Relaxed), 1);
    stats.record_error(cpal::ErrorKind::PermissionDenied);
    assert_eq!(stats.failure(), Some(cpal::ErrorKind::PermissionDenied));
}
#[test]
fn capture_timestamps_advance_at_source_rate() {
    let now = Instant::now();
    let (mut writer, mut reader, _) = CaptureWriter::new(48000, 1, None, 4).unwrap();
    writer.push(&[0.; BLOCK * 2], now);
    assert_eq!(reader.pop().unwrap().captured_at, now);
    assert_eq!(
        reader.pop().unwrap().captured_at,
        now + Duration::from_secs_f64(BLOCK as f64 / 48000.)
    );
}
#[test]
fn silence_holds_then_recovers() {
    let mut gate = SilenceGate::default();
    let controls = LiveControls::default();
    assert!(gate.update(-20., 480, 48000, controls));
    for _ in 0..49 {
        assert!(!gate.update(-80., 480, 48000, controls));
    }
    assert!(!gate.silent);
    assert!(gate.update(-80., 480, 48000, controls));
    assert!(gate.silent);
    assert!(gate.update(-30., 480, 48000, controls));
}
#[test]
fn projection_and_offsets_use_monotonic_time() {
    let now = Instant::now();
    let mut state = snapshot(now);
    let at = now + Duration::from_millis(125);
    assert_eq!(state.phase(1, at), Some(0.25));
    assert_eq!(state.beat_position(now + Duration::from_secs(2)), Some(4.));
    state.offset_seconds = 0.125;
    assert_eq!(state.phase(1, at), Some(0.));
    state.offset_seconds = -0.125;
    assert_eq!(state.phase(1, at), Some(0.5));
    state.valid_until = now;
    assert_eq!(state.phase(1, at), None);
}
#[test]
fn pulse_cursor_skips_history_and_rejects_duplicate_corrections() {
    let now = Instant::now();
    let mut state = snapshot(now);
    let mut cursor = PulseCursor::default();
    assert_eq!(cursor.poll(&state, now), [false; 3]);
    assert_eq!(
        cursor.poll(&state, now + Duration::from_millis(505)),
        [false, true, true]
    );
    state.grids[1].as_mut().unwrap().anchor += 0.01;
    assert!(!cursor.poll(&state, now + Duration::from_millis(515))[1]);
    assert_eq!(
        cursor.poll(&state, now + Duration::from_millis(5_350)),
        [false; 3]
    );
    state.generation += 1;
    assert_eq!(
        cursor.poll(&state, now + Duration::from_millis(5_505)),
        [false; 3]
    );
    state.transport = Transport::Silence;
    assert_eq!(
        cursor.poll(&state, now + Duration::from_secs(6)),
        [false; 3]
    );
}
#[test]
fn tempo_corrections_preserve_continuous_beat_count() {
    let old = Estimate {
        grids: [
            None,
            Some(PulseGrid {
                anchor: 0.,
                period: 0.5,
                provenance: Provenance::Detected,
            }),
            None,
        ],
        ..Default::default()
    };
    let mut new = Estimate {
        grids: [
            None,
            Some(PulseGrid {
                anchor: 10.,
                period: 0.4,
                provenance: Provenance::Detected,
            }),
            None,
        ],
        ..Default::default()
    };
    new.align_to(&old);
    let grid = new.grids[1].unwrap();
    assert_eq!(grid.position(10.), 20.);
    assert!((grid.next_boundary(10.) - 10.4).abs() < 1e-9);
}
#[test]
fn output_works_without_ui_updates() {
    let recorder = RecordingOutput::default();
    let mut runner = OutputRunner::spawn(Box::new(recorder.clone())).unwrap();
    let mut state = snapshot(Instant::now());
    state.sequence = 1;
    runner.publish(state);
    let deadline = Instant::now() + Duration::from_secs(2);
    while recorder.snapshots().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let recorded = recorder.snapshots();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].bpm, Some(120.));
    assert!(!runner.stats.failed.load(Ordering::Acquire));
}
#[test]
fn slow_output_cannot_block_the_producer() {
    struct Slow;
    impl OutputDriver for Slow {
        fn name(&self) -> &str {
            "slow-test"
        }
        fn reset(&mut self) {}
        fn on_timeline(
            &mut self,
            _: RhythmSnapshot,
        ) -> Result<(), tempotrack::output::OutputError> {
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, tempotrack::output::OutputError> {
            Ok(None)
        }
    }
    let mut runner = OutputRunner::spawn(Box::new(Slow)).unwrap();
    let mut state = snapshot(Instant::now());
    for sequence in 1..10_000 {
        state.sequence = sequence;
        runner.publish(state);
    }
    assert!(runner.stats.dropped.load(Ordering::Relaxed) > 0);
}
#[test]
fn cli_mode_selection_and_config_parity() {
    let args = Args::try_parse_from([
        "tempotrack",
        "--input",
        "Audio Interface",
        "--output",
        "stdout",
        "--gain-db",
        "-6",
        "--channel",
        "2",
        "--stop-on-silence",
        "false",
    ])
    .unwrap();
    assert!(!args.wants_gui());
    let config = args.settings.apply(Config::default()).unwrap();
    assert_eq!(config.input.as_deref(), Some("Audio Interface"));
    assert_eq!(config.live.gain_db, -6.);
    assert!(!config.live.stop_on_silence);
    assert_eq!(config.channel, Some(2));
    assert_eq!(
        Args::try_parse_from(["tempotrack"]).unwrap().wants_gui(),
        cfg!(feature = "gui")
    );
    assert!(
        !Args::try_parse_from(["tempotrack", "--headless"])
            .unwrap()
            .wants_gui()
    );
}
#[test]
fn cli_rejects_conflicts_and_invalid_settings() {
    assert!(Args::try_parse_from(["t", "--gui", "--headless"]).is_err());
    assert!(Args::try_parse_from(["t", "--input", "x", "--input-index", "0"]).is_err());
    assert!(Args::try_parse_from(["t", "--tracking", "fake"]).is_err());
    assert!(Args::try_parse_from(["t", "--channel", "0"]).is_err());
    let args = Args::try_parse_from(["t", "--output", "none", "--output", "stdout"]).unwrap();
    assert!(args.output_names(false).is_err());
    let args = Args::try_parse_from(["t", "--gain-db", "NaN"]).unwrap();
    assert!(args.settings.apply(Config::default()).is_err());
}
#[test]
fn device_selection_requires_unique_exact_name() {
    let names = ["Mic".into(), "Audio Interface".into(), "Mic".into()];
    assert_eq!(unique_name(&names, "Audio Interface").unwrap(), 1);
    assert!(unique_name(&names, "Mic").is_err());
    assert!(unique_name(&names, "Audio").is_err());
}
#[test]
fn settings_roundtrip_retains_live_controls() {
    let config = Config {
        input: Some("USB audio".into()),
        ..Default::default()
    };
    let restored: Config = serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored, config);
    let defaults: Config = serde_json::from_str("{}").unwrap();
    assert_eq!(defaults, Config::default());
    // Legacy detector preferences are ignored; rewritten settings drop the old knob.
    for tracking in ["pulseweave", "beatnet", "assisted"] {
        let restored: Config =
            serde_json::from_value(serde_json::json!({"tracking": tracking, "input": "USB audio"}))
                .unwrap();
        assert_eq!(restored.input.as_deref(), Some("USB audio"));
        assert!(
            serde_json::to_value(restored)
                .unwrap()
                .get("tracking")
                .is_none()
        );
    }
}
#[test]
fn engine_can_stop_and_join_without_an_audio_device() {
    let mut engine = Engine::spawn(vec![]).unwrap();
    engine.send(Command::Stop).unwrap();
    engine.shutdown().unwrap();
    assert!(engine.is_finished());
    assert!(engine.send(Command::Reset).is_err());
}

fn percussion(rate: u32, seconds: usize) -> Vec<f32> {
    (0..rate as usize * seconds)
        .map(|i| {
            let t = i as f64 / rate as f64;
            let beat = t % 0.5;
            let hat = t % 0.25;
            let kick = (std::f64::consts::TAU * 70. * t).sin() * (-beat * 45.).exp();
            let snare = (std::f64::consts::TAU * 1739. * t).sin() * (-hat * 100.).exp();
            (0.7 * kick + 0.2 * snare) as f32
        })
        .collect()
}
#[test]
fn real_backends_accept_both_rates_and_reset_cleanly() {
    for rate in [44100, 48000] {
        let audio = percussion(rate, 6);
        for tracking in [Tracking::Pulseweave, Tracking::Beatnet, Tracking::Assisted] {
            let config = Config::default();
            let mut tracker = if tracking == Tracking::Assisted {
                backend::create(&config, rate).unwrap()
            } else {
                backend::create_for_evaluation(&config, rate, tracking).unwrap()
            };
            let mut estimates = 0;
            for block in audio.chunks(256) {
                if let Some(estimate) = tracker.process(block).unwrap() {
                    estimates += 1;
                    assert!(estimate.bpm.is_some_and(|bpm| bpm.is_finite() && bpm > 0.));
                    assert!(estimate.grids.iter().flatten().all(|g| g.valid()));
                }
            }
            // This rhythmic fixture establishes a timeline; exact BPM and meter
            // accuracy are intentionally not asserted across different algorithms.
            assert!(estimates > 0, "no {tracking} estimates at {rate} Hz");
            tracker.reset(rate as u64 * 20);
            for block in audio.chunks(997) {
                if let Some(estimate) = tracker.process(block).unwrap() {
                    assert!(estimate.grids.iter().flatten().all(|g| g.anchor > 15.));
                    break;
                }
            }
        }
    }
}

#[test]
fn stable_backends_are_independent_of_input_chunking_including_low_sample_rates() {
    for rate in [8000, 48000] {
        let audio = percussion(rate, 8);
        for tracking in [Tracking::Pulseweave, Tracking::Beatnet, Tracking::Assisted] {
            let config = Config::default();
            let run = |chunk| {
                let mut backend = if tracking == Tracking::Assisted {
                    backend::create(&config, rate).unwrap()
                } else {
                    backend::create_for_evaluation(&config, rate, tracking).unwrap()
                };
                let mut results = Vec::new();
                for part in audio.chunks(chunk) {
                    backend
                        .process_each(part, &mut |e| results.push((e.bpm, e.grids, e.holding)))
                        .unwrap();
                }
                results
            };
            let expected = run(256);
            assert!(!expected.is_empty(), "no {tracking} lock at {rate}");
            assert_eq!(expected, run(997), "{tracking} at {rate}");
        }
    }
}

#[test]
fn legacy_low_level_settings_cannot_override_fixed_application_choices() {
    let restored: Config = serde_json::from_str(
        r#"{"model":3,"atom_subdivision":2,"sample_rate":96000,"buffer_size":64}"#,
    )
    .unwrap();
    assert_eq!(restored, Config::default());
    let saved = serde_json::to_value(restored).unwrap();
    for field in ["model", "atom_subdivision", "sample_rate", "buffer_size"] {
        assert!(saved.get(field).is_none());
    }
    assert_eq!(tempotrack::config::ATOM_SUBDIVISION, 4);
    for flag in [
        "--model",
        "--atom-subdivision",
        "--sample-rate",
        "--buffer-size",
    ] {
        assert!(Args::try_parse_from(["t", flag, "2"]).is_err());
    }
}
