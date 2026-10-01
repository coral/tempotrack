//! Rust library for sending and receiving MIDI over RTP (Real-time Transport Protocol), aka AppleMidi.
//!
//! This library is designed to be used with the `tokio` async runtime.
//! It provides a simple API for creating RTP MIDI sessions, sending and receiving MIDI messages, and handling invitations.
//!
//! # Features
//! - **Async**: Built on top of `tokio`, making it suitable for asynchronous applications.
//! - **Invitation Handling**: Can send and receive invitations to join RTP MIDI sessions.
//!   Users can control the logic for accepting or rejecting invitations.
//! - **SysEx Support**: Supports sending and receiving System Exclusive (SysEx) messages.
//!
//! ## Unsupported Features
//! - **Recovery Journal**: The library does not implement the recovery journal feature of RTP MIDI.
//!   This means that if a packet is lost, it cannot be recovered.
pub mod packets;
mod participant;
pub mod sessions;
