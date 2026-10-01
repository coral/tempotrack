use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, network_endian::U32};

#[derive(Debug, KnownLayout, IntoBytes, Immutable, FromBytes)]
#[repr(C)]
pub struct SessionInitiationPacketBody {
    pub protocol_version: U32,
    pub initiator_token: U32,
    pub sender_ssrc: U32,
}

impl SessionInitiationPacketBody {
    pub const SIZE: usize = 12;

    pub fn new(initiator_token: U32, sender_ssrc: U32) -> SessionInitiationPacketBody {
        SessionInitiationPacketBody {
            protocol_version: U32::new(2),
            initiator_token,
            sender_ssrc,
        }
    }
}

#[cfg(test)]
mod tests {
    use zerocopy::{FromBytes, IntoBytes, network_endian::U32};

    use crate::packets::control_packets::session_initiation_packet::SessionInitiationPacketBody;

    fn get_test_body() -> [u8; 12] {
        [
            0x00, 0x00, 0x00, 0x02, //version
            0xF8, 0xD1, 0x80, 0xE6, //initiator token
            0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
        ]
    }

    #[test]
    fn test_read_body() {
        let body = get_test_body();
        let result = SessionInitiationPacketBody::ref_from_bytes(&body);
        match result {
            Ok(body) => {
                assert_eq!(body.protocol_version, 2);
                assert_eq!(body.initiator_token, 0xF8D180E6);
                assert_eq!(body.sender_ssrc, 0xF519AEB9);
            }
            Err(e) => panic!("Failed to read body: {e}"),
        }
    }

    #[test]
    fn test_write() {
        let initiator_token = U32::new(0xF8D180E6);
        let sender_ssrc = U32::new(0xF519AEB9);

        let packet = SessionInitiationPacketBody::new(initiator_token, sender_ssrc);
        let bytes = packet.as_bytes();

        assert_eq!(bytes.len(), SessionInitiationPacketBody::SIZE);
        assert_eq!(&bytes[0..12], &get_test_body()[0..12]);
    }
}
