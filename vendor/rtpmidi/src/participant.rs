use std::{
    ffi::{CStr, CString},
    fmt::Display,
    net::SocketAddr,
    time::Instant,
};

use zerocopy::network_endian::U32;

#[derive(Debug, Clone, PartialEq)]
pub struct Participant {
    ctrl_addr: SocketAddr,
    initiator_token: Option<U32>,
    last_clock_sync: Instant,
    name: CString,
    invited_by_us: bool,
    ssrc: U32,
}

impl Participant {
    pub fn new(ctrl_addr: SocketAddr, invited_by_us: bool, initiator_token: Option<U32>, name: &CStr, ssrc: U32) -> Self {
        Participant {
            ctrl_addr,
            initiator_token,
            name: name.to_owned(),
            last_clock_sync: Instant::now(),
            invited_by_us,
            ssrc,
        }
    }

    pub(super) fn midi_port_addr(&self) -> SocketAddr {
        SocketAddr::new(self.ctrl_addr.ip(), self.ctrl_addr.port() + 1)
    }

    pub(super) fn last_clock_sync(&self) -> Instant {
        self.last_clock_sync
    }

    pub(super) fn received_clock_sync(&mut self) {
        self.last_clock_sync = Instant::now();
    }

    pub(super) fn is_invited_by_us(&self) -> bool {
        self.invited_by_us
    }

    pub(super) fn initiator_token(&self) -> Option<U32> {
        self.initiator_token
    }

    pub fn name(&self) -> &CStr {
        &self.name
    }

    pub fn addr(&self) -> SocketAddr {
        self.ctrl_addr
    }

    pub fn ssrc(&self) -> U32 {
        self.ssrc
    }
}

impl Display for Participant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Participant {{ name: {}, addr: {}, ssrc: {} }}",
            self.name.to_str().unwrap_or("Unknown"),
            self.ctrl_addr,
            self.ssrc.get()
        )
    }
}
