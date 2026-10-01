use bytes::{BufMut, BytesMut};

use crate::packets::midi_packets::{midi_command_list_body::MidiEventList, midi_event::MidiEvent};

#[derive(Debug)]
pub struct MidiCommandListHeader {
    flags: MidiCommandListFlags,
    length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiCommandListFlags {
    flags: u8,
}

#[repr(u8)]
enum MidiCommandSectionFlagMasks {
    B = 0b1000_0000,
    J = 0b0100_0000,
    Z = 0b0010_0000,
    P = 0b0001_0000,
}

impl MidiCommandListFlags {
    pub fn new(b_flag: bool, j_flag: bool, z_flag: bool, p_flag: bool) -> Self {
        let mut this = MidiCommandListFlags { flags: 0 };
        this.set_flag(MidiCommandSectionFlagMasks::B, b_flag);
        this.set_flag(MidiCommandSectionFlagMasks::J, j_flag);
        this.set_flag(MidiCommandSectionFlagMasks::Z, z_flag);
        this.set_flag(MidiCommandSectionFlagMasks::P, p_flag);
        this
    }

    fn get_flag(&self, flag: MidiCommandSectionFlagMasks) -> bool {
        self.flags & flag as u8 != 0
    }

    fn set_flag(&mut self, flag: MidiCommandSectionFlagMasks, value: bool) {
        if value {
            self.flags |= flag as u8;
        } else {
            self.flags &= !(flag as u8);
        }
    }

    // pub fn j_flag(&self) -> bool {
    //     self.get_flag(MidiCommandSectionFlagMasks::J)
    // }

    pub fn b_flag(&self) -> bool {
        self.get_flag(MidiCommandSectionFlagMasks::B)
    }

    pub fn z_flag(&self) -> bool {
        self.get_flag(MidiCommandSectionFlagMasks::Z)
    }

    pub fn needs_b_flag(size: usize) -> bool {
        size > 0x0F
    }

    pub fn from_u8(byte: u8) -> Self {
        MidiCommandListFlags { flags: byte & 0xF0 }
    }
}

impl MidiCommandListHeader {
    pub fn new(flags: MidiCommandListFlags, length: usize) -> Self {
        MidiCommandListHeader { flags, length }
    }

    pub fn build_for(events: &[MidiEvent], z_flag: bool) -> Self {
        let length = events.size(z_flag);
        let b_flag = MidiCommandListFlags::needs_b_flag(length);
        let flags = MidiCommandListFlags::new(b_flag, false, false, z_flag);
        Self::new(flags, length)
    }

    pub fn flags(&self) -> &MidiCommandListFlags {
        &self.flags
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn size(&self) -> usize {
        if self.flags.b_flag() { 2 } else { 1 }
    }

    pub fn from_slice(data: &[u8]) -> Self {
        let first_byte = data[0];
        let flags = MidiCommandListFlags::from_u8(first_byte);
        if flags.b_flag() {
            let length_lsb = data[1];
            let length = (((first_byte & 0x0F) as u16) << 8) | (length_lsb as u16);
            Self {
                flags,
                length: length as usize,
            }
        } else {
            let length = (first_byte & 0x0F) as usize;
            Self { flags, length }
        }
    }

    pub fn write(&self, buffer: &mut BytesMut) {
        if self.flags.b_flag() {
            // For large lengths: first byte has flags + upper 4 bits of length
            let first_byte = self.flags.flags | ((self.length >> 8) as u8 & 0x0F);
            buffer.put_u8(first_byte);
            // Second byte has lower 8 bits of length
            let second_byte = (self.length & 0xFF) as u8;
            buffer.put_u8(second_byte);
        } else {
            // For small lengths: first byte has flags + length (4 bits max)
            let first_byte = self.flags.flags | (self.length as u8 & 0x0F);
            buffer.put_u8(first_byte);
        }
    }
}
