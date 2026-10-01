use crate::packets::midi_packets::midi_event::MidiEvent;

use super::midi_command_list_header::MidiCommandListHeader;

#[derive(Debug)]
pub(crate) struct MidiCommandIterator<'a> {
    data: &'a [u8],
    running_status: Option<u8>,
    read_delta_time: bool,
}

impl<'a> MidiCommandIterator<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let command_list_header = MidiCommandListHeader::from_slice(data);
        let read_delta_time = command_list_header.flags().z_flag();
        let offset = command_list_header.size();
        let length = command_list_header.length();
        let slice = &data[offset..length + offset];
        MidiCommandIterator {
            data: slice,
            running_status: None,
            read_delta_time,
        }
    }
}

impl<'a> Iterator for MidiCommandIterator<'a> {
    type Item = MidiEvent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.data.is_empty() {
            match MidiEvent::from_be_bytes(self.data, self.read_delta_time, self.running_status) {
                Ok((event, new_offset)) => {
                    self.running_status = Some(event.command().status());
                    self.data = new_offset;
                    self.read_delta_time = true;
                    Some(event)
                }
                Err(_) => None, // Handle error appropriately, e.g., log or return None
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use midi_types::{Channel, MidiMessage, Note};

    use crate::packets::midi_packets::rtp_midi_message::RtpMidiMessage;

    use super::*;

    #[test]
    fn test_midi_command_iterator() {
        let data = &[70, 145, 65, 0, 11, 62, 0, 32, 126, 37, 8, 12, 8, 131, 136, 62, 83, 193, 93, 197, 83, 144];
        let iterator = MidiCommandIterator::new(data);
        let events = iterator.collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].delta_time(), 0);
        assert_eq!(events[1].delta_time(), 11);

        let RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(channel, key, velocity)) = events[0].command() else {
            panic!("Unexpected MIDI command")
        };
        assert_eq!(*channel, Channel::from(1));
        assert_eq!(*key, Note::from(65));
        assert_eq!(*velocity, Into::into(0));

        let RtpMidiMessage::MidiMessage(MidiMessage::NoteOn(channel, key, velocity)) = events[1].command() else {
            panic!("Unexpected MIDI command")
        };
        assert_eq!(*channel, Channel::from(1));
        assert_eq!(*key, Note::from(62));
        assert_eq!(*velocity, Into::into(0));
    }
}
