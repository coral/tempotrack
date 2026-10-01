use zerocopy::FromBytes;

use super::{control_packets::control_packet::ControlPacket, midi_packets::midi_packet::MidiPacket};

#[derive(Debug)]
pub(crate) enum RtpMidiPacket<'a> {
    Midi(&'a MidiPacket),
    Control(ControlPacket<'a>),
}

impl<'a> RtpMidiPacket<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, std::io::Error> {
        if ControlPacket::is_control_packet(bytes) {
            let packet =
                ControlPacket::try_from_bytes(bytes).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to parse Control packet"))?;
            Ok(RtpMidiPacket::Control(packet))
        } else {
            let (packet, _remaining) =
                MidiPacket::ref_from_prefix(bytes).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to parse MIDI packet"))?;
            Ok(RtpMidiPacket::Midi(packet))
        }
    }
}

#[cfg(test)]
mod tests {
    use midi_types::{Channel, MidiMessage, Note, Value7};
    use zerocopy::U16;
    use zerocopy::network_endian::U32;

    use super::*;
    use crate::packets::midi_packets::midi_event::MidiEvent;
    use crate::packets::midi_packets::rtp_midi_message::RtpMidiMessage;

    #[test]
    fn test_parse_midi_packet() {
        let commands = vec![MidiEvent::new(
            None,
            RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(Channel::C1, Note::C4, Value7::from(127))),
        )];
        let packet = MidiPacket::new_as_bytes(U16::new(1), U32::new(2), U32::new(3), &commands, false);

        let parsed_packet = RtpMidiPacket::parse(&packet).unwrap();
        if let RtpMidiPacket::Midi(parsed_midi_packet) = parsed_packet {
            assert_eq!(parsed_midi_packet.sequence_number(), 1);
            assert_eq!(parsed_midi_packet.timestamp(), 2);
            assert_eq!(parsed_midi_packet.ssrc(), 3);
            let values = parsed_midi_packet.commands().collect::<Vec<_>>();
            assert_eq!(values.len(), 1);
            assert_eq!(
                values[0].command().to_owned(),
                RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(Channel::C1, Note::C4, Value7::from(127)))
            );
        } else {
            panic!("Expected MidiPacket");
        }
    }

    // #[test]
    // fn test_parse_control_packet() {
    //     let packet = ControlPacket::new_acceptance(U32::new(1), U32::new(1), c"Test Name");
    //     let parsed = RtpMidiPacket::parse(&packet).unwrap();

    //     match parsed {
    //         RtpMidiPacket::Control(ControlPacket::Acceptance { body: _, name: _ }) => {
    //             // all good
    //         }
    //         _ => panic!("Expected ControlPacket"),
    //     }
    // }
}
