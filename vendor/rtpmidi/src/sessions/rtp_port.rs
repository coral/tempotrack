use std::{collections::HashMap, ffi::CStr, net::SocketAddr, sync::Arc};

use tokio::{net::UdpSocket, sync::Mutex};
use tracing::{Level, event, instrument};
use zerocopy::network_endian::U32;

use crate::{packets::control_packets::control_packet::ControlPacket, participant::Participant};

pub(super) trait RtpPort {
    fn session_name(&self) -> &CStr;
    fn ssrc(&self) -> U32;
    fn socket(&self) -> &Arc<UdpSocket>;
    fn participant_addr(participant: &Participant) -> SocketAddr;

    #[instrument(skip_all, fields(destination = %destination))]
    async fn send_invitation_acceptance<'a>(&self, initiator_token: U32, destination: SocketAddr) {
        let response_packet = ControlPacket::new_acceptance_as_bytes(initiator_token, self.ssrc(), self.session_name());

        if let Err(e) = self.socket().send_to(&response_packet, destination).await {
            event!(Level::ERROR, "Failed to send invitation response: {}", e);
        } else {
            event!(Level::INFO, "Sent invitation acceptance");
        }
    }

    #[instrument(skip_all, fields(ssrc = ssrc.get(), src = %src))]
    async fn handle_termination(&self, ssrc: U32, src: SocketAddr, participants: &Arc<Mutex<HashMap<U32, Participant>>>) {
        event!(Level::INFO, "Received termination packet");
        let mut lock = participants.lock().await;
        lock.remove(&ssrc);
    }

    #[instrument(skip_all, fields(destination = %participant.addr(), participant = participant.name().to_str().unwrap_or("Unknown")))]
    async fn send_termination_packet(&self, participant: &Participant) {
        let termination_packet = ControlPacket::new_termination_as_bytes(participant.initiator_token().unwrap(), self.ssrc());
        let addr = Self::participant_addr(participant);
        if let Err(e) = self.socket().send_to(&termination_packet, addr).await {
            event!(Level::WARN, "Failed to send termination packet: {}", e);
        } else {
            event!(Level::INFO, "Sent termination packet");
        }
    }
}
