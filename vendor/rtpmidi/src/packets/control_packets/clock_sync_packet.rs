use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    network_endian::{U32, U64},
};

#[derive(Debug, KnownLayout, IntoBytes, Immutable, FromBytes)]
#[repr(C, packed)]
pub struct ClockSyncPacket {
    pub sender_ssrc: U32,
    pub count: u8,
    _reserved: [u8; 3], // Reserved bytes
    pub timestamps: [U64; 3],
}

impl ClockSyncPacket {
    pub fn new(count: u8, timestamps: [U64; 3], sender_ssrc: U32) -> Self {
        ClockSyncPacket {
            sender_ssrc,
            count,
            _reserved: [0; 3],
            timestamps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_control_packet_0() {
        let buffer = [
            0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
            0x00, //count
            0x00, 0x00, 0x00, //reserved
            0x00, 0x00, 0x00, 0x00, 0x72, 0xD4, 0xC5, 0x8E, // timestamp 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // timestamp 2
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // timestamp 3
        ]; // Example buffer for a ClockSync packet

        let result = ClockSyncPacket::ref_from_bytes(&buffer);
        match result {
            Ok(packet) => {
                assert_eq!(packet.sender_ssrc, 4112101049);
                assert_eq!(packet.count, 0);
                assert_eq!(packet.timestamps[0], 1926546830);
            }
            Err(e) => panic!("Failed to read ClockSync packet: {e}"),
        };
    }

    #[test]
    fn test_read_control_packet_2() {
        let buffer = [
            0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
            0x02, //count
            0x00, 0x00, 0x00, //reserved
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x72, // timestamp 1
            0x00, 0x00, 0x00, 0x00, 0x04, 0x3D, 0xC7, 0xDF, // timestamp 2
            0x00, 0x00, 0x00, 0x00, 0x72, 0xD4, 0xC5, 0xCD, // timestamp 3
        ];

        let result = ClockSyncPacket::ref_from_bytes(&buffer);
        match result {
            Ok(packet) => {
                assert_eq!(packet.count, 2);
                assert_eq!(packet.sender_ssrc, 4112101049);
                assert_eq!(packet.timestamps[0], 114);
                assert_eq!(packet.timestamps[1], 71157727);
                assert_eq!(packet.timestamps[2], 1926546893);
            }
            Err(e) => panic!("Failed to read ClockSync packet: {e}"),
        };
    }

    // #[test]
    // fn test_write_control_packet() {
    //     let expected = [
    //         0xFF, 0xFF, 0x43, 0x4B, //header
    //         0xF5, 0x19, 0xAE, 0xB9, //sender ssrc
    //         0x02, //count
    //         0x00, 0x00, 0x00, //reserved
    //         0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // timestamp 1
    //         0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, // timestamp 2
    //         0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03,
    //     ];
    //     let packet = ClockSyncPacket::new_as_bytes(2, [U64::new(1), U64::new(2), U64::new(3)], U32::new(4112101049));

    //     assert_eq!(packet.len(), ClockSyncPacket::SIZE);
    //     assert_eq!(packet.as_bytes(), expected);
    // }

    // #[test]
    // fn test_new() {
    //     let packet_bytes = ClockSyncPacket::new_as_bytes(2, [U64::new(1), U64::new(2), U64::new(3)], U32::new(4112101049));
    //     let packet = ClockSyncPacket::ref_from_bytes(packet_bytes[4..].as_ref()).unwrap();

    //     assert_eq!(packet.count, 2);
    //     assert_eq!(packet.sender_ssrc, U32::new(4112101049));
    //     assert_eq!(packet.timestamps[0], U64::new(1));
    //     assert_eq!(packet.timestamps[1], U64::new(2));
    //     assert_eq!(packet.timestamps[2], U64::new(3));
    // }
}
