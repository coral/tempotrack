use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

use crate::Error;

/// Selected by the offline model comparison; not a user preference.
pub const BEATNET_MODEL: u8 = 1;
pub const ATOM_SUBDIVISION: u8 = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
/// Detector labels used by diagnostics; the application always runs Assisted.
pub enum Tracking {
    Pulseweave,
    Beatnet,
    #[default]
    Assisted,
}
impl fmt::Display for Tracking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pulseweave => "Pulseweave",
            Self::Beatnet => "BeatNet",
            Self::Assisted => "BeatNet + advisor",
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LiveControls {
    pub gain_db: f32,
    pub offset_ms: f64,
    pub stop_on_silence: bool,
    pub silence_threshold_db: f32,
    pub silence_hold_ms: u32,
}
impl Default for LiveControls {
    fn default() -> Self {
        Self {
            gain_db: 0.,
            offset_ms: 0.,
            stop_on_silence: true,
            silence_threshold_db: -50.,
            silence_hold_ms: 500,
        }
    }
}
impl LiveControls {
    pub fn validate(&self) -> Result<(), Error> {
        if !self.gain_db.is_finite()
            || !(-24. ..=24.).contains(&self.gain_db)
            || !self.offset_ms.is_finite()
            || !(-500. ..=500.).contains(&self.offset_ms)
            || !self.silence_threshold_db.is_finite()
            || !(-96. ..=0.).contains(&self.silence_threshold_db)
            || !(10..=10_000).contains(&self.silence_hold_ms)
        {
            return Err(Error::Config("gain must be −24..24 dB, offset −500..500 ms, silence threshold −96..0 dBFS, and hold 10..10000 ms".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub host: Option<String>,
    pub input: Option<String>,
    /// Persisted device ID, used by the desktop instead of an unstable enumeration index.
    pub input_id: Option<String>,
    pub input_index: Option<usize>,
    /// None mixes all channels; otherwise a one-based source channel.
    pub channel: Option<u16>,
    pub live: LiveControls,
    pub min_bpm: f64,
    pub max_bpm: f64,
    pub min_meter: u8,
    pub max_meter: u8,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            host: None,
            input: None,
            input_id: None,
            input_index: None,
            channel: None,
            live: LiveControls::default(),
            min_bpm: 55.,
            max_bpm: 215.,
            min_meter: 2,
            max_meter: 4,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        self.live.validate()?;
        if self.channel == Some(0) {
            return Err(Error::Config("channels are one-based".into()));
        }
        beatnet_rs::TrackerConfig {
            min_bpm: self.min_bpm,
            max_bpm: self.max_bpm,
            min_meter: self.min_meter,
            max_meter: self.max_meter,
            ..Default::default()
        }
        .validate()?;
        Ok(())
    }
    pub fn settings_path() -> Result<PathBuf, Error> {
        directories::ProjectDirs::from("net", "coral", "TempoTrack")
            .map(|dirs| dirs.config_dir().join("settings.json"))
            .ok_or_else(|| Error::Config("cannot locate the application settings directory".into()))
    }
    pub fn load() -> Result<Self, Error> {
        let path = Self::settings_path()?;
        match std::fs::read(path) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes)?;
                config.validate()?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<(), Error> {
        self.validate()?;
        let path = Self::settings_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
