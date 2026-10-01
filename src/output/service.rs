use super::{
    OutputDriver, OutputError, OutputRunner, OutputStatus,
    config::{MidiPort, OutputConfig},
};
use crate::{Error, rhythm::RhythmSnapshot};
use rtrb::{Producer, RingBuffer};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The entire output-facing interface used on the analysis thread: one bounded push.
pub struct OutputPublisher {
    producer: Producer<RhythmSnapshot>,
}
impl OutputPublisher {
    pub fn publish(&mut self, snapshot: RhythmSnapshot) {
        let _ = self.producer.push(snapshot);
    }
}
#[derive(Clone, PartialEq, Eq)]
enum Spec {
    Link,
    Osc(String, String),
    Midi(MidiPort),
    VirtualMidi,
    Rtp(String, u16),
}
impl Spec {
    fn id(&self) -> String {
        match self {
            Self::Link => "link".into(),
            Self::Rtp(..) => "rtpmidi".into(),
            Self::Osc(target, _) => format!("osc:{target}"),
            Self::Midi(port) => format!("midi:{}", port.id.as_ref().unwrap_or(&port.name)),
            Self::VirtualMidi => "midi:virtual".into(),
        }
    }
    fn open(&self) -> Result<Box<dyn OutputDriver>, OutputError> {
        Ok(match self {
            Self::Link => Box::new(super::link::LinkOutput::open()?),
            Self::Osc(target, prefix) => Box::new(super::osc::OscOutput::open(target, prefix)?),
            Self::Midi(port) => Box::new(super::midi::MidiOutput::open(port)?),
            Self::VirtualMidi => Box::new(super::midi::MidiOutput::virtual_port()?),
            Self::Rtp(name, port) => Box::new(super::rtp::RtpOutput::open(name, *port)?),
        })
    }
}
fn specs(config: &OutputConfig) -> Vec<Spec> {
    let mut result = vec![];
    if config.link {
        result.push(Spec::Link);
    }
    if config.osc.enabled {
        result.extend(
            config
                .osc
                .targets
                .iter()
                .map(|t| Spec::Osc(t.clone(), config.osc.prefix.clone())),
        );
    }
    if config.midi.enabled {
        result.extend(config.midi.ports.iter().cloned().map(Spec::Midi));
        if config.midi.virtual_port {
            result.push(Spec::VirtualMidi);
        }
    }
    if config.rtpmidi.enabled {
        result.push(Spec::Rtp(config.rtpmidi.name.clone(), config.rtpmidi.port));
    }
    result
}
pub struct OutputService {
    commands: Producer<OutputConfig>,
    statuses: Arc<Mutex<Vec<OutputStatus>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl OutputService {
    pub fn spawn(
        drivers: Vec<Box<dyn OutputDriver>>,
    ) -> Result<(Self, OutputPublisher), std::io::Error> {
        let (producer, mut timeline) = RingBuffer::new(128);
        let (commands, mut configs) = RingBuffer::<OutputConfig>::new(8);
        let statuses = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_status = statuses.clone();
        let worker_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("output-manager".into())
            .spawn(move || {
                let mut fixed = vec![];
                let mut failures = vec![];
                for driver in drivers {
                    let id = driver.name().to_owned();
                    match OutputRunner::spawn(driver) {
                        Ok(runner) => fixed.push(runner),
                        Err(e) => failures.push(OutputStatus {
                            id,
                            detail: e.to_string(),
                            failed: true,
                        }),
                    }
                }
                let mut active: Vec<(Spec, OutputRunner)> = vec![];
                let mut retired: Vec<OutputRunner> = vec![];
                let mut latest = None;
                let mut next_status = Instant::now();
                while !worker_stop.load(Ordering::Acquire) {
                    let mut desired = None;
                    while let Ok(config) = configs.pop() {
                        desired = Some(config);
                    }
                    if let Some(config) = desired {
                        next_status = Instant::now();
                        let specs = specs(&config);
                        let mut kept = vec![];
                        for (spec, runner) in active.drain(..) {
                            if specs.contains(&spec) {
                                kept.push((spec, runner));
                            } else {
                                runner.request_stop();
                                retired.push(runner);
                            }
                        }
                        active = kept;
                        failures.retain(|s| {
                            !s.id.starts_with("osc:")
                                && !s.id.starts_with("midi:")
                                && s.id != "link"
                                && s.id != "rtpmidi"
                        });
                        for spec in specs {
                            if active.iter().any(|(s, _)| *s == spec) {
                                continue;
                            }
                            let factory_spec = spec.clone();
                            match OutputRunner::spawn_factory(
                                spec.id(),
                                Box::new(move || factory_spec.open()),
                                true,
                            ) {
                                Ok(mut runner) => {
                                    if let Some(snapshot) = latest {
                                        runner.publish(snapshot);
                                    }
                                    active.push((spec, runner));
                                }
                                Err(e) => failures.push(OutputStatus {
                                    id: spec.id(),
                                    detail: e.to_string(),
                                    failed: true,
                                }),
                            }
                        }
                    }
                    let mut changed = false;
                    while let Ok(snapshot) = timeline.pop() {
                        latest = Some(snapshot);
                        changed = true;
                    }
                    if changed && let Some(snapshot) = latest {
                        for runner in &mut fixed {
                            runner.publish(snapshot);
                        }
                        for (_, runner) in &mut active {
                            runner.publish(snapshot);
                        }
                    }
                    // Joining only completed workers avoids stalling other destinations during teardown.
                    retired.retain(|runner| !runner.is_finished());
                    if Instant::now() >= next_status {
                        let states = fixed
                            .iter()
                            .chain(active.iter().map(|(_, r)| r))
                            .map(OutputRunner::status)
                            .chain(failures.iter().cloned())
                            .collect();
                        *worker_status.lock().unwrap_or_else(|e| e.into_inner()) = states;
                        next_status = Instant::now() + Duration::from_millis(100);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                for runner in fixed
                    .iter()
                    .chain(active.iter().map(|(_, r)| r))
                    .chain(retired.iter())
                {
                    runner.request_stop();
                }
                // All joins happen here, never in the analysis worker.
            })?;
        Ok((
            Self {
                commands,
                statuses,
                stop,
                thread: Some(thread),
            },
            OutputPublisher { producer },
        ))
    }
    pub fn configure(&mut self, config: OutputConfig) -> Result<(), Error> {
        config.validate()?;
        if self.thread.as_ref().is_none_or(|t| t.is_finished()) {
            return Err(Error::Config("output manager stopped".into()));
        }
        self.commands.push(config).map_err(|_| Error::Busy)
    }
    pub fn status(&self) -> Vec<OutputStatus> {
        self.statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for OutputService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Tracking;
    use std::sync::mpsc;

    struct SlowDriver {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl OutputDriver for SlowDriver {
        fn name(&self) -> &str {
            "slow-destination"
        }
        fn reset(&mut self) {}
        fn on_timeline(&mut self, _: RhythmSnapshot) -> Result<(), OutputError> {
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, OutputError> {
            let _ = self.entered.send(());
            let _ = self.release.recv_timeout(Duration::from_secs(2));
            Ok(None)
        }
    }
    struct FailedDriver;
    impl OutputDriver for FailedDriver {
        fn name(&self) -> &str {
            "failed-destination"
        }
        fn reset(&mut self) {}
        fn on_timeline(&mut self, _: RhythmSnapshot) -> Result<(), OutputError> {
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, OutputError> {
            Err(OutputError::Driver("endpoint unplugged".into()))
        }
    }
    struct HealthyDriver(mpsc::Sender<u64>);
    impl OutputDriver for HealthyDriver {
        fn name(&self) -> &str {
            "healthy-destination"
        }
        fn reset(&mut self) {}
        fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
            let _ = self.0.send(snapshot.sequence);
            Ok(())
        }
        fn poll(&mut self, _: Instant) -> Result<Option<Instant>, OutputError> {
            Ok(None)
        }
    }
    #[test]
    fn failed_and_blocked_destinations_do_not_stall_manager_or_healthy_output() {
        let (entered, blocked) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (delivered, received) = mpsc::channel();
        let (mut service, mut publisher) = OutputService::spawn(vec![
            Box::new(SlowDriver {
                entered,
                release: gate,
            }),
            Box::new(FailedDriver),
            Box::new(HealthyDriver(delivered)),
        ])
        .unwrap();
        blocked.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut snapshot = RhythmSnapshot::empty(Instant::now(), Tracking::Assisted);
        snapshot.sequence = 99;
        publisher.publish(snapshot);
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 99);
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let states = service.status();
            if let Some(failed) = states
                .iter()
                .find(|state| state.id == "failed-destination" && state.failed)
            {
                assert!(failed.detail.contains("endpoint unplugged"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "manager stopped updating statuses"
            );
            thread::sleep(Duration::from_millis(1));
        }
        service.configure(OutputConfig::default()).unwrap();
        snapshot.sequence = 100;
        publisher.publish(snapshot);
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 100);
        service.stop.store(true, Ordering::Release);
        // Unblock the synthetic I/O operation before joining all workers.
        drop(release);
        service.shutdown();
    }
    #[test]
    fn full_analysis_ring_drops_excess_without_growing_or_waiting() {
        let (producer, mut consumer) = RingBuffer::new(4);
        let mut publisher = OutputPublisher { producer };
        let mut snapshot = RhythmSnapshot::empty(Instant::now(), Tracking::Assisted);
        for sequence in 0..10_000 {
            snapshot.sequence = sequence;
            publisher.publish(snapshot);
        }
        let queued: Vec<_> = std::iter::from_fn(|| consumer.pop().ok())
            .map(|snapshot| snapshot.sequence)
            .collect();
        assert_eq!(queued, [0, 1, 2, 3]);
        publisher.publish(snapshot);
        assert_eq!(consumer.pop().unwrap().sequence, 9_999);
    }
}
