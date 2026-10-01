use super::{
    control_change_chapter::ControlChangeChapter, program_change_chapter::ProgramChangeChapter,
};

#[derive(Debug, Hash, Eq, PartialEq)]
#[allow(dead_code)]
pub enum ChannelJournalType {
    ProgramChange,
    ControlChange,
    ParameterSystem,
    PitchWheel,
    NoteOffOn,
    NoteCommandExtras,
    ChannelAftertouch,
    PolyAftertouch,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ChannelJournal {
    flags_and_channel_count: u8, // s-flag, y-flag, a-flag, h-flag, total channels
    checkpoint_packet_sequence_number: u16,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ChannelJournalChapter {
    ProgramChange(ProgramChangeChapter),
    ControlChange(ControlChangeChapter),
    ParameterSystem,
    PitchWheel,
    NoteOffOn,
    NoteCommandExtras,
    ChannelAftertouch,
    PolyAftertouch,
}
