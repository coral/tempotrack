use std::{collections::HashMap, fmt::Debug};

use super::{
    channel_journal::{channel_journal::ChannelJournal, control_change_chapter::ControlChangeChapter, program_change_chapter::ProgramChangeChapter},
    system_journal::system_journal::SystemJournal,
};

#[derive(Debug)]
#[allow(dead_code)]
pub struct RecoveryJournal {
    s_flag: bool,
    a_flag: bool,
    h_flag: bool,
    total_channels: u8,
    checkpoint_sequence_number: u32,
    system_journal: Option<SystemJournal>,                           // Optional system journal
    channel_journals: std::collections::HashMap<u8, ChannelJournal>, // Dictionary of channel journals
}

impl RecoveryJournal {
    pub fn from_be_bytes(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let flags_and_channel_count = bytes[0];
        let y_flag = flags_and_channel_count & 0b0100_0000 != 0; // system journal present
        let checkpoint_sequence_number = u16::from_be_bytes(bytes[1..3].try_into().unwrap());

        let system_journal = if y_flag { Some(SystemJournal::from_be_bytes(&mut bytes[3..])?) } else { None };

        let total_channels = (flags_and_channel_count & 0b0011_1111) as usize; // Total channels

        // Parse channel journals
        let mut channel_journals = HashMap::new();
        for _ in 0..total_channels {
            let s_flag = reader.read_bit()?;
            let channel = reader.read::<4, u8>()?;
            let h_flag = reader.read_bit()?;
            let _length = reader.read::<10, u16>()?;

            // Read TOC
            let has_program_change_chapter = reader.read_bit()?;
            let has_control_change_chapter = reader.read_bit()?;
            let _has_parameter_system_chapter = reader.read_bit()?;
            let _has_pitch_wheel_chapter = reader.read_bit()?;
            let _has_note_off_on_chapter = reader.read_bit()?;
            let _has_note_command_extras_chapter = reader.read_bit()?;
            let _has_channel_aftertouch_chapter = reader.read_bit()?;
            let _has_poly_aftertouch_chapter = reader.read_bit()?;

            let mut channel_journal = ChannelJournal {
                s_flag,
                channel,
                h_flag,
                chapters: HashMap::new(),
            };

            if has_program_change_chapter {
                let chapter = reader.parse::<ProgramChangeChapter>()?;
                channel_journal
                    .chapters
                    .insert(ChannelJournalType::ProgramChange, ChannelJournalChapter::ProgramChange(chapter));
            }
            if has_control_change_chapter {
                let chapter = reader.parse::<ControlChangeChapter>()?;
                channel_journal
                    .chapters
                    .insert(ChannelJournalType::ControlChange, ChannelJournalChapter::ControlChange(chapter));
            }
            channel_journals.insert(channel, channel_journal);
        }
        Ok(RecoveryJournal {
            s_flag,
            a_flag,
            h_flag,
            total_channels,
            checkpoint_sequence_number,
            system_journal,
            channel_journals,
        })
    }
}
