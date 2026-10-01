use clap::Parser;
use std::time::{Duration, Instant};
use tempotrack::{
    cli::Args,
    config::Config,
    output::{config::OutputConfig, schedule::ClockCursor},
    rhythm::{Provenance, PulseGrid, RhythmSnapshot, Transport},
};

fn timeline(now: Instant, bpm: f64) -> RhythmSnapshot {
    let mut snapshot = RhythmSnapshot::empty(now, Default::default());
    snapshot.transport = Transport::Tracking;
    snapshot.valid_until = now + Duration::from_secs(7200);
    snapshot.grids[1] = Some(PulseGrid {
        anchor: 0.,
        period: 60. / bpm,
        provenance: Provenance::Derived,
    });
    snapshot
}

#[test]
fn fractional_clock_keeps_absolute_deadlines_for_an_hour() {
    let now = Instant::now();
    let snapshot = timeline(now, 126.3);
    let mut clock = ClockCursor::new(24);
    assert!(clock.poll(&snapshot, now).is_none());
    let interval = 60. / 126.3 / 24.;
    for index in 1..=181_872 {
        let expected = now + Duration::from_secs_f64(f64::from(index) * interval);
        let poll = expected + Duration::from_micros(50);
        let tick = clock.poll(&snapshot, poll).unwrap();
        assert_eq!(tick.index, i64::from(index));
        assert_eq!(tick.missed, 0);
        assert!(
            tick.deadline
                .max(expected)
                .duration_since(tick.deadline.min(expected))
                <= Duration::from_nanos(2)
        );
        let next = clock.next_deadline(&snapshot, poll).unwrap();
        let expected_next = now + Duration::from_secs_f64(f64::from(index + 1) * interval);
        assert!(
            next.max(expected_next)
                .duration_since(next.min(expected_next))
                <= Duration::from_nanos(2)
        );
    }
}

#[test]
fn stalls_skip_history_and_never_send_a_catchup_burst() {
    let now = Instant::now();
    let snapshot = timeline(now, 120.);
    let mut clock = ClockCursor::new(24);
    clock.poll(&snapshot, now);
    // Ten boundaries passed; number ten is already too old to send.
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(220))
            .is_none()
    );
    let at = now + Duration::from_secs_f64(11. / 48. + 0.00001);
    let tick = clock.poll(&snapshot, at).unwrap();
    assert_eq!((tick.index, tick.missed), (11, 10));
    for _ in 0..50 {
        assert!(clock.poll(&snapshot, at).is_none());
    }
}

#[test]
fn silence_staleness_and_new_generations_reprime_but_holdover_continues() {
    let now = Instant::now();
    let mut snapshot = timeline(now, 120.);
    let mut clock = ClockCursor::new(24);
    clock.poll(&snapshot, now);
    snapshot.transport = Transport::Holdover;
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(21))
            .is_some()
    );
    snapshot.transport = Transport::Silence;
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(42))
            .is_none()
    );
    snapshot.transport = Transport::Tracking;
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(63))
            .is_none()
    );
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(84))
            .is_some()
    );
    snapshot.generation += 1;
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(105))
            .is_none()
    );
    snapshot.valid_until = now + Duration::from_millis(110);
    assert!(
        clock
            .poll(&snapshot, now + Duration::from_millis(126))
            .is_none()
    );
    assert!(
        clock
            .next_deadline(&snapshot, now + Duration::from_millis(126))
            .is_none()
    );
}

#[test]
fn sequence_gaps_and_continuous_tempo_changes_preserve_clock_count() {
    let now = Instant::now();
    let mut snapshot = timeline(now, 120.);
    let mut clock = ClockCursor::new(24);
    clock.poll(&snapshot, now);
    let change = 0.1;
    let position = snapshot.grids[1].unwrap().position(change);
    let period = 60. / 121.7;
    snapshot.grids[1] = Some(PulseGrid {
        anchor: change - position * period,
        period,
        provenance: Provenance::Derived,
    });
    snapshot.sequence = 12345;
    let at = now + Duration::from_secs_f64(change);
    clock.poll(&snapshot, at);
    let next = clock.next_deadline(&snapshot, at).unwrap();
    let tick = clock
        .poll(&snapshot, next + Duration::from_micros(1))
        .unwrap();
    assert_eq!(tick.index, 5);
    assert!(
        clock
            .poll(&snapshot, next + Duration::from_micros(2))
            .is_none()
    );
}

