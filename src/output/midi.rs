//! Local MIDI clock. Connection management belongs to the output worker.
use super::{OutputDriver, OutputError, config::MidiPort, schedule::ClockCursor};
use crate::rhythm::RhythmSnapshot;
use midir::{MidiOutput as Client, MidiOutputConnection};
use std::time::{Duration, Instant};

pub const VIRTUAL_PORT_NAME: &str = "TempoTrack Clock";

fn driver_error(error: impl std::fmt::Display) -> OutputError {
    OutputError::Driver(format!("MIDI: {error}"))
}

pub fn list_ports() -> Result<Vec<MidiPort>, OutputError> {
    let client = Client::new("TempoTrack discovery").map_err(driver_error)?;
    client
        .ports()
        .iter()
        .map(|port| {
            Ok(MidiPort {
                id: Some(port.id()),
                name: client.port_name(port).map_err(driver_error)?,
            })
        })
        .collect()
}

fn select_port(selection: &MidiPort, ports: &[MidiPort]) -> Result<usize, OutputError> {
    if let Some(id) = &selection.id
        && let Some(index) = ports.iter().position(|port| port.id.as_ref() == Some(id))
    {
        return Ok(index);
    }
    let mut matches = ports
        .iter()
        .enumerate()
        .filter(|(_, port)| port.name == selection.name);
    match (matches.next(), matches.next()) {
        (Some((index, _)), None) => Ok(index),
        (None, _) => Err(driver_error(format!(
            "output {:?} is unavailable",
            selection.name
        ))),
        _ => Err(driver_error(format!(
            "output name {:?} is ambiguous; select its port ID",
            selection.name
        ))),
    }
}

pub struct MidiOutput {
    connection: MidiOutputConnection,
    name: String,
    timeline: MidiTimeline,
    monitor: Option<PortMonitor>,
}

struct PortMonitor {
    client: Client,
    id: String,
    next_check: Instant,
}

impl MidiOutput {
    pub fn open(selection: &MidiPort) -> Result<Self, OutputError> {
        let client = Client::new("TempoTrack").map_err(driver_error)?;
        let ports = client.ports();
        let descriptors = ports
            .iter()
            .map(|port| {
                Ok(MidiPort {
                    id: Some(port.id()),
                    name: client.port_name(port).map_err(driver_error)?,
                })
            })
            .collect::<Result<Vec<_>, OutputError>>()?;
        let index = select_port(selection, &descriptors)?;
        let monitor = PortMonitor {
            client: Client::new("TempoTrack port monitor").map_err(driver_error)?,
            id: ports[index].id(),
            next_check: Instant::now() + Duration::from_secs(1),
        };
        let connection = client
            .connect(&ports[index], VIRTUAL_PORT_NAME)
            .map_err(driver_error)?;
        Ok(Self {
            connection,
            name: descriptors[index].name.clone(),
            timeline: MidiTimeline::default(),
            monitor: Some(monitor),
        })
    }

    #[cfg(unix)]
    pub fn virtual_port() -> Result<Self, OutputError> {
        Self::virtual_named(VIRTUAL_PORT_NAME)
    }

    #[cfg(unix)]
    fn virtual_named(name: &str) -> Result<Self, OutputError> {
        use midir::os::unix::VirtualOutput;
        let connection = Client::new("TempoTrack")
            .map_err(driver_error)?
            .create_virtual(name)
            .map_err(driver_error)?;
        Ok(Self {
            connection,
            name: name.into(),
            timeline: MidiTimeline::default(),
            monitor: None,
        })
    }

    #[cfg(not(unix))]
    pub fn virtual_port() -> Result<Self, OutputError> {
        Err(driver_error(
            "virtual outputs are supported on macOS and Linux",
        ))
    }
}

