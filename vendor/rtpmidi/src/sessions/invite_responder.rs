use std::{ffi::CStr, net::SocketAddr};

use crate::packets::control_packets::session_initiation_packet::SessionInitiationPacketBody;

pub type InviteHandler = dyn Fn(&SessionInitiationPacketBody, &CStr, &SocketAddr) -> bool + Send + Sync + 'static;

pub enum InviteResponder {
    Accept,
    Reject,
    Custom(Box<InviteHandler>),
}

impl InviteResponder {
    pub fn handle(&self, packet: &SessionInitiationPacketBody, name: &CStr, addr: &SocketAddr) -> bool {
        match self {
            InviteResponder::Accept => true,
            InviteResponder::Reject => false,
            InviteResponder::Custom(handler) => handler(packet, name, addr),
        }
    }

    pub fn new<F>(handler: F) -> InviteResponder
    where
        F: Fn(&SessionInitiationPacketBody, &CStr, &SocketAddr) -> bool + Send + Sync + 'static,
    {
        InviteResponder::Custom(Box::new(handler))
    }
}

impl std::fmt::Debug for InviteResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InviteResponder::Accept => write!(f, "Accept"),
            InviteResponder::Reject => write!(f, "Reject"),
            InviteResponder::Custom(_) => write!(f, "Custom"),
        }
    }
}
