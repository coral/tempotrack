//! Apple network MIDI listener. Its runtime and discovery daemon belong only to this output.
use super::{OutputDriver, OutputError, schedule::ClockCursor};
use crate::rhythm::RhythmSnapshot;
use mdns_sd::{DaemonEvent, IfKind, Receiver, ServiceDaemon, ServiceInfo};
use rtpmidi::{
    packets::midi_packets::rtp_midi_message::RtpMidiMessage,
    sessions::{invite_responder::InviteResponder, rtp_midi_session::RtpMidiSession},
};
use std::{
    hash::{BuildHasher, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::runtime::{Builder, Runtime};

const SERVICE_TYPE: &str = "_apple-midi._udp.local.";

pub struct RtpOutput {
    session: Arc<RtpMidiSession>,
    runtime: Runtime,
    mdns: ServiceDaemon,
    monitor: Receiver<DaemonEvent>,
    registered_name: String,
    advertised_name: String,
    participants: Vec<String>,
    next_status: Instant,
    snapshot: Option<RhythmSnapshot>,
    cursor: ClockCursor,
    clock: RtpMidiMessage<'static>,
    stopped: bool,
}

fn service(name: &str, port: u16, identity: u32) -> Result<ServiceInfo, OutputError> {
    // rtpmidi currently binds IPv4 only, so publish only reachable IPv4 endpoints.
    ServiceInfo::new(
        SERVICE_TYPE,
        name,
        &format!("tempotrack-{identity:08x}.local."),
        "",
        port,
        &[] as &[(&str, &str)],
    )
    .map(ServiceInfo::enable_addr_auto)
    .map_err(|error| OutputError::Driver(format!("RTP-MIDI discovery: {error}")))
}

impl RtpOutput {
    pub fn open(name: &str, port: u16) -> Result<Self, OutputError> {
        if port == 0
            || port == u16::MAX
            || name.trim().is_empty()
            || name.len() > 63
            || name.contains('\0')
        {
            return Err(OutputError::Driver(
                "RTP-MIDI needs a 1–63 byte session name and a control port between 1 and 65534"
                    .into(),
            ));
        }
        let identity = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32;
        let info = service(name, port, identity)?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        // Advertise only after both the control socket and adjacent data socket bind.
        let session = runtime.block_on(RtpMidiSession::start(
            port,
            name,
            identity,
            InviteResponder::Accept,
        ))?;
        let mdns = ServiceDaemon::new()
            .map_err(|error| OutputError::Driver(format!("RTP-MIDI discovery: {error}")))?;
        let registration = (|| {
            mdns.disable_interface(IfKind::IPv6)?;
            let monitor = mdns.monitor()?;
            mdns.register(info.clone())?;
            Ok::<_, mdns_sd::Error>(monitor)
        })();
        let monitor = match registration {
            Ok(monitor) => monitor,
            Err(error) => {
                session.stop_immediately();
                let _ = mdns.shutdown();
                return Err(OutputError::Driver(format!("RTP-MIDI discovery: {error}")));
            }
        };
        Ok(Self {
            session,
            runtime,
            mdns,
            monitor,
            registered_name: info.get_fullname().to_owned(),
            advertised_name: name.to_owned(),
            participants: Vec::new(),
            next_status: Instant::now(),
            snapshot: None,
            cursor: ClockCursor::new(24),
            clock: midi_types::MidiMessage::TimingClock.into(),
            stopped: false,
        })
    }
}

impl OutputDriver for RtpOutput {
    fn name(&self) -> &str {
        "rtpmidi"
    }
    fn reset(&mut self) {
        self.snapshot = None;
        self.cursor.reset();
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        self.runtime.block_on(tokio::task::yield_now());
        while let Ok(event) = self.monitor.try_recv() {
            match event {
                DaemonEvent::Error(error) => {
                    return Err(OutputError::Driver(format!("RTP-MIDI discovery: {error}")));
                }
                DaemonEvent::NameChange(change) if change.original == self.registered_name => {
                    self.advertised_name = change
                        .new_name
                        .strip_suffix(&format!(".{SERVICE_TYPE}"))
                        .unwrap_or(&change.new_name)
                        .to_owned();
                }
                _ => {}
            }
        }
        if now >= self.next_status {
            self.participants = self
                .runtime
                .block_on(self.session.participants())
                .into_iter()
                .map(|participant| participant.name().to_string_lossy().into_owned())
                .collect();
            self.participants.sort();
            self.next_status = now + Duration::from_millis(500);
        }
        let Some(snapshot) = self.snapshot else {
            return Ok(None);
        };
        if self.cursor.poll(&snapshot, now).is_some() {
            self.runtime
                .block_on(async {
                    tokio::time::timeout(
                        Duration::from_millis(20),
                        self.session.send_midi(&self.clock),
                    )
                    .await
                })
                .map_err(|_| OutputError::Driver("RTP-MIDI send timed out".into()))??;
        }
        Ok(self.cursor.next_deadline(&snapshot, now))
    }
    fn status(&self) -> String {
        if self.participants.is_empty() {
            format!("{} · waiting for participants", self.advertised_name)
        } else {
            format!(
                "{} · {}",
                self.advertised_name,
                self.participants.join(", ")
            )
        }
    }
    fn shutdown(&mut self) -> Result<(), OutputError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let unregister = self.mdns.unregister(&self.registered_name).map(|receiver| {
            let _ = receiver.recv_timeout(Duration::from_millis(250));
        });
        let stop = self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(250), self.session.stop_gracefully()).await
        });
        self.session.stop_immediately();
        let shutdown = self.mdns.shutdown().map(|receiver| {
            let _ = receiver.recv_timeout(Duration::from_millis(250));
        });
        unregister.and(shutdown).map_err(|error| {
            OutputError::Driver(format!("RTP-MIDI discovery shutdown: {error}"))
        })?;
        stop.map_err(|_| OutputError::Driver("RTP-MIDI shutdown timed out".into()))?;
        Ok(())
    }
}

