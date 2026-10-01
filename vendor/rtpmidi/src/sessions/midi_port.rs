use super::rtp_midi_session::{RtpMidiSession, current_timestamp};
use super::rtp_port::RtpPort;
use crate::packets::control_packets::clock_sync_packet::ClockSyncPacket;
use crate::packets::control_packets::control_packet::ControlPacket;
use crate::packets::control_packets::session_initiation_packet::SessionInitiationPacketBody;
use crate::packets::midi_packets::midi_event::MidiEvent;
use crate::packets::midi_packets::midi_packet::MidiPacket;
use crate::packets::midi_packets::rtp_midi_message::RtpMidiMessage;
use crate::packets::packet::RtpMidiPacket;
use crate::participant::Participant;
use crate::sessions::events::event_handling::EventListeners;
use crate::sessions::rtp_midi_session::current_timestamp_u32;
use std::ffi::{CStr, CString};
use std::iter;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tracing::{Level, event, instrument};
use zerocopy::network_endian::{U16, U32, U64};

pub const MAX_MIDI_PACKET_SIZE: usize = 32768;

impl RtpPort for MidiPort {
    fn session_name(&self) -> &CStr {
        &self.name
    }

    fn ssrc(&self) -> U32 {
        self.ssrc
    }

    fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }

    fn participant_addr(participant: &Participant) -> SocketAddr {
        participant.midi_port_addr()
    }
}

pub(super) struct MidiPort {
    name: CString,
    ssrc: U32,
    start_time: Instant,
    sequence_number: Arc<Mutex<u16>>,
    socket: Arc<UdpSocket>,
}

