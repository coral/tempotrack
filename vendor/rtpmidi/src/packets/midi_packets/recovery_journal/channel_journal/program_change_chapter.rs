#[derive(Debug)]
#[allow(dead_code)]
pub struct ProgramChangeChapter {
    pub s: bool,
    pub program: u8,
    pub b: bool,
    pub bank_msb: u8,
    pub x: bool,
    pub bank_lsb: u8,
}

impl ProgramChangeChapter {
    fn from_bytes(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let s = reader.read_bit()?;
        let program = reader.read::<7, u8>()?;
        let b = reader.read_bit()?;
        let bank_msb = reader.read::<7, u8>()?;
        let x = reader.read_bit()?;
        let bank_lsb = reader.read::<7, u8>()?;

        Ok(Self {
            s,
            program,
            b,
            bank_msb,
            x,
            bank_lsb,
        })
    }
}