impl Drop for RtpOutput {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtpmidi::packets::midi_packets::midi_event::MidiEvent;

    #[test]
    fn clock_payload_is_exactly_one_realtime_byte() {
        let message = midi_types::MidiMessage::TimingClock;
        assert_eq!(message.len(), 1);
        let (event, remaining) = MidiEvent::from_be_bytes(&[0xf8], false, None).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(event.command(), &message.into());
        let mut encoded = Default::default();
        event.command().write(&mut encoded, None);
        assert_eq!(encoded.as_ref(), &[0xf8]);
    }

    #[test]
    fn advertisement_uses_apple_midi_control_port() {
        let info = service("Test Clock", 5004, 42).unwrap();
        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_port(), 5004);
        assert_eq!(info.get_fullname(), "Test Clock._apple-midi._udp.local.");
        assert_eq!(info.get_hostname(), "tempotrack-0000002a.local.");
    }

    #[test]
    fn invalid_port_and_name_fail_before_network_initialization() {
        for (name, port) in [
            ("Clock", 0),
            ("Clock", u16::MAX),
            ("", 5004),
            ("bad\0name", 5004),
        ] {
            assert!(RtpOutput::open(name, port).is_err());
        }
    }

    /// Exercises actual AppleMIDI control/data invitations and packet decoding,
    /// without MIDI hardware. Opt in because it opens UDP/mDNS sockets.
    #[test]
    #[ignore = "opens UDP and multicast sockets; run for local network integration validation"]
    fn listener_accepts_invitations_and_sends_only_clock() {
        use crate::{
            config::Tracking,
            rhythm::{Provenance, PulseGrid, Transport},
        };
        use rtpmidi::sessions::events::event_handling::MidiMessageEvent;
        use std::{net::UdpSocket, sync::Mutex};

        fn free_port_pair() -> u16 {
            for _ in 0..100 {
                let control = UdpSocket::bind("127.0.0.1:0").unwrap();
                let port = control.local_addr().unwrap().port();
                if port < u16::MAX && UdpSocket::bind(("127.0.0.1", port + 1)).is_ok() {
                    return port;
                }
            }
            panic!("no available adjacent UDP ports");
        }

        let output_port = free_port_pair();
        let mut output = RtpOutput::open("TempoTrack test", output_port).unwrap();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let client = runtime
            .block_on(RtpMidiSession::start(
                free_port_pair(),
                "Clock receiver",
                0xabcdef12,
                InviteResponder::Accept,
            ))
            .unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let received = bytes.clone();
        runtime.block_on(client.add_listener(MidiMessageEvent, move |(message, _)| {
            received.lock().unwrap().push(message);
        }));
        runtime.block_on(client.invite_participant(([127, 0, 0, 1], output_port).into()));
        let start = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(start, Tracking::Assisted);
        snapshot.transport = Transport::Tracking;
        snapshot.valid_until = start + Duration::from_secs(3);
        snapshot.grids[1] = Some(PulseGrid {
            anchor: 0.,
            period: 0.5,
            provenance: Provenance::Detected,
        });
        output.on_timeline(snapshot).unwrap();
        while start.elapsed() < Duration::from_secs(2) {
            output.poll(Instant::now()).unwrap();
            runtime.block_on(tokio::task::yield_now());
            if bytes.lock().unwrap().len() >= 12 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        output.shutdown().unwrap();
        runtime.block_on(client.stop_gracefully());
        let messages = bytes.lock().unwrap();
        assert!(
            messages.len() >= 12,
            "did not receive clock packets: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| *message == midi_types::MidiMessage::TimingClock)
        );
        // Runtime teardown and graceful stop release both ports.
        drop(output);
        let _control = UdpSocket::bind(("0.0.0.0", output_port)).unwrap();
        let _data = UdpSocket::bind(("0.0.0.0", output_port + 1)).unwrap();
    }
}
