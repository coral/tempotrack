#[derive(Debug, PartialEq)]
pub struct ControlChangeChapter {
    pub entries: Vec<ControlChangeEntry>,
}

#[derive(Debug, PartialEq)]
pub struct ControlChangeEntry {
    pub number: u8,
    pub value: u8,
    pub value_type: ControlChangeChapterValueType,
}

#[derive(Debug, PartialEq)]
pub enum ControlChangeChapterValueType {
    Value,
    Toggle,
    Count,
}

impl ControlChangeChapter {
    fn from_bytes(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let length = reader.read::<8, u8>()?;
        let mut entries = Vec::new();

        for _ in 0..length {
            let number = reader.read::<7, u8>()?;
            let a_flag = reader.read_bit()?;
            let (value, value_type) = if a_flag {
                let toggle = reader.read_bit()?;
                let value = reader.read::<6, u8>()?;
                let value_type = if toggle {
                    ControlChangeChapterValueType::Toggle
                } else {
                    ControlChangeChapterValueType::Count
                };
                (value, value_type)
            } else {
                (
                    reader.read::<7, u8>()?,
                    ControlChangeChapterValueType::Value,
                )
            };

            entries.push(ControlChangeEntry {
                number,
                value,
                value_type,
            });
        }

        Ok(ControlChangeChapter { entries })
    }
}
