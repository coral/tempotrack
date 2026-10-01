struct SystemChapterD {
    flags: u8, // s, b, g, h, j, k, y, z
    reset: Option<u8>,
    tune_request: Option<u8>,
    song_select: Option<u8>,
    undefined_system_common_j: Option<u8>,
    undefined_system_common_k: Option<u8>,
    undefined_system_realtime_y: Option<u8>,
    undefined_system_realtime_z: Option<u8>,
}

impl SystemChapterD {
    pub fn from_be_bytes(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let flags = bytes[0];
        let mut i: usize = 1;

        let reset = if flags & 0b0000_0001 != 0 { Some(bytes[1]) } else { None };
        let tune_request = if flags & 0b0000_0010 != 0 { Some(bytes[2]) } else { None };
        let song_select = if flags & 0b0000_0100 != 0 { Some(bytes[3]) } else { None };
        let undefined_system_common_j = if flags & 0b0000_1000 != 0 { Some(bytes[4]) } else { None };
        let undefined_system_common_k = if flags & 0b0001_0000 != 0 { Some(bytes[5]) } else { None };
        let undefined_system_realtime_y = if flags & 0b0010_0000 != 0 { Some(bytes[6]) } else { None };
        let undefined_system_realtime_z = if flags & 0b0100_0000 != 0 { Some(bytes[7]) } else { None };
        Ok(SystemChapterD {
            flags,
            reset,
            tune_request,
            song_select,
            undefined_system_common_j,
            undefined_system_common_k,
            undefined_system_realtime_y,
            undefined_system_realtime_z,
        })
    }
}
