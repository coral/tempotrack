use bytes::BufMut;
use midi_types::MidiMessage;

use crate::packets::midi_packets::midi_message_ext::ReadWriteExt;

#[derive(Debug, Clone, PartialEq)]
pub enum RtpMidiMessage<'a> {
    MidiMessage(MidiMessage),
    SysEx(&'a [u8]),
}

impl From<MidiMessage> for RtpMidiMessage<'_> {
    fn from(msg: MidiMessage) -> Self {
        RtpMidiMessage::MidiMessage(msg)
    }
}

impl RtpMidiMessage<'_> {
    pub fn len(&self) -> usize {
        match self {
            RtpMidiMessage::MidiMessage(msg) => msg.len(),
            RtpMidiMessage::SysEx(data) => data.len() + 2, // +1 for the SysEx start byte
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn write(&self, bytes: &mut bytes::BytesMut, running_status: Option<u8>) {
        match self {
            RtpMidiMessage::MidiMessage(msg) => msg.write(bytes, running_status),
            RtpMidiMessage::SysEx(data) => {
                bytes.put_u8(0xF0); // SysEx start byte
                bytes.extend_from_slice(data);
                bytes.put_u8(0xF7); // SysEx end byte
            }
        }
    }

    pub(crate) fn status(&self) -> u8 {
        match self {
            RtpMidiMessage::MidiMessage(msg) => msg.status(),
            RtpMidiMessage::SysEx(_) => 0xF0, // SysEx messages have a special status byte
        }
    }
}
