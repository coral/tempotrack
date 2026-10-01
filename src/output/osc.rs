//! General-purpose OSC clock over UDP. All timestamps are immediate bundles;
//! receiving applications need no synchronized wall clock.
use super::{OutputDriver, OutputError, schedule::ClockCursor};
use crate::rhythm::RhythmSnapshot;
use rosc::{OscBundle, OscMessage, OscPacket, OscType};
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 30);
const HEARTBEAT: Duration = Duration::from_secs(1);

pub struct OscOutput {
    socket: UdpSocket,
    destination: SocketAddr,
    prefix: String,
    snapshot: Option<RhythmSnapshot>,
    cursors: [ClockCursor; 3],
    origins: [Option<i64>; 3],
    active: Option<bool>,
    next_frame: Instant,
    next_heartbeat: Instant,
}

impl OscOutput {
    pub fn open(target: &str, prefix: &str) -> Result<Self, OutputError> {
        let mut last_error = None;
        for destination in target.to_socket_addrs()? {
            let bind = if destination.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            match UdpSocket::bind(bind).and_then(|socket| {
                socket.connect(destination)?;
                socket.set_nonblocking(true)?;
                Ok(socket)
            }) {
                Ok(socket) => {
                    let now = Instant::now();
                    return Ok(Self {
                        socket,
                        destination,
                        prefix: prefix.trim_end_matches('/').into(),
                        snapshot: None,
                        cursors: [
                            ClockCursor::new(1),
                            ClockCursor::new(1),
                            ClockCursor::new(4),
                        ],
                        origins: [None; 3],
                        active: None,
                        next_frame: now,
                        next_heartbeat: now,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.map_or_else(
            || OutputError::Driver(format!("OSC destination {target:?} has no addresses")),
            OutputError::Io,
        ))
    }

    fn message(&self, suffix: &str, value: OscType) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: format!("{}{suffix}", self.prefix),
            args: vec![value],
        })
    }

    fn send(&self, content: Vec<OscPacket>) -> Result<(), OutputError> {
        if content.is_empty() {
            return Ok(());
        }
        let bytes = rosc::encoder::encode(&OscPacket::Bundle(OscBundle {
            timetag: (0, 1).into(),
            content,
        }))
        .map_err(|error| OutputError::Driver(format!("OSC encoding: {error}")))?;
        match self.socket.send(&bytes) {
            Ok(_) => Ok(()),
            // A congested destination must not accumulate historical clock events.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn advance(deadline: Instant, now: Instant, interval: Duration) -> Instant {
    let skipped = now.saturating_duration_since(deadline).as_nanos() / interval.as_nanos();
    // Retain the original cadence while skipping missed frames.
    deadline + interval.mul_f64((skipped + 1) as f64)
}

impl OutputDriver for OscOutput {
    fn name(&self) -> &str {
        "osc"
    }
    fn status(&self) -> String {
        format!("Sending · {}", self.destination)
    }
    fn reset(&mut self) {
        self.snapshot = None;
        self.cursors.iter_mut().for_each(ClockCursor::reset);
        self.origins = [None; 3];
    }
    fn on_timeline(&mut self, snapshot: RhythmSnapshot) -> Result<(), OutputError> {
        if self
            .snapshot
            .is_some_and(|old| old.generation != snapshot.generation)
        {
            self.reset();
        }
        self.snapshot = Some(snapshot);
        Ok(())
    }
    fn poll(&mut self, now: Instant) -> Result<Option<Instant>, OutputError> {
        let active = self.snapshot.is_some_and(|snapshot| {
            snapshot.active(now) && snapshot.grids[1].is_some_and(|grid| grid.valid())
        });
        // Most scheduler polls have no OSC event. Allocate only when sending.
        let mut messages = Vec::new();
        if self.active != Some(active) || now >= self.next_heartbeat {
            messages.push(self.message("/active", OscType::Int(i32::from(active))));
            self.next_heartbeat = if now >= self.next_heartbeat {
                advance(self.next_heartbeat, now, HEARTBEAT)
            } else {
                now + HEARTBEAT
            };
        }
        self.active = Some(active);
        let mut next = self.next_heartbeat;
        if let Some(snapshot) = self.snapshot {
            if active && now >= self.next_frame {
                let grid = snapshot.grids[1].expect("active requires a valid beat grid");
                messages.push(self.message("/bpm", OscType::Float((60. / grid.period) as f32)));
                messages.push(
                    self.message(
                        "/phase",
                        // Rounding a phase just below one to f32 must not produce
                        // one: receivers treat phase as a half-open interval.
                        OscType::Float(
                            (grid.phase(snapshot.time_at(now)) as f32)
                                .min(f32::from_bits(1_f32.to_bits() - 1)),
                        ),
                    ),
                );
                self.next_frame = advance(self.next_frame, now, FRAME);
            }
            if active {
                next = next.min(self.next_frame);
            }
            for (index, suffix) in ["/bar", "/beat", "/atom"].into_iter().enumerate() {
                let mut timeline = snapshot;
                if !active {
                    timeline.transport = crate::rhythm::Transport::Stopped;
                }
                // Beat and atom share the stable beat grid; bar uses its own
                // anchor so detected downbeats preserve their position.
                if index == 0 {
                    timeline.grids[1] = timeline.grids[0];
                }
                if let Some(tick) = self.cursors[index].poll(&timeline, now) {
                    let origin = *self.origins[index].get_or_insert(tick.index);
                    let count = tick
                        .index
                        .saturating_sub(origin)
                        .clamp(0, i64::from(i32::MAX));
                    messages.push(self.message(suffix, OscType::Int(count as i32)));
                }
                if let Some(deadline) = self.cursors[index].next_deadline(&timeline, now) {
                    next = next.min(deadline);
                }
            }
        }
        self.send(messages)?;
        Ok(Some(next))
    }
    fn shutdown(&mut self) -> Result<(), OutputError> {
        self.send(vec![self.message("/active", OscType::Int(0))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Tracking,
        rhythm::{Provenance, PulseGrid, Transport},
    };

    fn receive(socket: &UdpSocket) -> Vec<OscMessage> {
        let mut bytes = [0; 2048];
        let size = socket.recv(&mut bytes).unwrap();
        let (rest, packet) = rosc::decoder::decode_udp(&bytes[..size]).unwrap();
        assert!(rest.is_empty());
        let OscPacket::Bundle(bundle) = packet else {
            panic!("expected a bundle")
        };
        assert_eq!(bundle.timetag, (0, 1).into());
        bundle
            .content
            .into_iter()
            .map(|packet| {
                let OscPacket::Message(message) = packet else {
                    panic!("nested bundle")
                };
                message
            })
            .collect()
    }
    fn setup() -> (OscOutput, UdpSocket, RhythmSnapshot, Instant) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let output = OscOutput::open(&socket.local_addr().unwrap().to_string(), "/test").unwrap();
        let now = Instant::now();
        let mut snapshot = RhythmSnapshot::empty(now, Tracking::Assisted);
        snapshot.transport = Transport::Tracking;
        snapshot.source_time = 0.01;
        snapshot.valid_until = now + Duration::from_secs(10);
        snapshot.bpm = Some(99.); // OSC must use effective grid frequency.
        snapshot.grids = [2., 0.5, 0.125].map(|period| {
            Some(PulseGrid {
                anchor: 0.,
                period,
                provenance: Provenance::Detected,
            })
        });
        (output, socket, snapshot, now)
    }
    #[test]
    fn publishes_effective_tempo_and_phase_then_inactive_without_transport() {
        let (mut output, socket, snapshot, now) = setup();
        output.on_timeline(snapshot).unwrap();
        output.poll(now).unwrap();
        let messages = receive(&socket);
        assert!(messages.contains(&OscMessage {
            addr: "/test/bpm".into(),
            args: vec![OscType::Float(120.)]
        }));
        assert!(messages.contains(&OscMessage {
            addr: "/test/phase".into(),
            args: vec![OscType::Float(0.02)]
        }));
        assert!(messages.contains(&OscMessage {
            addr: "/test/active".into(),
            args: vec![OscType::Int(1)]
        }));
        output.poll(now + Duration::from_secs(11)).unwrap();
        assert_eq!(
            receive(&socket),
            vec![OscMessage {
                addr: "/test/active".into(),
                args: vec![OscType::Int(0)]
            }]
        );
    }
    #[test]
    fn pulse_events_are_not_throttled_to_telemetry_frames() {
        let (mut output, socket, snapshot, now) = setup();
        output.on_timeline(snapshot).unwrap();
        output.poll(now).unwrap();
        receive(&socket);
        output.poll(now + Duration::from_millis(110)).unwrap();
        receive(&socket);
        // 115 ms reaches the atom boundary and is not a telemetry-aligned time.
        output.poll(now + Duration::from_micros(115_010)).unwrap();
        let messages = receive(&socket);
        assert!(messages.iter().any(|message| message.addr == "/test/atom"));
        assert!(!messages.iter().any(|message| message.addr == "/test/beat"));
        assert!(!messages.iter().any(|message| message.addr == "/test/bpm"));
        output.shutdown().unwrap();
        assert_eq!(receive(&socket)[0].addr, "/test/active");
    }

    #[test]
    fn new_generation_resets_counters_without_historical_bursts() {
        let (mut output, socket, mut snapshot, now) = setup();
        output.on_timeline(snapshot).unwrap();
        output.poll(now).unwrap();
        receive(&socket);
        output.poll(now + Duration::from_micros(490_010)).unwrap();
        let messages = receive(&socket);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.addr == "/test/beat")
                .count(),
            1
        );
        assert!(messages.contains(&OscMessage {
            addr: "/test/beat".into(),
            args: vec![OscType::Int(0)]
        }));
        snapshot.generation += 1;
        snapshot.source_time = 8.01;
        snapshot.reference_time = now + Duration::from_secs(1);
        output.on_timeline(snapshot).unwrap();
        output.poll(snapshot.reference_time).unwrap();
        assert!(
            !receive(&socket)
                .iter()
                .any(|message| message.addr == "/test/beat")
        );
        output
            .poll(snapshot.reference_time + Duration::from_micros(490_010))
            .unwrap();
        assert!(receive(&socket).contains(&OscMessage {
            addr: "/test/beat".into(),
            args: vec![OscType::Int(0)]
        }));
    }
}
