pub(crate) trait StatusBit {
    fn status_bit(&self) -> bool;
}

impl StatusBit for u8 {
    fn status_bit(&self) -> bool {
        self & 0x80 != 0
    }
}
