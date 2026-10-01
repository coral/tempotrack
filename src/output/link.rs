//! Link owns a disposable runtime: the upstream driver's discovery tasks outlive disable().
use super::{OutputDriver, OutputError};
use crate::rhythm::RhythmSnapshot;
use ableton_link_rs::link::BasicLink;
use std::{
    panic::AssertUnwindSafe,
    time::{Duration, Instant},
};
use tokio::runtime::{Builder, Runtime};

const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

pub struct LinkOutput {
    link: BasicLink,
    runtime: Runtime,
    snapshot: Option<RhythmSnapshot>,
    aligned: bool,
    peers: usize,
    next_publish: Instant,
    stopped: bool,
}

impl LinkOutput {
    pub fn open() -> Result<Self, OutputError> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        // Upstream currently unwraps interface enumeration and socket creation. Keep
        // these failures inside this output instead of killing the output supervisor.
        let link = std::panic::catch_unwind(AssertUnwindSafe(|| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut link = BasicLink::new(120.).await;
                    link.enable_start_stop_sync(false);
                    link
                })
                .await
            })
        }))
        .map_err(|panic| {
            OutputError::Driver(format!(
                "Link initialization failed: {}",
                panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("network initialization panicked")
            ))
        })?
        .map_err(|_| OutputError::Driver("Link initialization timed out".into()))?;
        Ok(Self {
            link,
            runtime,
            snapshot: None,
            aligned: false,
            peers: 0,
            next_publish: Instant::now(),
            stopped: false,
        })
    }
}

/// Return the audio clock's beat count with detected downbeats on quantum boundaries.
fn target(snapshot: &RhythmSnapshot, now: Instant) -> Option<(f64, f64, f64)> {
    if !snapshot.active(now) {
        return None;
    }
    let beat = snapshot.grids[1].filter(|grid| grid.valid())?;
    let quantum = f64::from(snapshot.meter.filter(|meter| *meter > 0).unwrap_or(4));
    let origin = snapshot.grids[0]
        .filter(|grid| grid.valid())
        .map_or(beat.anchor, |bar| bar.anchor);
    Some((
        60. / beat.period,
        (snapshot.time_at(now) - origin) / beat.period,
        quantum,
    ))
}

fn corrected_beat(current: f64, desired: f64, quantum: f64, bpm: f64) -> f64 {
    let error = (desired - current + quantum * 0.5).rem_euclid(quantum) - quantum * 0.5;
    let limit = 0.002 * bpm / 60.;
    current + error.clamp(-limit, limit)
}

impl OutputDriver for LinkOutput {
    fn name(&self) -> &str {
        "link"
    }
    fn reset(&mut self) {
        self.snapshot = None;
        self.aligned = false;
        self.next_publish = Instant::now();
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        // A current-thread runtime only progresses while block_on is running.
        // Yielding lets discovery and measurement tasks run without sleeping here.
        self.runtime.block_on(tokio::task::yield_now());
        let peers = self.link.num_peers();
        if self.peers == 0 && peers > 0 {
            // Joining the first peer can replace Link's seed session timeline.
            // Establish the audio phase once in that session before bounded updates.
            self.aligned = false;
            self.next_publish = now;
        }
        self.peers = peers;
        if now < self.next_publish {
            return Ok(Some(self.next_publish));
        }
        self.next_publish = now + PUBLISH_INTERVAL;
        let Some(snapshot) = self.snapshot else {
            return Ok(Some(self.next_publish));
        };
        if target(&snapshot, now).is_none() {
            self.aligned = false;
            return Ok(Some(self.next_publish));
        }
        if !self.link.is_enabled() {
            self.runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(1), self.link.enable()).await
                })
                .map_err(|_| OutputError::Driver("Link discovery startup timed out".into()))?;
        }
        // Pair Link's clock reading with a fresh source-time projection, including
        // time spent enabling discovery, instead of pairing it with an old poll time.
        let Some((bpm, desired, quantum)) = target(&snapshot, Instant::now()) else {
            self.aligned = false;
            return Ok(Some(self.next_publish));
        };
        let at = self.link.clock().micros();
        let mut state = self.link.capture_app_session_state();
        state.set_tempo(bpm, at);
        let beat = if self.aligned {
            corrected_beat(state.beat_at_time(at, quantum), desired, quantum, bpm)
        } else {
            desired
        };
        state.force_beat_at_time(beat, at, quantum);
        self.runtime
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    self.link.commit_app_session_state(state),
                )
                .await
            })
            .map_err(|_| OutputError::Driver("Link timeline publication timed out".into()))?;
        self.aligned = true;
        Ok(Some(self.next_publish))
    }
    fn status(&self) -> String {
        if self.link.is_enabled() {
            format!("{} peers", self.link.num_peers())
        } else {
            "Waiting for an audio clock".into()
        }
    }
    fn shutdown(&mut self) -> Result<(), OutputError> {
        if !self.stopped {
            self.stopped = true;
            self.runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_millis(250), self.link.disable()).await
                })
                .map_err(|_| OutputError::Driver("Link shutdown timed out".into()))?;
        }
        Ok(())
    }
}

