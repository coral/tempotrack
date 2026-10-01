use bytes::{BufMut, BytesMut};
use midi_types::{
    Channel, Control, MidiMessage, Note, Program, QuarterFrame, Value7, Value14,
    status::{self},
};

use crate::packets::midi_packets::{rtp_midi_message::RtpMidiMessage, util::StatusBit};
use std::io::Result;

pub(super) trait ReadWriteExt {
    fn write(&self, writer: &mut BytesMut, running_status: Option<u8>);
    fn status(&self) -> u8;
    fn from_status_byte(status_byte: u8, channel: u8, bytes: &[u8]) -> std::io::Result<(RtpMidiMessage<'_>, &[u8])>;
    fn from_be_bytes(bytes: &[u8], running_status: Option<u8>) -> std::io::Result<(RtpMidiMessage<'_>, &[u8])>;
}

impl ReadWriteExt for MidiMessage {
    fn write(&self, bytes: &mut BytesMut, running_status: Option<u8>) {
        if let Some(status_byte) = running_status {
            if status_byte != self.status() {
                bytes.put_u8(self.status());
            }
        } else {
            bytes.put_u8(self.status());
        }

        match self {
            // Timing Clock consists only of its status byte, already written above.
            MidiMessage::TimingClock => {}
            MidiMessage::NoteOn(_channel, key, velocity) | MidiMessage::NoteOff(_channel, key, velocity) => {
                bytes.put_u8(Into::into(*key));
                bytes.put_u8(Into::into(*velocity));
            }
            MidiMessage::ChannelPressure(_channel, pressure) => {
                bytes.put_u8(Into::into(*pressure));
            }
            MidiMessage::ControlChange(_channel, controller, value) => {
                bytes.put_u8(Into::into(*controller));
                bytes.put_u8(Into::into(*value));
            }
            MidiMessage::ProgramChange(_channel, program) => {
                bytes.put_u8(Into::into(*program));
            }
            MidiMessage::KeyPressure(_channel, key, pressure) => {
                bytes.put_u8(Into::into(*key));
                bytes.put_u8(Into::into(*pressure));
            }
            MidiMessage::PitchBendChange(_channel, value) => {
                let raw: u16 = Into::into(*value);
                bytes.put_u8((raw >> 7) as u8);
                bytes.put_u8((raw & 0x7F) as u8);
            }
            _ => {
                // Handle other MIDI messages or SysEx messages here
                // For now, we will panic if an unsupported message is encountered
                panic!("Unsupported MIDI message type: {self:?}");
            }
        }
    }

    fn status(&self) -> u8 {
        match self {
            MidiMessage::NoteOn(channel, _, _) => status::NOTE_ON | u8::from(*channel),
            MidiMessage::NoteOff(channel, _, _) => status::NOTE_OFF | u8::from(*channel),
            MidiMessage::ChannelPressure(channel, _) => status::CHANNEL_PRESSURE | u8::from(*channel),
            MidiMessage::ControlChange(channel, _, _) => status::CONTROL_CHANGE | u8::from(*channel),
            MidiMessage::ProgramChange(channel, _) => status::PROGRAM_CHANGE | u8::from(*channel),
            MidiMessage::KeyPressure(channel, _, _) => status::KEY_PRESSURE | u8::from(*channel),
            MidiMessage::PitchBendChange(channel, _) => status::PITCH_BEND_CHANGE | u8::from(*channel),
            MidiMessage::QuarterFrame(_data) => status::QUARTER_FRAME,
            MidiMessage::SongPositionPointer(_song_position) => status::SONG_POSITION_POINTER,
            MidiMessage::SongSelect(_song_number) => status::SONG_SELECT,
            MidiMessage::TuneRequest => status::TUNE_REQUEST,
            MidiMessage::TimingClock => status::TIMING_CLOCK,
            MidiMessage::Start => status::START,
            MidiMessage::Continue => status::CONTINUE,
            MidiMessage::Stop => status::STOP,
            MidiMessage::ActiveSensing => status::ACTIVE_SENSING,
            MidiMessage::Reset => status::RESET,
        }
    }

    fn from_status_byte(status_byte: u8, channel: u8, bytes: &[u8]) -> Result<(RtpMidiMessage<'_>, &[u8])> {
        let command = match status_byte {
            0x80..0x90 => RtpMidiMessage::MidiMessage(MidiMessage::NoteOff(Channel::from(channel), Note::from(bytes[0]), Value7::from(bytes[1]))),
            0x90..0xA0 => RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(Channel::from(channel), Note::from(bytes[0]), Value7::from(bytes[1]))),
            0xA0..0xB0 => RtpMidiMessage::MidiMessage(MidiMessage::KeyPressure(Channel::from(channel), Note::from(bytes[0]), Value7::from(bytes[1]))),
            0xB0..0xC0 => RtpMidiMessage::MidiMessage(MidiMessage::ControlChange(
                Channel::from(channel),
                Control::from(bytes[0]),
                Value7::from(bytes[1]),
            )),
            0xC0..0xD0 => RtpMidiMessage::MidiMessage(MidiMessage::ProgramChange(Channel::from(channel), Program::from(bytes[0]))),
            0xD0..0xE0 => RtpMidiMessage::MidiMessage(MidiMessage::ChannelPressure(Channel::from(channel), Value7::from(bytes[0]))),
            0xE0..0xF0 => RtpMidiMessage::MidiMessage(MidiMessage::PitchBendChange(Channel::from(channel), Value14::from((bytes[0], bytes[1])))),
            0xF0 => {
                let end_index = bytes.iter().position(|&b| b == 0xF7).unwrap_or(bytes.len());
                RtpMidiMessage::SysEx(&bytes[1..end_index])
            }
            0xF1 => RtpMidiMessage::MidiMessage(MidiMessage::QuarterFrame(QuarterFrame::from(bytes[0]))),
            0xF2 => RtpMidiMessage::MidiMessage(MidiMessage::SongPositionPointer(Value14::from((bytes[0], bytes[1])))),
            0xF3 => RtpMidiMessage::MidiMessage(MidiMessage::SongSelect(Value7::from(bytes[0]))),
            0xF6 => RtpMidiMessage::MidiMessage(MidiMessage::TuneRequest),
            0xF8 => RtpMidiMessage::MidiMessage(MidiMessage::TimingClock),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unsupported MIDI status byte: {status_byte:#02X}"),
                ));
            }
        };