struct MidiTimeline {
    snapshot: Option<RhythmSnapshot>,
    cursor: ClockCursor,
}
impl Default for MidiTimeline {
    fn default() -> Self {
        Self {
            snapshot: None,
            cursor: ClockCursor::new(24),
        }
    }
}
impl MidiTimeline {
    fn poll(
        &mut self,
        now: Instant,
        mut send: impl FnMut(&[u8]) -> Result<(), OutputError>,
    ) -> Result<Option<Instant>, OutputError> {
        let Some(snapshot) = self.snapshot else {
            return Ok(None);
        };
        if self.cursor.poll(&snapshot, now).is_some() {
            // Timing Clock is the entire protocol: never synthesize transport.
            send(&[0xf8])?;
        }
        Ok(self.cursor.next_deadline(&snapshot, now))
    }
}
impl OutputDriver for MidiOutput {
    fn name(&self) -> &str {
        "midi"
    }
    fn status(&self) -> String {
        format!("Connected · {}", self.name)
    }
    fn reset(&mut self) {
        self.timeline = MidiTimeline::default();
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        self.timeline.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        if let Some(monitor) = &mut self.monitor
            && now >= monitor.next_check
        {
            // ALSA may successfully send to zero subscribers after unplugging
            // a port. Check its identity even while the clock is inactive.
            if !monitor
                .client
                .ports()
                .iter()
                .any(|port| port.id() == monitor.id)
            {
                return Err(driver_error(format!("output {:?} disconnected", self.name)));
            }
            monitor.next_check = now + Duration::from_secs(1);
        }
        let connection = &mut self.connection;
        self.timeline
            .poll(now, |bytes| connection.send(bytes).map_err(driver_error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Tracking,
        rhythm::{Provenance, PulseGrid, Transport},
    };
    use std::time::Duration;

    fn port(id: &str, name: &str) -> MidiPort {
        MidiPort {
            id: Some(id.into()),
            name: name.into(),
        }
    }
    #[test]
    fn ids_win_and_name_fallback_must_be_unambiguous() {
        let ports = [
            port("a", "Duplicate"),
            port("b", "Duplicate"),
            port("c", "Unique"),
        ];
        assert_eq!(select_port(&port("b", "Old name"), &ports).unwrap(), 1);
        assert_eq!(select_port(&port("gone", "Unique"), &ports).unwrap(), 2);
        assert!(select_port(&port("gone", "Duplicate"), &ports).is_err());
        assert!(select_port(&port("gone", "Missing"), &ports).is_err());
    }
    #[test]
    fn clock_emits_only_timing_bytes_and_stops_when_stale() {
        let now = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(now, Tracking::Assisted);
        snapshot.transport = Transport::Tracking;
        snapshot.valid_until = now + Duration::from_secs(1);
        snapshot.grids[1] = Some(PulseGrid {
            anchor: 0.,
            period: 0.5,
            provenance: Provenance::Detected,
        });
        let mut timeline = MidiTimeline {
            snapshot: Some(snapshot),
            ..MidiTimeline::default()
        };
        let mut packets = Vec::new();
        // Initialize just inside the beat, then visit exactly 24 future boundaries.
        timeline
            .poll(now + Duration::from_millis(1), |_| Ok(()))
            .unwrap();
        for tick in 1..=24 {
            timeline
                .poll(
                    now + Duration::from_secs_f64(f64::from(tick) / 48. + 0.00001),
                    |bytes| {
                        packets.push(bytes.to_vec());
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert_eq!(packets, vec![vec![0xf8]; 24]);
        timeline
            .poll(now + Duration::from_secs(2), |_| {
                panic!("stale clock sent MIDI")
            })
            .unwrap();
    }

    #[cfg(unix)]
    fn unique_name(label: &str) -> String {
        format!(
            "TempoTrack test {label} {} {}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// Uses real CoreMIDI / ALSA endpoints. Opt in where a MIDI service exists:
    /// cargo test virtual_midi_loopback -- --ignored --nocapture
    #[cfg(unix)]
    #[test]
    #[ignore = "requires a running CoreMIDI or ALSA MIDI service"]
    fn virtual_midi_loopback_sends_24_clocks_and_removes_endpoint() {
        use midir::{Ignore, MidiInput};
        use std::{sync::mpsc, thread};
        let name = unique_name("clock");
        let mut output = MidiOutput::virtual_named(&name).unwrap();
        let mut input = MidiInput::new("TempoTrack clock test receiver").unwrap();
        input.ignore(Ignore::None);
        let deadline = Instant::now() + Duration::from_secs(2);
        let port = loop {
            if let Some(port) = input.ports().into_iter().find(|port| {
                input
                    .port_name(port)
                    .is_ok_and(|label| label.contains(&name))
            }) {
                break port;
            }
            assert!(Instant::now() < deadline, "virtual source did not appear");
            thread::sleep(Duration::from_millis(10));
        };
        let id = port.id();
        let (sender, receiver) = mpsc::sync_channel(128);
        let connection = input
            .connect(
                &port,
                "TempoTrack clock test",
                move |_, bytes, _| {
                    let _ = sender.try_send(bytes.to_vec());
                },
                (),
            )
            .unwrap();
        let now = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(now, Tracking::Assisted);
        snapshot.transport = Transport::Tracking;
        snapshot.valid_until = now + Duration::from_secs(2);
        snapshot.grids[1] = Some(PulseGrid {
            anchor: 0.,
            period: 0.5,
            provenance: Provenance::Detected,
        });
        output.on_timeline(snapshot).unwrap();
        output.poll(now).unwrap();
        for tick in 1..=24 {
            let due = now + Duration::from_secs_f64(f64::from(tick) / 48.);
            thread::sleep(due.saturating_duration_since(Instant::now()));
            output.poll(Instant::now()).unwrap();
        }
        output.shutdown().unwrap();
        for _ in 0..24 {
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
                [0xf8]
            );
        }
        assert!(
            receiver.recv_timeout(Duration::from_millis(50)).is_err(),
            "unexpected clock or transport message"
        );
        drop(output);
        let probe = MidiInput::new("TempoTrack cleanup probe").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.ports().iter().any(|port| port.id() == id) {
            assert!(
                Instant::now() < deadline,
                "virtual endpoint survived output drop"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(connection);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a running CoreMIDI or ALSA MIDI service"]
    fn virtual_midi_destination_disconnection_is_detected_and_reopens_by_name() {
        use midir::{Ignore, MidiInput, os::unix::VirtualInput};
        use std::thread;
        let name = unique_name("destination");
        let receiver = || {
            let mut input = MidiInput::new("TempoTrack reconnect test").unwrap();
            input.ignore(Ignore::None);
            input.create_virtual(&name, |_, _, _| {}, ()).unwrap()
        };
        let endpoint = receiver();
        let find = || {
            list_ports()
                .unwrap()
                .into_iter()
                .find(|port| port.name.contains(&name))
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let selection = loop {
            if let Some(port) = find() {
                break port;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        };
        let mut output = MidiOutput::open(&selection).unwrap();
        drop(endpoint);
        let deadline = Instant::now() + Duration::from_secs(2);
        while find().is_some() {
            assert!(Instant::now() < deadline, "destination survived drop");
            thread::sleep(Duration::from_millis(10));
        }
        output.monitor.as_mut().unwrap().next_check = Instant::now();
        assert!(
            output
                .poll(Instant::now())
                .unwrap_err()
                .to_string()
                .contains("disconnected")
        );
        drop(output);
        let _replacement = receiver();
        let deadline = Instant::now() + Duration::from_secs(2);
        while find().is_none() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        // Persisted ID may now be gone; the unique exact name is the fallback.
        let _reconnected = MidiOutput::open(&selection).unwrap();
    }
}