impl MidiPort {
    pub async fn bind(port: u16, name: CString, ssrc: U32) -> std::io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?);

        Ok(MidiPort {
            ssrc,
            start_time: Instant::now(),
            name,
            sequence_number: Arc::new(Mutex::new(0)),
            socket,
        })
    }

    #[instrument(name = "MIDI", skip_all, fields(name = %ctx.name(), src, src_name))]
    pub async fn start(&self, ctx: &RtpMidiSession, listeners: Arc<Mutex<EventListeners>>, buf: &mut [u8; MAX_MIDI_PACKET_SIZE]) {
        let recv = self.socket.recv_from(buf).await;
        if recv.is_err() {
            event!(Level::ERROR, "Failed to receive data on MIDI port: {recv:?}");
            return;
        }

        let (amt, src) = recv.unwrap();
        tracing::Span::current().record("src", src.to_string());
        event!(Level::TRACE, "Received {amt} bytes");

        let packet = RtpMidiPacket::parse(&buf[..amt]);
        if packet.is_err() {
            event!(Level::ERROR, "Failed to parse RTP MIDI packet: {packet:?}");
            return;
        }

        let packet = packet.unwrap();
        event!(Level::TRACE, "Parsed RTP MIDI packet: {:?}", &packet);
        match packet {
            RtpMidiPacket::Control(control_packet) => match control_packet {
                ControlPacket::Invitation { body, name } => {
                    event!(Level::INFO, name = name.to_str().unwrap_or("Unknown"), "Received session invitation");
                    self.handle_invitation(body, name, src, ctx).await;
                }
                ControlPacket::Acceptance { body, name } => {
                    event!(Level::INFO, name = name.to_str().unwrap_or("Unknown"), "Received session acceptance");
                    if let Ok(participant) = self.handle_acceptance(body, ctx).await {
                        event!(Level::INFO, "Accepted MIDI port invitation from {participant}");
                        listeners.lock().await.notify_participant_joined(&participant);
                    }
                }
                ControlPacket::ClockSync(clock_sync_packet) => {
                    event!(Level::DEBUG, "Received clock sync from {}", src);
                    self.handle_clock_sync(clock_sync_packet, ctx).await;
                }
                ControlPacket::Termination(body) => {
                    event!(Level::INFO, "Received session termination from {}", src);
                    let mut part_lock = ctx.participants.lock().await;
                    if let Some(participant) = part_lock.remove(&body.sender_ssrc) {
                        listeners.lock().await.notify_participant_left(&participant);
                        event!(Level::INFO, "Removed participant: {participant}");
                    } else {
                        event!(Level::WARN, "No participant found for SSRC {}", body.sender_ssrc.get());
                    }
                }
                _ => {
                    event!(Level::WARN, "Unhandled control packet {:?}", control_packet);
                }
            },
            RtpMidiPacket::Midi(midi_packet) => {
                event!(Level::DEBUG, "Parsed MIDI packet: {:#?}", midi_packet);
                let mut seq = self.sequence_number.lock().await;
                *seq = midi_packet.sequence_number().get().wrapping_add(1);
                for command in midi_packet.commands() {
                    match command.command() {
                        RtpMidiMessage::MidiMessage(message) => {
                            event!(Level::DEBUG, "Received MIDI message: {message:?}");
                            let timestamp = u32::from(midi_packet.timestamp()) + command.delta_time();
                            listeners.lock().await.notify_midi_message(*message, timestamp);
                        }
                        RtpMidiMessage::SysEx(sysex) => {
                            event!(Level::DEBUG, "Received SysEx message: {sysex:?}");
                            listeners.lock().await.notify_sysex_packet(sysex);
                        }
                    }
                }
            }
        }
    }

    #[instrument(skip_all, fields(sender = %sender_name.to_str().unwrap_or("Unknown"), token = %body.initiator_token, src = %src))]
    async fn handle_invitation(&self, body: &SessionInitiationPacketBody, sender_name: &CStr, src: SocketAddr, ctx: &RtpMidiSession) {
        let invitation = ctx.pending_invitations.lock().await.remove(&body.sender_ssrc);
        match invitation {
            None => {
                event!(Level::WARN, "Received unexpected MIDI port invitation for SSRC {}", body.sender_ssrc.get());
            }
            Some(_inv) => {
                event!(Level::DEBUG, "Found pending invitation for SSRC {}", body.sender_ssrc.get());

                let ctrl_addr = SocketAddr::new(src.ip(), src.port() - 1);
                ctx.participants.lock().await.insert(
                    body.sender_ssrc,
                    Participant::new(ctrl_addr, false, Some(body.initiator_token), sender_name, body.sender_ssrc),
                );
                self.send_invitation_acceptance(body.initiator_token, src).await;
            }
        }
    }

    #[instrument(skip_all, fields(token = %ack_body.initiator_token))]
    async fn handle_acceptance(&self, ack_body: &SessionInitiationPacketBody, ctx: &RtpMidiSession) -> Result<Participant, &str> {
        let mut locked_pending_invitations = ctx.pending_invitations.lock().await;

        let inv = locked_pending_invitations.get(&ack_body.sender_ssrc).cloned();
        if inv.is_none() {
            event!(
                Level::WARN,
                ssrc = ack_body.sender_ssrc.get(),
                "Received Acceptance but no pending invitation found for this SSRC."
            );
            return Err("No pending invitation found");
        }

        let inv = inv.unwrap();
        if inv.token != ack_body.initiator_token {
            event!(Level::WARN, expected = inv.token.get(), "Received Acceptance with mismatched token",);
            return Err("Token mismatch in acceptance");
        }

        locked_pending_invitations.remove(&ack_body.sender_ssrc);
        drop(locked_pending_invitations);
        event!(Level::DEBUG, "Matched Acceptance for MIDI port invitation. Sending Clock Sync.");
        let ctrl_addr = SocketAddr::new(inv.addr.ip(), inv.addr.port() - 1);
        let participant = Participant::new(ctrl_addr, true, Some(inv.token), &inv.name, ack_body.sender_ssrc);
        ctx.participants.lock().await.insert(ack_body.sender_ssrc, participant.clone());
        let timestamps = [U64::new(0); 3];
        self.send_clock_sync(std::iter::once(&participant), timestamps, 1).await;
        Ok(participant)
    }

    #[instrument(skip_all, fields(count = count))]
    pub(super) async fn send_clock_sync<'a, I>(&self, participants: I, mut timestamps: [U64; 3], count: u8)
    where
        I: IntoIterator<Item = &'a Participant>,
    {
        if count > 2 {
            event!(Level::ERROR, "Invalid count for clock sync");
            return;
        }
        timestamps[count as usize] = current_timestamp(self.start_time);

        let packet = ControlPacket::new_clock_sync_as_bytes(count, timestamps, self.ssrc);
        for participant in participants {
            if let Err(e) = self.socket.send_to(&packet, participant.midi_port_addr()).await {
                event!(
                    Level::WARN,
                    name = participant.name().to_str().unwrap_or("Unknown"),
                    addr = %participant.midi_port_addr(),
                    "Failed to send clock sync: {e}"
                );
            } else {
                event!(Level::DEBUG, name = participant.name().to_str().unwrap_or("Unknown"), "Sent clock sync");
            }
        }
    }

    #[instrument(skip_all, fields(count = packet.count, ssrc = packet.sender_ssrc.get(), src_name))]
    async fn handle_clock_sync(&self, packet: &ClockSyncPacket, ctx: &RtpMidiSession) {
        let mut part_lock = ctx.participants.lock().await;
        let maybe_participant = part_lock.get_mut(&packet.sender_ssrc);

        if maybe_participant.is_none() {
            event!(Level::WARN, "Received clock sync but no matching participant found");
            return;
        }
        let participant = maybe_participant.unwrap();
        tracing::Span::current().record("src_name", participant.name().to_str().unwrap_or("Unknown"));
        participant.received_clock_sync();
        event!(Level::DEBUG, "Updated clock sync for existing participant");
        let participant = participant.clone();
        drop(part_lock);

        match packet.count {
            0 | 1 => {
                self.send_clock_sync(iter::once(&participant), packet.timestamps, packet.count + 1).await;
            }
            2 => {
                let latency_estimate = (packet.timestamps[2].get() - packet.timestamps[0].get()) as f32 / 10.0;
                event!(Level::INFO, latency_estimate = std::format!("{latency_estimate}ms"), "Clock sync finalized");
            }
            _ => {
                event!(Level::ERROR, "Unexpected clock sync count");
            }
        }
    }

    #[instrument(skip_all, fields(name = %ctx.name(), participants))]
    pub async fn send_midi_batch<'a>(&self, ctx: &RtpMidiSession, commands: &'a [MidiEvent<'a>]) -> std::io::Result<()> {
        let lock = ctx.participants.lock().await;
        let participants: Vec<Participant> = lock.values().cloned().collect();
        let mut seq = self.sequence_number.lock().await;
        let packet = MidiPacket::new_as_bytes(U16::new(*seq), current_timestamp_u32(self.start_time), self.ssrc, commands, false);
        *seq = seq.wrapping_add(1);
        event!(Level::DEBUG, "Sending MIDI packet batch");
        for participant in participants {
            self.socket.send_to(&packet, participant.midi_port_addr()).await?;
        }
        Ok(())
    }

    #[instrument(skip_all, fields(name = %ctx.name()))]
    pub async fn send_midi<'a>(&self, ctx: &RtpMidiSession, command: &'a RtpMidiMessage<'a>) -> std::io::Result<()> {
        let batch: [MidiEvent; 1] = [MidiEvent::new(None, command.to_owned())];
        self.send_midi_batch(ctx, &batch).await
    }

    #[instrument(skip_all, fields(addr = %addr))]
    pub(super) async fn send_invitation(&self, invitation: &[u8], addr: SocketAddr) {
        event!(Level::DEBUG, "Sending session invitation");
        let result = self.socket.send_to(invitation, addr).await;
        if let Err(e) = result {
            event!(Level::WARN, "Failed to send session invitation: {e}");
        } else {
            event!(Level::INFO, "Sent session invitation");
        }
    }
}
