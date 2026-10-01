use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub link: bool,
    pub osc: OscConfig,
    pub midi: MidiConfig,
    pub rtpmidi: RtpConfig,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OscConfig {
    pub enabled: bool,
    pub targets: Vec<String>,
    pub prefix: String,
}
impl Default for OscConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            targets: vec![],
            prefix: "/tempotrack".into(),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiConfig {
    pub enabled: bool,
    pub ports: Vec<MidiPort>,
    pub virtual_port: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiPort {
    pub id: Option<String>,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtpConfig {
    pub enabled: bool,
    pub name: String,
    pub port: u16,
}
impl Default for RtpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: "TempoTrack".into(),
            port: 5004,
        }
    }
}
impl OutputConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if self.osc.enabled && self.osc.targets.is_empty() {
            return Err(Error::Config("Add at least one OSC destination.".into()));
        }
        if !self.osc.prefix.starts_with('/')
            || self.osc.prefix.ends_with('/')
            || self.osc.prefix.contains("//")
            || self
                .osc
                .prefix
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "#*,?[]{}".contains(c))
        {
            return Err(Error::Config(
                "OSC prefix must be a literal path such as /tempotrack, without a trailing slash."
                    .into(),
            ));
        }
        for (index, target) in self.osc.targets.iter().enumerate() {
            validate_target(target)?;
            if self.osc.targets[..index].contains(target) {
                return Err(Error::Config(format!(
                    "OSC destination {target} appears twice."
                )));
            }
        }
        if self.midi.enabled && self.midi.ports.is_empty() && !self.midi.virtual_port {
            return Err(Error::Config(
                "Select a MIDI output or enable the virtual MIDI port.".into(),
            ));
        }
        for (index, port) in self.midi.ports.iter().enumerate() {
            if port.name.chars().any(char::is_control)
                || port
                    .id
                    .as_ref()
                    .is_some_and(|id| id.chars().any(char::is_control))
            {
                return Err(Error::Config(
                    "MIDI output names and IDs cannot contain control characters.".into(),
                ));
            }
            if port.name.trim().is_empty() && port.id.as_ref().is_none_or(|id| id.is_empty()) {
                return Err(Error::Config(
                    "MIDI outputs require a name or port ID.".into(),
                ));
            }
            if self.midi.ports[..index]
                .iter()
                .any(|p| match (&p.id, &port.id) {
                    (Some(a), Some(b)) => a == b,
                    _ => p.name == port.name,
                })
            {
                return Err(Error::Config("A MIDI output is selected twice.".into()));
            }
        }
        if !(1..=65534).contains(&self.rtpmidi.port) {
            return Err(Error::Config(
                "RTP-MIDI control port must be 1–65534; the next port carries MIDI.".into(),
            ));
        }
        if self.rtpmidi.name.trim().is_empty()
            || self.rtpmidi.name.len() > 63
            || self.rtpmidi.name.chars().any(char::is_control)
        {
            return Err(Error::Config(
                "RTP-MIDI session name must contain 1–63 bytes and no control characters.".into(),
            ));
        }
        Ok(())
    }
}
pub fn validate_target(target: &str) -> Result<(), Error> {
    let valid = target.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty()
            && !host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/?#".contains(c))
            && (!host.contains(':')
                || host
                    .strip_prefix('[')
                    .and_then(|h| h.strip_suffix(']'))
                    .is_some_and(|h| h.parse::<std::net::Ipv6Addr>().is_ok()))
            && port.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if valid {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "Invalid OSC destination {target:?}; use host:port or [IPv6]:port."
        )))
    }
}
