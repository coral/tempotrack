use super::invite_responder::InviteResponder;
use super::rtp_midi_session::RtpMidiSession;
use super::rtp_port::RtpPort;
use crate::packets::control_packets::control_packet::ControlPacket;
use crate::packets::control_packets::session_initiation_packet::SessionInitiationPacketBody;
use crate::participant::Participant;
use crate::sessions::rtp_midi_session::PendingInvitation;
use std::ffi::CStr;
use std::ffi::CString;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tracing::Level;
use tracing::event;
use tracing::instrument;
use zerocopy::network_endian::U32;

pub const MAX_CONTROL_PACKET_SIZE: usize = 1024;

pub(super) struct ControlPort {
    ssrc: U32,
    session_name: CString,
    socket: Arc<UdpSocket>,
}

impl RtpPort for ControlPort {
    fn session_name(&self) -> &CStr {
        &self.session_name
    }

    fn ssrc(&self) -> U32 {
        self.ssrc
    }

    fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }

    fn participant_addr(participant: &Participant) -> SocketAddr {
        participant.addr()
    }
}

impl ControlPort {
    pub async fn bind(port: u16, name: CString, ssrc: U32) -> std::io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?);

        Ok(ControlPort {
            session_name: name,
            ssrc,
            socket,
        })
    }

    #[instrument(skip_all, fields(name = %ctx.name(), addr = %addr))]
    pub async fn invite_participant(&self, ctx: &RtpMidiSession, addr: SocketAddr) {
        let initiator_token = U32::new(rand::random::<u32>());
        let invitation = ControlPacket::new_invitation_as_bytes(initiator_token, self.ssrc, &self.session_name);
        let result = self.socket.send_to(&invitation, addr).await;
        if let Err(e) = result {
            event!(Level::ERROR, "Failed to send session invitation: {}", e);
            return;
        }
        event!(Level::INFO, "Sent session invitation");
        ctx.pending_invitations.lock().await.insert(
            U32::new(0),
            PendingInvitation {
                addr,
                token: initiator_token,
                name: CString::new("Test Name").unwrap(),
            },
        );
    }

    #[instrument(skip_all, name = "CTRL", fields(name = %self.session_name.to_string_lossy(), src))]
    pub async fn start(&self, ctx: &RtpMidiSession, invite_handler: &InviteResponder, buf: &mut [u8; MAX_CONTROL_PACKET_SIZE]) {
        let recv = self.socket.recv_from(buf).await;

        if let Err(e) = recv {
            event!(Level::ERROR, "Failed to receive data on control port: {}", e);
            return;
        }

        let (amt, src) = recv.unwrap();
        tracing::Span::current().record("src", src.to_string());
        event!(Level::TRACE, "Received {} bytes", amt);

        let maybe_ctrl_packet = ControlPacket::try_from_bytes(&buf[..amt]);
        if let Err(e) = maybe_ctrl_packet {
            event!(Level::WARN, "Failed to parse control packet: {}", e);
            return;
        }

        let packet = maybe_ctrl_packet.unwrap();
        event!(Level::TRACE, packet = std::format!("{:?}", packet), "Parsed packet");

        match packet {
            ControlPacket::Invitation { body, name } => {
                self.handle_invitation(body, name, invite_handler, ctx, src).await;
            }
            ControlPacket::Acceptance { body, name } => {
                self.handle_acceptance(body, name, ctx, src).await;
            }
            ControlPacket::Rejection(body) => {
                self.handle_rejection(body, ctx, src).await;
            }
            ControlPacket::Termination(body) => {
                self.handle_termination(body.sender_ssrc, src, &ctx.participants).await;
            }
            _ => {
                event!(Level::WARN, packet = std::format!("{:?}", packet), "Control: Unhandled control packet");
            }
        }
    }

    #[instrument(skip_all)]
    async fn handle_invitation(
        &self,
        invitation: &SessionInitiationPacketBody,
        inviter_name: &CStr,
        invite_handler: &InviteResponder,
        ctx: &RtpMidiSession,
        src: SocketAddr,
    ) {
        event!(Level::INFO, token = invitation.initiator_token.get(), "Received session invitation");
        let accept = invite_handler.handle(invitation, inviter_name, &src);
        if accept {
            event!(Level::INFO, "Accepted session invitation");
            ctx.pending_invitations.lock().await.insert(
                invitation.sender_ssrc,
                PendingInvitation {
                    addr: src,
                    token: invitation.initiator_token,
                    name: inviter_name.to_owned(),
                },
            );
            self.send_invitation_acceptance(invitation.initiator_token, src).await;
        } else {
            event!(Level::INFO, "Rejected session initiation");
            let rejection_packet = ControlPacket::new_rejection_as_bytes(invitation.initiator_token, self.ssrc);
            let result = self.socket.send_to(&rejection_packet, src).await;
            if let Err(e) = result {
                event!(Level::ERROR, "Failed to send session rejection: {}", e);
            } else {
                event!(Level::DEBUG, "Sent session rejection");
            }
        }
    }

    #[instrument(skip_all, fields(token = rejection.initiator_token.get()))]
    async fn handle_rejection(&self, rejection: &SessionInitiationPacketBody, ctx: &RtpMidiSession, src: SocketAddr) {
        event!(Level::INFO, "Received session rejection");
        let _ = self.remove_invitation(rejection, ctx, src).await;
    }

    #[instrument(skip_all)]
    async fn remove_invitation(&self, invitation_response: &SessionInitiationPacketBody, ctx: &RtpMidiSession, src: SocketAddr) -> Option<PendingInvitation> {
        event!(Level::DEBUG, "Removing invitation for SSRC {} at {}", invitation_response.sender_ssrc, src);
        let mut locked_pending_invitations = ctx.pending_invitations.lock().await;
        if locked_pending_invitations.contains_key(&invitation_response.sender_ssrc) {
            locked_pending_invitations.remove(&invitation_response.sender_ssrc)
        } else if !locked_pending_invitations.contains_key(&invitation_response.sender_ssrc)
            && locked_pending_invitations.contains_key(&U32::ZERO)
            && locked_pending_invitations[&U32::ZERO].token == invitation_response.initiator_token
            && locked_pending_invitations[&U32::ZERO].addr == src
        {
            locked_pending_invitations.remove(&U32::ZERO)
        } else {
            None
        }
    }

    #[instrument(skip_all, fields(ssrc = ack_body.sender_ssrc.get(), src = %src))]
    async fn handle_acceptance(&self, ack_body: &SessionInitiationPacketBody, name: &CStr, ctx: &RtpMidiSession, src: SocketAddr) {
        event!(Level::INFO, "Received session acknowledgment");
        let inv = self.remove_invitation(ack_body, ctx, src).await;
        if inv.is_none() {
            event!(Level::WARN, "Received Acknowledgment but no matching invitation found");
            return;
        }

        let inv = inv.unwrap();
        if inv.token != ack_body.initiator_token.get() {
            event!(
                Level::WARN,
                "Received Acknowledgment from {} with mismatched token. Expected {}, got {}.",
                inv.addr,
                inv.token,
                ack_body.initiator_token.get()
            );
            return;
        }

        event!(
            Level::DEBUG,
            "Matched Acknowledgment from {} invitation. Sending MIDI port invitation.",
            inv.addr
        );

        let midi_addr = SocketAddr::new(inv.addr.ip(), inv.addr.port() + 1);

        // Generate a new token specifically for the MIDI port invitation
        let midi_token = U32::new(rand::random::<u32>());

        let mut lock = ctx.pending_invitations.lock().await;
        lock.insert(
            ack_body.sender_ssrc,
            PendingInvitation {
                addr: midi_addr,
                token: midi_token,
                name: name.to_owned(),
            },
        );

        let response_packet = ControlPacket::new_invitation_as_bytes(midi_token, self.ssrc, self.session_name.as_ref());
        ctx.midi_port.send_invitation(&response_packet, midi_addr).await;
    }
}
