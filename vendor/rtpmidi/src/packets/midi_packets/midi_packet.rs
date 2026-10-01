use bytes::{BufMut, Bytes, BytesMut};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    network_endian::{U16, U32},
};

use super::midi_command_iterator::MidiCommandIterator;
use super::midi_command_list_body::MidiEventList;
use crate::packets::midi_packets::{midi_command_list_header::MidiCommandListHeader, midi_event::MidiEvent, midi_packet_header::MidiPacketHeader};

#[derive(FromBytes, KnownLayout, Immutable, Debug)]
#[repr(C)]
pub(crate) struct MidiPacket {
    header: MidiPacketHeader,
    body: [u8],
}

impl MidiPacket {
    pub(crate) fn new_as_bytes<'a>(sequence_number: U16, timestamp: U32, ssrc: U32, commands: &'a [MidiEvent<'a>], z_flag: bool) -> Bytes {
        let packet_header = MidiPacketHeader::new(sequence_number, timestamp, ssrc);
        let command_list_header = MidiCommandListHeader::build_for(commands, z_flag);

        // Get the size of the body from the header as it's already calculated
        let mut buffer = BytesMut::with_capacity(std::mem::size_of::<MidiPacketHeader>() + command_list_header.size() + command_list_header.length());
        buffer.put_slice(packet_header.as_bytes());
        command_list_header.write(&mut buffer);
        commands.write(&mut buffer, z_flag);
        buffer.freeze()
    }

    pub fn commands(&self) -> MidiCommandIterator<'_> {
        MidiCommandIterator::new(&self.body)
    }

    pub fn sequence_number(&self) -> U16 {
        self.header.sequence_number
    }

    #[allow(dead_code)]
    pub fn timestamp(&self) -> U32 {
        self.header.timestamp
    }

    #[allow(dead_code)]
    pub fn ssrc(&self) -> U32 {
        self.header.ssrc
    }
}

#[cfg(test)]
mod tests {
    use midi_types::{Channel, MidiMessage, Note, Value7};

    use crate::packets::midi_packets::rtp_midi_message::RtpMidiMessage;

    use super::*;

    #[test]
    fn test_midi_packet_creation() {
        let sequence_number = U16::from(1);
        let timestamp = U32::from(2);
        let ssrc = U32::from(3);
        let commands = vec![
            MidiEvent::new(None, RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(Channel::C1, Note::C4, Value7::from(127)))),
            MidiEvent::new(None, RtpMidiMessage::MidiMessage(MidiMessage::NoteOff(Channel::C1, Note::C4, Value7::from(0)))),
        ];
        let z_flag = false;

        let packet = MidiPacket::new_as_bytes(sequence_number, timestamp, ssrc, &commands, z_flag);

        let expected = [
            0x80, 0x61, // flags
            0x00, 0x01, // sequence number
            0x00, 0x00, 0x00, 0x02, // timestamp
            0x00, 0x00, 0x00, 0x03, // ssrc
            0x07, // command list flags and length
            0x90, 0x48, 0x7F, // command list header and commands would follow here
            0x00, // delta time
            0x80, 0x48, 0x00, // Note On command for C4
        ];

        assert_eq!(packet.len(), expected.len());
        assert_eq!(&packet[..], &expected);
    }
}
