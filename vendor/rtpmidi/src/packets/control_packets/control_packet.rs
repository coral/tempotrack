use std::ffi::CStr;

use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use zerocopy::{
    FromBytes, IntoBytes,
    network_endian::{U32, U64},
};

use crate::packets::{control_packets::session_initiation_packet::SessionInitiationPacketBody, error::PacketParseError};

use super::clock_sync_packet::ClockSyncPacket;

const CONTROL_PACKET_MARKER_VALUE: [u8; 2] = [255, 255];

#[derive(Debug)]
pub enum ControlPacket<'a> {
    ClockSync(&'a ClockSyncPacket),
    Invitation { body: &'a SessionInitiationPacketBody, name: &'a CStr },
    Acceptance { body: &'a SessionInitiationPacketBody, name: &'a CStr },
    Rejection(&'a SessionInitiationPacketBody),
    Termination(&'a SessionInitiationPacketBody),
}

impl<'a> ControlPacket<'a> {
    pub fn is_control_packet(buffer: &[u8]) -> bool {
        buffer.starts_with(&CONTROL_PACKET_MARKER_VALUE)
    }

    pub fn try_from_bytes(buffer: &'a [u8]) -> Result<Self> {
        if buffer.len() < 4 {
            return Err(anyhow::Error::new(PacketParseError::NotEnoughData));
        }

        // Validate marker (2 bytes)
        if !buffer.starts_with(&CONTROL_PACKET_MARKER_VALUE) {
            return Err(anyhow::Error::new(PacketParseError::InvalidData));
        }

        // Parse command type (2 bytes)
        let command = &buffer[2..4];
        let remaining = &buffer[4..];

        // Parse body based on command type
        let result = match command {
            b"CK" => {
                let clock_sync = ClockSyncPacket::ref_from_bytes(remaining)
                    .map_err(|_| PacketParseError::InvalidData)
                    .context("Failed to parse Clock Sync Packet")?;
                ControlPacket::ClockSync(clock_sync)
            }
            b"IN" => {
                let (session_body, name_bytes) = SessionInitiationPacketBody::ref_from_prefix(remaining)
                    .map_err(|_| PacketParseError::InvalidData)
                    .context("Failed to parse Session Invitation Packet")?;
                let name = CStr::from_bytes_with_nul(name_bytes).context("Failed to parse Session name from Session Invitation Packet")?;
                ControlPacket::Invitation { body: session_body, name }
            }
            b"OK" => {
                let (session_body, name_bytes) = SessionInitiationPacketBody::ref_from_prefix(remaining)
                    .map_err(|_| PacketParseError::InvalidData)
                    .context("Failed to parse Session Acceptance Packet")?;
                let name = CStr::from_bytes_with_nul(name_bytes).context("Failed to parse Session name from Session Acceptance Packet")?;
                ControlPacket::Acceptance { body: session_body, name }
            }
            b"NO" => {
                let session_body = SessionInitiationPacketBody::ref_from_bytes(remaining)
                    .map_err(|_| PacketParseError::InvalidData)
                    .context("Failed to parse Session Rejection Packet")?;
                ControlPacket::Rejection(session_body)
            }
            b"BY" => {
                let session_body = SessionInitiationPacketBody::ref_from_bytes(remaining)
                    .map_err(|_| PacketParseError::InvalidData)
                    .context("Failed to parse Session Termination Packet")?;
                ControlPacket::Termination(session_body)
            }
            _ => return Err(anyhow::Error::new(PacketParseError::InvalidData).context(format!("Unknown control packet command: {command:?}"))),
        };
        Ok(result)
    }

    pub fn new_invitation_as_bytes(initiator_token: U32, ssrc: U32, name: &CStr) -> Bytes {
        let body = SessionInitiationPacketBody::new(initiator_token, ssrc);
        let name_bytes = name.to_bytes_with_nul();
        let header = CONTROL_PACKET_MARKER_VALUE;
        let command = b"IN";

        let mut packet = BytesMut::with_capacity(header.len() + command.len() + body.as_bytes().len() + name_bytes.len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(command);
        packet.extend_from_slice(body.as_bytes());
        packet.extend_from_slice(name_bytes);
        packet.freeze()
    }

    pub fn new_acceptance_as_bytes(initiator_token: U32, ssrc: U32, name: &CStr) -> Bytes {
        let body = SessionInitiationPacketBody::new(initiator_token, ssrc);
        let name_bytes = name.to_bytes_with_nul();
        let header = CONTROL_PACKET_MARKER_VALUE;
        let command = b"OK";

        let mut packet = BytesMut::with_capacity(header.len() + command.len() + body.as_bytes().len() + name_bytes.len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(command);
        packet.extend_from_slice(body.as_bytes());
        packet.extend_from_slice(name_bytes);
        packet.freeze()
    }

    pub fn new_rejection_as_bytes(initiator_token: U32, ssrc: U32) -> Bytes {
        let body = SessionInitiationPacketBody::new(initiator_token, ssrc);
        let header = CONTROL_PACKET_MARKER_VALUE;
        let command = b"NO";

        let mut packet = BytesMut::with_capacity(header.len() + command.len() + body.as_bytes().len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(command);
        packet.extend_from_slice(body.as_bytes());
        packet.freeze()
    }

    pub fn new_termination_as_bytes(initiator_token: U32, ssrc: U32) -> Bytes {
        let body = SessionInitiationPacketBody::new(initiator_token, ssrc);
        let header = CONTROL_PACKET_MARKER_VALUE;
        let command = b"BY";

        let mut packet = BytesMut::with_capacity(header.len() + command.len() + body.as_bytes().len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(command);
        packet.extend_from_slice(body.as_bytes());
        packet.freeze()
    }

    pub fn new_clock_sync_as_bytes(count: u8, timestamps: [U64; 3], sender_ssrc: U32) -> Bytes {
        let clock_sync_packet = ClockSyncPacket::new(count, timestamps, sender_ssrc);
        let packet_bytes = clock_sync_packet.as_bytes();
        let header = CONTROL_PACKET_MARKER_VALUE;
        let command = b"CK";

        let mut packet = BytesMut::with_capacity(header.len() + command.len() + packet_bytes.len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(command);
        packet.extend_from_slice(packet_bytes);
        packet.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_control_packet() {
        let data = vec![0, 0, 0, 0];
        let result = ControlPacket::try_from_bytes(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_too_short_control_packet() {
        let data = vec![255, 255, 67];
        let result = ControlPacket::try_from_bytes(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_control_packet() {
        let valid_packet = vec![255, 255, 67, 75];
        let invalid_packet = vec![0, 0, 0, 0];
        assert!(ControlPacket::is_control_packet(&valid_packet));
        assert!(!ControlPacket::is_control_packet(&invalid_packet));
    }

    #[test]
    fn test_parse_unknown_control_packet() {
        let data = vec![255, 255, 0, 0];
        let result = ControlPacket::try_from_bytes(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_clock_sync_packet_2() {
        let buffer = [
            0xFF, 0xFF, b'C', b'K', //header
            0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
            0x02, //count
            0x00, 0x00, 0x00, //reserved
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // timestamp 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, // timestamp 2
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, // timestamp 3
        ]; // Example buffer for a ClockSync packet

        let result = ControlPacket::try_from_bytes(&buffer);
        if let Err(e) = result {
            panic!("Failed to parse control packet: {e}");
        }
        assert!(result.is_ok());
        if let ControlPacket::ClockSync(packet) = &result.unwrap() {
            assert_eq!(packet.count, 2);
            assert_eq!(packet.sender_ssrc, 4112101049);
            assert_eq!(packet.timestamps[0], 1);
            assert_eq!(packet.timestamps[1], 2);
            assert_eq!(packet.timestamps[2], 3);
        } else {
            panic!("Expected ClockSync packet");
        }
    }

    #[test]
    fn test_read_session_initiation_packet() {
        let buffer = [
            0xFF, 0xFF, b'I', b'N', //header
            0x00, 0x00, 0x00, 0x02, //version
            0xF8, 0xD1, 0x80, 0xE6, //initiator token
            0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
            0x4C, 0x6F, 0x76, 0x65, 0x6C, 0x79, 0x20, 0x53, 0x65, 0x73, 0x73, 0x69, 0x6F, 0x6E, 0x00, //name
        ];

        let result = ControlPacket::try_from_bytes(&buffer);
        if let Err(e) = result {
            panic!("Failed to parse control packet: {e}");
        }

        assert!(result.is_ok());
        if let ControlPacket::Invitation { body: _body, name } = &result.unwrap() {
            assert_eq!(name.to_bytes(), b"Lovely Session");
        } else {
            panic!("Expected Invitation packet");
        }
    }
}