#[test]
fn output_delay_and_advance_shift_deadlines_by_exactly_the_offset() {
    let now = Instant::now();
    for offset in [-0.05, 0., 0.05] {
        let mut snapshot = timeline(now, 120.);
        snapshot.offset_seconds = offset;
        let mut cursor = ClockCursor::new(1);
        cursor.poll(&snapshot, now + Duration::from_millis(100));
        let expected = now + Duration::from_secs_f64(0.5 + offset);
        assert_eq!(
            cursor.next_deadline(&snapshot, now + Duration::from_millis(100)),
            Some(expected)
        );
    }
}

#[test]
fn saved_settings_migrate_and_all_outputs_roundtrip_together() {
    let old: Config = serde_json::from_str("{}").unwrap();
    assert_eq!(old.outputs, OutputConfig::default());
    let args = Args::try_parse_from([
        "t",
        "--output",
        "link",
        "--output",
        "osc",
        "--output",
        "midi",
        "--output",
        "rtpmidi",
        "--osc-target",
        "localhost:9000",
        "--osc-target",
        "[::1]:9001",
        "--midi-port",
        "Receiver A",
        "--midi-port",
        "Receiver B",
        "--midi-virtual",
        "--rtpmidi-name",
        "Stage Clock",
        "--rtpmidi-port",
        "6004",
    ])
    .unwrap();
    let outputs = args
        .output_settings
        .apply(&args.output_names(false).unwrap())
        .unwrap();
    assert!(outputs.link && outputs.osc.enabled && outputs.midi.enabled && outputs.rtpmidi.enabled);
    assert_eq!(outputs.osc.targets.len(), 2);
    assert_eq!(outputs.midi.ports.len(), 2);
    let json = serde_json::to_string(&outputs).unwrap();
    assert_eq!(
        serde_json::from_str::<OutputConfig>(&json).unwrap(),
        outputs
    );
}

#[test]
fn invalid_output_options_fail_before_opening_audio_or_network() {
    for arguments in [
        vec!["t", "--output", "osc"],
        vec!["t", "--osc-target", "localhost:9000"],
        vec!["t", "--output", "osc", "--osc-target", "localhost:0"],
        vec!["t", "--output", "osc", "--osc-target", "bad\0host:9000"],
        vec![
            "t",
            "--output",
            "osc",
            "--osc-target",
            "localhost:9000",
            "--osc-prefix",
            "/bad\0prefix",
        ],
        vec![
            "t",
            "--output",
            "osc",
            "--osc-target",
            "localhost:9000",
            "--osc-prefix",
            "/bad/*",
        ],
        vec![
            "t",
            "--output",
            "osc",
            "--osc-target",
            "localhost:9000",
            "--osc-target",
            "localhost:9000",
        ],
        vec!["t", "--output", "midi"],
        vec!["t", "--output", "midi", "--midi-port-id", "bad\0id"],
        vec![
            "t",
            "--output",
            "midi",
            "--midi-port",
            "Same",
            "--midi-port",
            "Same",
        ],
        vec!["t", "--output", "rtpmidi", "--rtpmidi-name", ""],
        vec!["t", "--output", "link", "--output", "link"],
        vec!["t", "--output", "none", "--output", "link"],
    ] {
        let args = Args::try_parse_from(arguments.clone()).unwrap();
        assert!(
            args.output_names(false)
                .and_then(|names| args.output_settings.apply(&names))
                .is_err(),
            "{arguments:?}"
        );
    }
    assert!(Args::try_parse_from(["t", "--output", "rtpmidi", "--rtpmidi-port", "65535"]).is_err());
}
