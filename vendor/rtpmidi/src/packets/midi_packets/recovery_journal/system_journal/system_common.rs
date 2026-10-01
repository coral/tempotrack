struct SystemCommon {
    flags_and_length: u16, // s, c, v, l, dsz, length
    values: Vec<u8>,       // Variable-length system common values
}

impl SystemCommon {
    pub fn from_be_bytes(bytes: &mut [u8]) -> Result<Self, std::io::Error> {
        let flags_and_length = u16::from_be_bytes(bytes[0..1].try_into().unwrap());
        let length = (flags_and_length & 0b0000_0011_1111_1111) as usize;

        let count_field = (flags_and_length & 0b0100_0000_0000_0000) != 0;
        let value_field = (flags_and_length & 0b0010_0000_0000_0000) != 0;
        let legal_value = (flags_and_length & 0b0001_0000_0000_0000) != 0;

        let dsz = (flags_and_length & 0b0000_1100_0000_0000) >> 10;

        let mut i = 2;
        let mut values = Vec::new();
        while i < length {
            values.push(bytes[i]);
            i += 1;
        }

        Ok(SystemCommon { flags_and_length, values })
    }
}
