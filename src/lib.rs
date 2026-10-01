//! Shared, synchronous audio tracking engine. The desktop UI is an optional client.
#![forbid(unsafe_code)]

pub mod audio;
pub mod backend;
pub mod cli;
pub mod clock;
pub mod config;
pub mod engine;
pub mod output;
pub mod rhythm;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    #[error("audio: {0}")]
    Audio(#[from] cpal::Error),
    #[error("BeatNet: {0}")]
    BeatNet(#[from] beatnet_rs::Error),
    #[error("Pulseweave: {0}")]
    Pulseweave(#[from] pulseweave::stream::StreamError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings: {0}")]
    Settings(#[from] serde_json::Error),
    #[error("the engine is busy; retry the change")]
    Busy,
    #[error("the engine worker stopped unexpectedly")]
    WorkerStopped,
    #[error("audio input stopped delivering samples; check the device and retry")]
    InputStalled,
    #[error("audio stream: {0} Check input settings and retry.")]
    StreamFailed(cpal::ErrorKind),
}
