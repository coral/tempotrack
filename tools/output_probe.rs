//! Synthetic clock diagnostic: tests output scheduling without audio capture or inference.
use clap::Parser;
use serde::Serialize;
use std::{
    hint::black_box,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tempotrack::{
    cli::OutputArgs,
    output::{OutputDriver, OutputError, OutputService, schedule::ClockCursor},
    rhythm::{Provenance, PulseGrid, RhythmSnapshot, Transport},
};

#[derive(Parser)]
#[command(
    about = "Measure output-worker jitter with a synthetic clock; optionally drive real outputs"
)]
struct Args {
    #[arg(long, default_value = "126.3")]
    bpm: f64,
    #[arg(long, default_value = "10", value_parser = clap::value_parser!(u32).range(1..=3600))]
    seconds: u32,
    /// CPU load threads in addition to the output workers.
    #[arg(long, default_value = "0", value_parser = clap::value_parser!(u16).range(0..=256))]
    load_workers: u16,
    /// Optional real destinations. Sends clock on the network/ports you select.
    #[arg(long = "output", value_parser = ["link", "osc", "midi", "rtpmidi"])]
    outputs: Vec<String>,
    #[command(flatten)]
    destinations: OutputArgs,
}

#[derive(Clone, Copy)]
struct Sample {
    index: i64,
    late_ms: f64,
    at: Instant,
    missed: u64,
}
struct Probe {
    cursor: ClockCursor,
    snapshot: Option<RhythmSnapshot>,
    samples: Arc<Mutex<Vec<Sample>>>,
}
impl OutputDriver for Probe {
    fn name(&self) -> &str {
        "timing-probe"
    }
    fn reset(&mut self) {
        self.cursor.reset();
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        let Some(snapshot) = self.snapshot else {
            return Ok(None);
        };
        if let Some(tick) = self.cursor.poll(&snapshot, now) {
            self.samples.lock().unwrap().push(Sample {
                index: tick.index,
                late_ms: now.saturating_duration_since(tick.deadline).as_secs_f64() * 1000.,
                at: now,
                missed: tick.missed,
            });
        }
        Ok(self.cursor.next_deadline(&snapshot, now))
    }
}
#[derive(Serialize)]
struct Distribution {
    median: f64,
    p95: f64,
    p99: f64,
    max: f64,
}
fn distribution(mut values: Vec<f64>) -> Distribution {
    values.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        values
            .get(((values.len().saturating_sub(1)) as f64 * p).round() as usize)
            .copied()
            .unwrap_or(0.)
    };
    Distribution {
        median: percentile(0.5),
        p95: percentile(0.95),
        p99: percentile(0.99),
        max: percentile(1.),
    }
}
#[derive(Serialize)]
struct Report {
    measurement: &'static str,
    bpm: f64,
    seconds: u32,
    load_workers: u16,
    ticks: usize,
    missed_ticks: u64,
    duplicate_or_backward_ticks: usize,
    deadline_lateness_ms: Distribution,
    interval_jitter_ms: Distribution,
    beat_phase_error: Distribution,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !(30. ..=400.).contains(&args.bpm) {
        return Err("--bpm must be between 30 and 400".into());
    }
    let outputs = args.destinations.apply(&args.outputs)?;
    let samples = Arc::new(Mutex::new(Vec::with_capacity(
        (f64::from(args.seconds) * args.bpm * 24. / 60.) as usize + 16,
    )));
    let (mut service, mut publisher) = OutputService::spawn(vec![Box::new(Probe {
        cursor: ClockCursor::new(24),
        snapshot: None,
        samples: samples.clone(),
    })])?;
    service.configure(outputs)?;
    let stop = Arc::new(AtomicBool::new(false));
    let loads: Vec<_> = (0..args.load_workers)
        .map(|_| {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut value = 0.125_f64;
                while !stop.load(Ordering::Relaxed) {
                    for _ in 0..4096 {
                        value = black_box(value.sin().mul_add(0.999, 0.001));
                    }
                }
                black_box(value);
            })
        })
        .collect();
    let start = Instant::now();
    let end = start + Duration::from_secs(u64::from(args.seconds));
    let mut snapshot = RhythmSnapshot::empty(start, Default::default());
    snapshot.generation = 1;
    snapshot.transport = Transport::Tracking;
    snapshot.bpm = Some(args.bpm);
    snapshot.meter = Some(4);
    snapshot.grids = [4., 1., 0.25].map(|beats| {
        Some(PulseGrid {
            anchor: 0.,
            period: beats * 60. / args.bpm,
            provenance: Provenance::Derived,
        })
    });
    let mut next = start;
    while Instant::now() < end {
        let now = Instant::now();
        snapshot.sequence += 1;
        snapshot.reference_time = now;
        snapshot.source_time = now.duration_since(start).as_secs_f64();
        snapshot.valid_until = (now + Duration::from_millis(250)).min(end);
        publisher.publish(snapshot);
        next += Duration::from_millis(20);
        if next <= now {
            next = now + Duration::from_millis(20);
        }
        thread::sleep(next.min(end).saturating_duration_since(Instant::now()));
    }
    for status in service.status() {
        eprintln!("{}: {}", status.id, status.detail);
    }
    service.shutdown();
    stop.store(true, Ordering::Relaxed);
    for worker in loads {
        let _ = worker.join();
    }
    let samples = samples.lock().unwrap();
    if samples.is_empty() {
        return Err("No clock ticks recorded".into());
    }
    // Include deadlines missed at the end, even when there is no later emitted
    // tick to carry ClockCursor's missed count. Acquisition before the cursor's
    // first observation is deliberately outside the measured interval.
    let final_index = (f64::from(args.seconds) * args.bpm * 24. / 60.).ceil() as i64 - 1;
    let expected = (final_index - samples[0].index + 1).max(0) as u64;
    let report = Report {
        measurement: "24 PPQN output worker; software deadlines, not network or hardware receive timing",
        bpm: args.bpm,
        seconds: args.seconds,
        load_workers: args.load_workers,
        ticks: samples.len(),
        missed_ticks: samples[0].missed + expected.saturating_sub(samples.len() as u64),
        duplicate_or_backward_ticks: samples
            .windows(2)
            .filter(|pair| pair[1].index <= pair[0].index)
            .count(),
        deadline_lateness_ms: distribution(samples.iter().map(|sample| sample.late_ms).collect()),
        interval_jitter_ms: distribution(
            samples
                .windows(2)
                .map(|pair| {
                    let ideal = (pair[1].index - pair[0].index) as f64 * 60. / args.bpm / 24.;
                    (pair[1].at.duration_since(pair[0].at).as_secs_f64() - ideal).abs() * 1000.
                })
                .collect(),
        ),
        beat_phase_error: distribution(
            samples
                .iter()
                .map(|sample| sample.late_ms * args.bpm / 60_000.)
                .collect(),
        ),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
