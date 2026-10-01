#[derive(Debug)]
#[allow(dead_code)]
pub struct SystemJournal {
    flags_and_length: u16,    // s-flag, d-flag, v-flag, q-flag, f-flag, x-flag
    system_chapters: Vec<u8>, // Variable-length system chapters
}

impl SystemJournal {
    pub fn from_be_bytes(bytes: &mut [u8]) -> Result<Self, std::io::Error> {
        let flags_and_length = u16::from_be_bytes([bytes[0], bytes[1]]);
        let chapter_d = flags_and_length & 0b0100_0000_0000_0000 != 0; // d-flag
        let active_sense = flags_and_length & 0b0010_0000_0000_0000 != 0; // v-flag
        let sequencer_state = flags_and_length & 0b0001_0000_0000_0000 != 0; // q-flag
        let midi_time_code = flags_and_length & 0b0000_1000_0000_0000 != 0; // f-flag
        let system_exclusive = flags_and_length & 0b0000_0100_0000_0000 != 0; // x-flag
        let length = (flags_and_length & 0b0000_0011_1111_1111) as usize; // Length of system chapters

        let mut i = 2;
        let mut system_chapters = Vec::new();
        if chapter_d {
            i += 1;
        }

        Ok(SystemJournal {
            flags_and_length,
            system_chapters,
        })
    }
}

#[allow(dead_code)]
enum SystemJournalType {
    S,
    D,
    V,
    Q,
    F,
    X,
}