        let remaining = &bytes[command.len() - 1..];
        Ok((command, remaining))
    }

    fn from_be_bytes(bytes: &[u8], running_status: Option<u8>) -> std::io::Result<(RtpMidiMessage<'_>, &[u8])> {
        let (status_byte, bytes) = if bytes[0].status_bit() {
            (bytes[0], &bytes[1..])
        } else {
            (
                running_status.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Running status not set"))?,
                bytes,
            )
        };
        let channel = status_byte & 0x0F;
        Self::from_status_byte(status_byte, channel, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReadWriteExt;

    #[test]
    fn test_midi_command() {
        let command = MidiMessage::NoteOn(Channel::C7, Note::C4, Value7::from(0x7F));
        if let MidiMessage::NoteOn(channel, key, velocity) = command {
            assert_eq!(channel, Channel::C7);
            assert_eq!(key, Note::C4);
            assert_eq!(velocity, Value7::from(0x7F));
        } else {
            panic!("Not a NoteOn command");
        }
    }

    #[test]
    fn test_midi_command_write() {
        let command = MidiMessage::NoteOn(Channel::C5, Note::E3, Value7::from(0x7F));
        let mut bytes = BytesMut::new();
        command.write(&mut bytes, None);
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[..3], [0x94, 0x40, 0x7F]);
    }

    fn test_command_write_type(command: MidiMessage, expected_bytes: &[u8]) {
        let mut bytes = BytesMut::new();
        command.write(&mut bytes, None);
        assert_eq!(bytes.len(), expected_bytes.len());
        assert_eq!(bytes, expected_bytes);
    }

    #[test]
    fn test_command_write_note_off() {
        let command = MidiMessage::NoteOff(From::from(4), From::from(0x40), From::from(0x7F));
        let expected_bytes: Vec<u8> = vec![0x84u8, 0x40, 0x7F];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_note_on() {
        let command = MidiMessage::NoteOn(From::from(4), From::from(0x40), From::from(0x7F));
        let expected_bytes: Vec<u8> = vec![0x94u8, 0x40, 0x7F];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_polyphonic_key_pressure() {
        let command = MidiMessage::KeyPressure(From::from(4), From::from(0x40), From::from(0x7F));
        let expected_bytes: Vec<u8> = vec![0xA4u8, 0x40, 0x7F];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_control_change() {
        let command = MidiMessage::ControlChange(From::from(4), From::from(0x40), From::from(0x7F));
        let expected_bytes: Vec<u8> = vec![0xB4u8, 0x40, 0x7F];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_program_change() {
        let command = MidiMessage::ProgramChange(From::from(4), From::from(0x40));
        let expected_bytes: Vec<u8> = vec![0xC4u8, 0x40];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_channel_pressure() {
        let command = MidiMessage::ChannelPressure(From::from(4), From::from(0x40));
        let expected_bytes: Vec<u8> = vec![0xD4u8, 0x40];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_pitch_bend() {
        let command = MidiMessage::PitchBendChange(From::from(4), From::from((0x40, 0x7F)));
        let expected_bytes: Vec<u8> = vec![0xE4u8, 0x40, 0x7F];
        test_command_write_type(command, &expected_bytes);
    }

    #[test]
    fn test_command_write_invalid() {
        let command = MidiMessage::NoteOn(From::from(4), From::from(0x40), From::from(0x7F));
        let mut bytes = BytesMut::new();
        command.write(&mut bytes, None);
        assert_eq!(&bytes[..], &[0x94u8, 0x40, 0x7F]);
    }
}
