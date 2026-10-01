use midi_types::MidiMessage;

use crate::participant::Participant;

pub(super) type MidiMessageListener = dyn Fn((MidiMessage, u32)) + Send + 'static;
pub(super) type SysExPacketListener = dyn for<'a> Fn(&'a [u8]) + Send + 'static;
pub(super) type ParticipantListener = dyn for<'a> Fn(&'a Participant) + Send + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RtpMidiEventType {
    MidiMessage,
    SysExPacket,
    ParticipantJoined,
    ParticipantLeft,
}

pub struct EventListeners {
    midi_message: Vec<Box<MidiMessageListener>>,
    sysex_packet: Vec<Box<SysExPacketListener>>,
    participant_joined: Vec<Box<ParticipantListener>>,
    participant_left: Vec<Box<ParticipantListener>>,
}

pub struct MidiMessageEvent;
pub struct SysExPacketEvent;
pub struct ParticipantJoinedEvent;
pub struct ParticipantLeftEvent;

pub trait EventType {
    type Data<'a>;

    fn add_listener_to_storage<F>(listeners: &mut EventListeners, callback: F)
    where
        F: for<'a> Fn(Self::Data<'a>) + Send + 'static;
}

impl EventType for MidiMessageEvent {
    type Data<'a> = (MidiMessage, u32);

    fn add_listener_to_storage<F>(listeners: &mut EventListeners, callback: F)
    where
        F: for<'a> Fn(Self::Data<'a>) + Send + 'static,
    {
        listeners.midi_message.push(Box::new(callback));
    }
}

impl EventType for SysExPacketEvent {
    type Data<'a> = &'a [u8];

    fn add_listener_to_storage<F>(listeners: &mut EventListeners, callback: F)
    where
        F: for<'a> Fn(Self::Data<'a>) + Send + 'static,
    {
        listeners.sysex_packet.push(Box::new(callback));
    }
}

impl EventType for ParticipantJoinedEvent {
    type Data<'a> = &'a Participant;

    fn add_listener_to_storage<F>(listeners: &mut EventListeners, callback: F)
    where
        F: for<'a> Fn(Self::Data<'a>) + Send + 'static,
    {
        listeners.participant_joined.push(Box::new(callback));
    }
}

impl EventType for ParticipantLeftEvent {
    type Data<'a> = &'a Participant;

    fn add_listener_to_storage<F>(listeners: &mut EventListeners, callback: F)
    where
        F: for<'a> Fn(Self::Data<'a>) + Send + 'static,
    {
        listeners.participant_left.push(Box::new(callback));
    }
}

impl Default for EventListeners {
    fn default() -> Self {
        Self::new()
    }
}

impl EventListeners {
    pub fn new() -> Self {
        Self {
            midi_message: Vec::new(),
            sysex_packet: Vec::new(),
            participant_joined: Vec::new(),
            participant_left: Vec::new(),
        }
    }

    pub fn notify_midi_message(&self, message: MidiMessage, delta_time: u32) {
        for listener in &self.midi_message {
            listener((message, delta_time));
        }
    }

    pub fn notify_sysex_packet(&self, bytes: &[u8]) {
        for listener in &self.sysex_packet {
            listener(bytes);
        }
    }

    pub fn notify_participant_joined(&self, participant: &Participant) {
        for listener in &self.participant_joined {
            listener(participant);
        }
    }

    pub fn notify_participant_left(&self, participant: &Participant) {
        for listener in &self.participant_left {
            listener(participant);
        }
    }
}
