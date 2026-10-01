use crate::{Error, config::Config};
use clap::{Args as ClapArgs, Parser};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Live audio tempo tracking, on your desktop or headless",
    after_help = "No arguments opens the desktop when built with GUI support. Audio/output flags select headless mode unless --gui is supplied.\nExample: tempotrack --input \"Audio Interface\" --output stdout"
)]
pub struct Args {
    /// Run without opening a window.
    #[arg(long, conflicts_with = "gui")]
    pub headless: bool,
    /// Open the desktop, optionally overriding saved settings with CLI flags.
    #[arg(long)]
    pub gui: bool,
    /// List input devices with their host and zero-based index, then exit.
    #[arg(long, conflicts_with = "list_outputs")]
    pub list_inputs: bool,
    /// List compiled output drivers, then exit.
    #[arg(long)]
    pub list_outputs: bool,
    /// Output driver; repeat to enable distinct outputs. Headless default: stdout.
    #[arg(long = "output", value_parser = ["stdout", "none"], action = clap::ArgAction::Append)]
    pub outputs: Vec<String>,
    #[command(flatten)]
    pub settings: SettingsArgs,
}

#[derive(Debug, Default, ClapArgs)]
pub struct SettingsArgs {
    #[arg(long)]
    pub host: Option<String>,
    /// Exact input device name. Use --list-inputs to discover devices.
    #[arg(long, conflicts_with = "input_index")]
    pub input: Option<String>,
    #[arg(long)]
    pub input_index: Option<usize>,
    /// One-based channel number, or 'mix' to average all input channels.
    #[arg(long, value_parser = parse_channel)]
    pub channel: Option<Channel>,
    #[arg(long, allow_hyphen_values = true)]
    pub gain_db: Option<f32>,
    #[arg(long, allow_hyphen_values = true)]
    pub offset_ms: Option<f64>,
    #[arg(long, action = clap::ArgAction::Set)]
    pub stop_on_silence: Option<bool>,
    #[arg(long, allow_hyphen_values = true)]
    pub silence_threshold_db: Option<f32>,
    #[arg(long)]
    pub silence_hold_ms: Option<u32>,
    #[arg(long)]
    pub min_bpm: Option<f64>,
    #[arg(long)]
    pub max_bpm: Option<f64>,
    #[arg(long)]
    pub min_meter: Option<u8>,
    #[arg(long)]
    pub max_meter: Option<u8>,
}
#[derive(Debug, Clone, Copy)]
pub enum Channel {
    Mix,
    Single(u16),
}
fn parse_channel(value: &str) -> Result<Channel, String> {
    if value == "mix" {
        return Ok(Channel::Mix);
    }
    let channel = value
        .parse::<u16>()
        .map_err(|_| "channel must be 'mix' or a one-based number".to_owned())?;
    if channel == 0 {
        return Err("channels are one-based".into());
    }
    Ok(Channel::Single(channel))
}
impl SettingsArgs {
    pub fn apply(&self, mut config: Config) -> Result<Config, Error> {
        macro_rules! set { ($($field:ident),*) => { $(if let Some(value) = &self.$field { config.$field = value.clone(); })* }; }
        macro_rules! live { ($($field:ident),*) => { $(if let Some(value) = self.$field { config.live.$field = value; })* }; }
        if let Some(host) = &self.host {
            config.host = Some(host.clone());
            config.input_id = None;
        }
        if let Some(input) = &self.input {
            config.input = Some(input.clone());
            config.input_id = None;
            config.input_index = None;
        }
        if let Some(index) = self.input_index {
            config.input_index = Some(index);
            config.input = None;
            config.input_id = None;
        }
        if let Some(channel) = self.channel {
            config.channel = match channel {
                Channel::Mix => None,
                Channel::Single(n) => Some(n),
            };
        }
        set!(min_bpm, max_bpm, min_meter, max_meter);
        live!(
            gain_db,
            offset_ms,
            stop_on_silence,
            silence_threshold_db,
            silence_hold_ms
        );
        config.validate()?;
        Ok(config)
    }
    fn supplied(&self) -> bool {
        self.host.is_some()
            || self.input.is_some()
            || self.input_index.is_some()
            || self.channel.is_some()
            || self.gain_db.is_some()
            || self.offset_ms.is_some()
            || self.stop_on_silence.is_some()
            || self.silence_threshold_db.is_some()
            || self.silence_hold_ms.is_some()
            || self.min_bpm.is_some()
            || self.max_bpm.is_some()
            || self.min_meter.is_some()
            || self.max_meter.is_some()
    }
}
impl Args {
    pub fn wants_gui(&self) -> bool {
        self.gui
            || cfg!(feature = "gui")
                && !self.headless
                && !self.settings.supplied()
                && self.outputs.is_empty()
    }
    pub fn output_names(&self, gui: bool) -> Result<Vec<String>, Error> {
        if self.outputs.is_empty() {
            return Ok(if gui { vec![] } else { vec!["stdout".into()] });
        }
        if self.outputs.len() > 1 && self.outputs.iter().any(|o| o == "none") {
            return Err(Error::Config(
                "--output none cannot be combined with other outputs".into(),
            ));
        }
        for (i, name) in self.outputs.iter().enumerate() {
            if self.outputs[..i].contains(name) {
                return Err(Error::Config(format!("output {name:?} was selected twice")));
            }
        }
        Ok(self.outputs.clone())
    }
}