impl Drop for LinkOutput {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Tracking,
        rhythm::{Provenance, PulseGrid, Transport},
    };

    #[test]
    fn correction_wraps_the_bar_and_is_limited_to_two_milliseconds() {
        assert!((corrected_beat(3.999, 0.001, 4., 120.) - 4.001).abs() < 1e-10);
        assert!((corrected_beat(1., 2., 4., 120.) - 1.004).abs() < 1e-10);
        assert!((corrected_beat(2., 1., 4., 180.) - 1.994).abs() < 1e-10);
    }

    #[test]
    fn target_uses_effective_grid_tempo_downbeat_and_offset() {
        let now = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(now, Tracking::Assisted);
        snapshot.transport = Transport::Tracking;
        snapshot.valid_until = now + Duration::from_secs(1);
        snapshot.source_time = 2.;
        snapshot.offset_seconds = 0.1;
        snapshot.bpm = Some(125.); // Display tempo deliberately differs from clock.
        snapshot.meter = Some(3);
        snapshot.grids = [
            Some(PulseGrid {
                anchor: 0.5,
                period: 1.5,
                provenance: Provenance::Derived,
            }),
            Some(PulseGrid {
                anchor: 0.,
                period: 0.5,
                provenance: Provenance::Detected,
            }),
            None,
        ];
        let (bpm, beat, quantum) = target(&snapshot, now).unwrap();
        assert_eq!((bpm, quantum), (120., 3.));
        assert!((beat - 2.8).abs() < 1e-10);
        assert!(target(&snapshot, now + Duration::from_secs(2)).is_none());
    }

    /// The probe is built from the official Ableton C++ library, not this Rust crate.
    /// Its stdout contains: wall_microseconds tempo phase peers is_playing.
    #[test]
    #[ignore = "requires TEMPOTRACK_LINK_REFERENCE pointing to the official C++ tempo probe and LAN sockets"]
    fn interoperates_with_official_link_reference() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
            sync::mpsc,
            time::{SystemTime, UNIX_EPOCH},
        };

        let executable = std::env::var("TEMPOTRACK_LINK_REFERENCE")
            .expect("set TEMPOTRACK_LINK_REFERENCE to the official C++ tempo probe");
        let mut peer = Command::new(executable)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = peer.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut output = LinkOutput::open().unwrap();
        let start = Instant::now();
        let wall_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let initial_period = 60. / 131.7;
        let changed_period = 60. / 137.3;
        let mut samples = Vec::new();
        let mut peer_counts = Vec::new();
        while start.elapsed() < Duration::from_secs(12) {
            let now = Instant::now();
            let elapsed = start.elapsed().as_secs_f64();
            let (period, anchor) = if elapsed < 6. {
                (initial_period, 0.)
            } else {
                (changed_period, 6. - 6. / initial_period * changed_period)
            };
            let mut snapshot = RhythmSnapshot::empty(start, Tracking::Assisted);
            snapshot.transport = Transport::Tracking;
            snapshot.valid_until = now + Duration::from_millis(250);
            snapshot.meter = Some(4);
            snapshot.grids[1] = Some(PulseGrid {
                anchor,
                period,
                provenance: Provenance::Detected,
            });
            output.on_timeline(snapshot).unwrap();
            output.poll(now).unwrap();
            if elapsed > 3. {
                peer_counts.push(output.link.num_peers());
            }
            for line in rx.try_iter() {
                let values: Vec<f64> = line
                    .split_whitespace()
                    .map(|value| value.parse().unwrap())
                    .collect();
                if values.len() == 5
                    && ((3.0..5.0).contains(&(values[0] / 1e6 - wall_start))
                        || values[0] / 1e6 - wall_start > 9.)
                {
                    samples.push(values);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!(
            "Link status: {}; latest reference: {:?}",
            output.status(),
            samples.last()
        );
        let shutdown = output.shutdown();
        let _ = peer.kill();
        let _ = peer.wait();
        reader.join().unwrap();
        drop(output);
        assert!(samples.len() >= 10, "official peer did not provide samples");
        assert!(
            peer_counts.iter().all(|count| *count >= 1),
            "Rust peer count dropped during an active connection: {} zero observations",
            peer_counts.iter().filter(|count| **count == 0).count()
        );
        for sample in samples {
            assert!(
                sample[3] >= 1.,
                "official peer did not discover TempoTrack: {sample:?}"
            );
            let elapsed = sample[0] / 1e6 - wall_start;
            let (bpm, period, desired) = if elapsed < 6. {
                (131.7, initial_period, elapsed / initial_period)
            } else {
                (
                    137.3,
                    changed_period,
                    6. / initial_period + (elapsed - 6.) / changed_period,
                )
            };
            assert!(
                (sample[1] - bpm).abs() < 0.02,
                "official peer tempo: {sample:?}"
            );
            assert_eq!(sample[4], 0., "no Start/Stop sync should occur");
            let error_seconds = ((sample[2] - desired + 2.).rem_euclid(4.) - 2.).abs() * period;
            assert!(
                error_seconds < 0.02,
                "official peer phase error {error_seconds}s: {sample:?}"
            );
        }
        shutdown.unwrap();
    }
}
