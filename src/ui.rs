mod knob;
mod outputs;

use iced::{
    Alignment, Border, Color, Element, Fill, Font, Size, Subscription, Task, Theme, color,
    widget::{
        button, column, container, pick_list, progress_bar, row, scrollable, space, text,
        text_input, toggler,
    },
};
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};
use tempotrack::{
    audio::{self, InputDevice},
    cli::SettingsArgs,
    config::{Config, Tracking},
    engine::{Command, Engine, Event},
    output::{
        OutputDriver, OutputStatus,
        config::{MidiPort, OutputConfig},
    },
    rhythm::{PulseCursor, Quality, RhythmSnapshot, Transport},
};

const COMPACT: Size = Size::new(344., 440.);
const BACKGROUND: Color = color!(0x151719);
const PANEL: Color = color!(0x1e2124);
const LINE: Color = color!(0x303438);
const INK: Color = color!(0xe9e7e2);
const MUTED: Color = color!(0x8a9196);
const ACCENT: Color = color!(0xeeb779);
const MINT: Color = color!(0x96c9b2);
const ERROR: Color = color!(0xe8a192);

pub fn run(
    args: SettingsArgs,
    drivers: Vec<Box<dyn OutputDriver>>,
    output_override: Option<OutputConfig>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (saved, warning) = match Config::load() {
        Ok(config) => (config, None),
        Err(error) => (Config::default(), Some(error.to_string())),
    };
    let mut config = args.apply(saved)?;
    if let Some(outputs) = output_override {
        config.outputs = outputs;
    }
    let mut engine = Engine::spawn(drivers)?;
    engine.configure_outputs(config.outputs.clone())?;
    engine.start(config.clone())?;
    let now = Instant::now();
    let state = RefCell::new(Some(App {
        snapshot: RhythmSnapshot::empty(now, Tracking::Assisted),
        draft: Draft::new(config.clone()),
        config,
        engine: Some(engine),
        settings: false,
        settings_tab: SettingsTab::Audio,
        output_page: outputs::Page::Overview,
        output_status: vec![],
        output_status_at: now,
        midi_ports: vec![],
        midi_loading: false,
        midi_error: None,
        settings_error: None,
        running: true,
        error: warning,
        devices: vec![],
        now,
        flashes: [None; 3],
        cursor: PulseCursor::default(),
        input_label: "Opening input…".into(),
        editor: None,
    }));
    iced::application(
        move || {
            (
                state.borrow_mut().take().expect("application boots once"),
                discover(),
            )
        },
        App::update,
        App::view,
    )
    .title("TempoTrack")
    .theme(theme())
    .window(iced::window::Settings {
        size: COMPACT,
        min_size: Some(COMPACT),
        ..Default::default()
    })
    .centered()
    .exit_on_close_request(false)
    .subscription(App::subscription)
    .run()?;
    Ok(())
}
fn theme() -> Theme {
    Theme::custom(
        "TempoTrack",
        iced::theme::Palette {
            background: BACKGROUND,
            text: INK,
            primary: ACCENT,
            success: MINT,
            warning: ACCENT,
            danger: ERROR,
        },
    )
}
fn discover() -> Task<Message> {
    Task::perform(
        async { audio::list_inputs().map_err(|e| e.to_string()) },
        Message::Devices,
    )
}

struct App {
    config: Config,
    draft: Draft,
    engine: Option<Engine>,
    snapshot: RhythmSnapshot,
    settings: bool,
    settings_tab: SettingsTab,
    output_page: outputs::Page,
    output_status: Vec<OutputStatus>,
    output_status_at: Instant,
    midi_ports: Vec<MidiPort>,
    midi_loading: bool,
    midi_error: Option<String>,
    settings_error: Option<String>,
    running: bool,
    error: Option<String>,
    devices: Vec<InputDevice>,
    now: Instant,
    flashes: [Option<Instant>; 3],
    cursor: PulseCursor,
    input_label: String,
    editor: Option<ValueEditor>,
}
#[derive(Debug, Clone)]
enum Message {
    Tick(Instant),
    ToggleRun,
    Reset,
    Knob(Parameter, knob::Edit),
    EditValue(Parameter),
    ValueText(String),
    CommitValue,
    CancelValue,
    CheckEditorFocus,
    EditorFocus(Parameter, bool),
    Tab(bool),
    Silence(bool),
    Settings(bool),
    SettingsTab(SettingsTab),
    Output(outputs::Message),
    MidiPorts(Result<Vec<MidiPort>, String>),
    Apply,
    Refresh,
    Devices(Result<Vec<InputDevice>, String>),
    Host(String),
    Device(DeviceChoice),
    DefaultInput,
    Field(Field, String),
    Close,
    Closed(Result<(), String>),
    Dismiss,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Audio,
    Outputs,
}
#[derive(Debug, Clone, Copy)]
enum Field {
    Channel,
    MinBpm,
    MaxBpm,
    MinMeter,
    MaxMeter,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parameter {
    Gain,
    Offset,
    Threshold,
    Hold,
}
impl Parameter {
    fn spec(self) -> knob::Parameter {
        use knob::{Parameter as Spec, Unit};
        let defaults = tempotrack::config::LiveControls::default();
        match self {
            Self::Gain => Spec {
                label: "Gain",
                min: -24.,
                max: 24.,
                default: f64::from(defaults.gain_db),
                logarithmic: false,
                bipolar: true,
                unit: Unit::Decibels,
            },
            Self::Offset => Spec {
                label: "Offset",
                min: -500.,
                max: 500.,
                default: defaults.offset_ms,
                logarithmic: false,
                bipolar: true,
                unit: Unit::Milliseconds,
            },
            Self::Threshold => Spec {
                label: "Threshold",
                min: -96.,
                max: 0.,
                default: f64::from(defaults.silence_threshold_db),
                logarithmic: false,
                bipolar: false,
                unit: Unit::Decibels,
            },
            Self::Hold => Spec {
                label: "Hold",
                min: 10.,
                max: 10000.,
                default: f64::from(defaults.silence_hold_ms),
                logarithmic: true,
                bipolar: false,
                unit: Unit::Milliseconds,
            },
        }
    }
}
struct ValueEditor {
    parameter: Parameter,
    text: String,
    invalid: bool,
}
#[derive(Debug, Clone, PartialEq)]
struct DeviceChoice(InputDevice, bool);
impl std::fmt::Display for DeviceChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.1 {
            write!(f, "{} · #{}", self.0.name, self.0.index)
        } else {
            f.write_str(&self.0.name)
        }
    }
}
struct Draft {
    config: Config,
    channel: String,
    min_bpm: String,
    max_bpm: String,
    min_meter: String,
    max_meter: String,
    rtp_port: String,
}
impl Draft {
    fn new(config: Config) -> Self {
        Self {
            channel: config.channel.map_or("Mix".into(), |n| n.to_string()),
            min_bpm: config.min_bpm.to_string(),
            max_bpm: config.max_bpm.to_string(),
            min_meter: config.min_meter.to_string(),
            max_meter: config.max_meter.to_string(),
            rtp_port: config.outputs.rtpmidi.port.to_string(),
            config,
        }
    }
    fn field(&mut self, field: Field) -> &mut String {
        match field {
            Field::Channel => &mut self.channel,
            Field::MinBpm => &mut self.min_bpm,
            Field::MaxBpm => &mut self.max_bpm,
            Field::MinMeter => &mut self.min_meter,
            Field::MaxMeter => &mut self.max_meter,
        }
    }
    fn parse(&self) -> Result<Config, String> {
        fn number<T: std::str::FromStr>(s: &str, label: &str) -> Result<T, String> {
            s.trim()
                .parse()
                .map_err(|_| format!("Enter a valid {label}."))
        }
        let mut config = self.config.clone();
        config.channel = if self.channel == "Mix" {
            None
        } else {
            Some(number(&self.channel, "channel")?)
        };
        config.min_bpm = number(&self.min_bpm, "minimum tempo")?;
        config.max_bpm = number(&self.max_bpm, "maximum tempo")?;
        config.min_meter = number(&self.min_meter, "minimum meter")?;
        config.max_meter = number(&self.max_meter, "maximum meter")?;
        config.outputs.rtpmidi.port = number(&self.rtp_port, "RTP-MIDI port")?;
        config.outputs.validate().map_err(|e| e.to_string())?;
        config.validate().map_err(|e| e.to_string())?;
        Ok(config)
    }
}

impl App {
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(16)).map(Message::Tick),
            iced::window::close_requests().map(|_| Message::Close),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::CancelValue),
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                    modifiers,
                    ..
                }) => Some(Message::Tab(modifiers.shift())),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::CancelValue),
                iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
                    iced::mouse::Button::Left,
                ))
                | iced::Event::Touch(iced::touch::Event::FingerPressed { .. }) => {
                    Some(Message::CheckEditorFocus)
                }
                _ => None,
            }),
        ])
    }
    fn persist(&mut self) {
        if let Err(error) = self.config.save() {
            self.error = Some(error.to_string());
        }
    }
    fn send(&mut self, command: Command) {
        if let Some(engine) = &mut self.engine
            && let Err(error) = engine.send(command)
        {
            self.error = Some(error.to_string());
        }
    }
    fn restart(&mut self) {
        self.error = None;
        self.cursor.reset();
        self.flashes = [None; 3];
        self.snapshot.transport = Transport::Initializing;
        self.snapshot.grids = [None; 3];
        self.snapshot.bpm = None;
        if let Some(engine) = &mut self.engine
            && let Err(error) = engine.start(self.config.clone())
        {
            self.error = Some(error.to_string());
            self.running = false;
        }
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick(now) => {
                self.now = now;
                if let Some(engine) = &mut self.engine {
                    if self.settings
                        && now.saturating_duration_since(self.output_status_at)
                            >= Duration::from_millis(250)
                    {
                        self.output_status = engine.output_status();
                        self.output_status_at = now;
                    }
                    if let Some(snapshot) = engine.latest() {
                        self.snapshot = snapshot;
                    }
                    while let Some(event) = engine.event() {
                        match event {
                            Event::Started {
                                input,
                                rate,
                                channels,
                            } => self.input_label = format!("{input} · {rate} Hz · {channels} ch"),
                            Event::Error(error) => {
                                self.error = Some(error);
                                self.running = false;
                            }
                            Event::OutputError(name) => {
                                self.settings_error = Some(format!("Output {name} failed"))
                            }
                        }
                    }
                    if engine.is_finished() {
                        self.error = Some(
                            "The analysis worker stopped unexpectedly. Restart TempoTrack.".into(),
                        );
                        self.running = false;
                    }
                }
                for (i, pulse) in self
                    .cursor
                    .poll(&self.snapshot, now)
                    .into_iter()
                    .enumerate()
                {
                    if pulse {
                        self.flashes[i] = Some(now);
                    }
                }
            }
            Message::ToggleRun => {
                self.running = !self.running;
                if self.running {
                    self.restart();
                } else {
                    self.send(Command::Stop);
                    self.snapshot.transport = Transport::Stopped;
                }
            }
            Message::Reset => {
                self.cursor.reset();
                self.flashes = [None; 3];
                self.send(Command::Reset);
            }
            Message::Knob(parameter, edit) => match edit {
                knob::Edit::Begin => self.editor = None,
                knob::Edit::Change(value) => self.set_parameter(parameter, value),
                knob::Edit::End => {
                    if !self.settings {
                        self.persist();
                    }
                }
                knob::Edit::EnterValue => return self.update(Message::EditValue(parameter)),
            },
            Message::EditValue(parameter) => {
                self.editor = Some(ValueEditor {
                    parameter,
                    text: parameter.spec().format(self.parameter_value(parameter)),
                    invalid: false,
                });
                return iced::widget::operation::focus("knob-value")
                    .chain(iced::widget::operation::select_all("knob-value"));
            }
            Message::ValueText(value) => {
                if let Some(editor) = &mut self.editor {
                    editor.text = value;
                    editor.invalid = false;
                }
            }
            Message::CommitValue => {
                if let Some(editor) = &mut self.editor {
                    if let Some(value) = editor.parameter.spec().parse(&editor.text) {
                        let parameter = editor.parameter;
                        self.editor = None;
                        self.set_parameter(parameter, value);
                        if !self.settings {
                            self.persist();
                        }
                    } else {
                        editor.invalid = true;
                    }
                }
            }
            Message::CheckEditorFocus => {
                if let Some(editor) = &self.editor {
                    let parameter = editor.parameter;
                    return iced::widget::operation::is_focused("knob-value")
                        .map(move |focused| Message::EditorFocus(parameter, focused));
                }
            }
            Message::EditorFocus(parameter, focused) => {
                if !focused
                    && self
                        .editor
                        .as_ref()
                        .is_some_and(|e| e.parameter == parameter)
                {
                    self.editor = None;
                }
            }
            Message::CancelValue => self.editor = None,
            Message::Tab(backwards) => {
                return if backwards {
                    iced::widget::operation::focus_previous()
                } else {
                    iced::widget::operation::focus_next()
                };
            }
            Message::Silence(stop) => {
                self.config.live.stop_on_silence = stop;
                self.send(Command::Adjust(self.config.live));
                self.persist();
            }
            Message::Settings(open) => {
                self.settings = open;
                if open {
                    self.draft = Draft::new(self.config.clone());
                    self.settings_error = None;
                }
                self.editor = None;
            }
            Message::SettingsTab(tab) => {
                self.settings_tab = tab;
                self.editor = None;
                if tab == SettingsTab::Outputs && self.midi_ports.is_empty() && !self.midi_loading {
                    return self.refresh_midi();
                }
            }
            Message::Output(message) => return self.update_output(message),
            Message::MidiPorts(result) => {
                self.midi_loading = false;
                match result {
                    Ok(ports) => {
                        self.midi_ports = ports;
                        self.midi_error = None;
                    }
                    Err(error) => self.midi_error = Some(error),
                }
            }
            Message::Refresh => return discover(),
            Message::Devices(result) => match result {
                Ok(devices) => self.devices = devices,
                Err(error) => self.error = Some(error),
            },
            Message::Host(host) => {
                self.draft.config.host = (host != "Default").then_some(host);
                self.draft.config.input = None;
                self.draft.config.input_id = None;
                self.draft.config.input_index = None;
                self.draft.channel = "Mix".into();
            }
            Message::Device(DeviceChoice(device, _)) => {
                self.draft.config.host = Some(device.host);
                self.draft.config.input = Some(device.name);
                self.draft.config.input_id = Some(device.id);
                self.draft.config.input_index = None;
                self.draft.channel = "Mix".into();
            }
            Message::DefaultInput => {
                self.draft.config.host = None;
                self.draft.channel = "Mix".into();
                self.draft.config.input = None;
                self.draft.config.input_id = None;
                self.draft.config.input_index = None;
            }
            Message::Field(field, value) => *self.draft.field(field) = value,
            Message::Apply => match self.draft.parse() {
                Ok(config) => {
                    let audio_changed = audio_settings_changed(&self.config, &config);
                    if self.config.outputs != config.outputs
                        && let Some(engine) = &mut self.engine
                        && let Err(error) = engine.configure_outputs(config.outputs.clone())
                    {
                        self.settings_error = Some(error.to_string());
                        return Task::none();
                    }
                    self.config = config;
                    self.settings_error = None;
                    if let Err(error) = self.config.save() {
                        self.settings_error = Some(error.to_string());
                    }
                    if self.running && audio_changed {
                        self.restart();
                    }
                    if self.settings_error.is_none() {
                        self.settings = false;
                    }
                    self.editor = None;
                }
                Err(error) => self.settings_error = Some(error),
            },
            Message::Dismiss => self.error = None,
            Message::Close => {
                let engine = self.engine.take();
                let config = self.config.clone();
                return Task::perform(
                    async move {
                        if let Some(mut engine) = engine {
                            engine.shutdown().map_err(|e| e.to_string())?;
                        }
                        config.save().map_err(|e| e.to_string())
                    },
                    Message::Closed,
                );
            }
            Message::Closed(result) => {
                if let Err(error) = result {
                    eprintln!("TempoTrack: {error}");
                }
                return iced::exit();
            }
        }
        Task::none()
    }
    fn view(&self) -> Element<'_, Message> {
        if self.settings {
            container(self.settings_view().spacing(8))
                .padding(16)
                .height(Fill)
                .width(Fill)
                .into()
        } else {
            // Fixed instrument layout: an error never adds a scrolling second page.
            container(self.main_view().spacing(10))
                .padding(16)
                .height(Fill)
                .width(Fill)
                .into()
        }
    }
    fn main_view(&self) -> iced::widget::Column<'_, Message> {
        let bpm = self
            .snapshot
            .bpm
            .map_or_else(|| "—".into(), |bpm| format!("{bpm:.1}"));
        let (status, status_color) = if self.error.is_some() {
            ("INPUT UNAVAILABLE", ERROR)
        } else {
            match self.snapshot.transport {
                Transport::Tracking => ("TRACKING", MINT),
                Transport::Holdover => ("FREE RUNNING", ACCENT),
                Transport::Initializing => ("STARTING", ACCENT),
                Transport::Listening => ("ACQUIRING", ACCENT),
                Transport::Silence => ("NO SIGNAL", MUTED),
                Transport::Stopped => ("STOPPED", MUTED),
                Transport::Error => ("INPUT UNAVAILABLE", ERROR),
            }
        };
        let header = row![
            dot(ACCENT, 5.),
            text("LIVE TEMPO").size(10).color(MUTED).font(medium_font()),
            space::horizontal(),
            button(text("Settings").size(11))
                .padding([5, 9])
                .on_press(Message::Settings(true))
                .style(quiet_button),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let hero = row![
            text(bpm)
                .size(56)
                .line_height(1.)
                .font(Font::MONOSPACE)
                .color(INK),
            container(text("BPM").size(10).font(medium_font()).color(MUTED)).padding(
                iced::Padding {
                    bottom: 7.,
                    ..Default::default()
                }
            ),
            space::horizontal(),
            container(
                column![
                    dot(status_color, 5.),
                    text(status).size(8).color(status_color)
                ]
                .spacing(7)
                .align_x(Alignment::End)
            )
            .padding(iced::Padding {
                bottom: 8.,
                ..Default::default()
            }),
        ]
        .spacing(8)
        .align_y(Alignment::End);
        let mut lamps = row![].spacing(7);
        for (i, label) in ["BAR", "BEAT", "ATOM"].into_iter().enumerate() {
            let glow = if self.snapshot.active(self.now) {
                self.flashes[i].map_or(0., |time| {
                    (1. - self.now.saturating_duration_since(time).as_secs_f32() / 0.16).max(0.)
                })
            } else {
                0.
            };
            let accent = [ACCENT, INK, MINT][i];
            let phase = self.snapshot.phase(i, self.now).unwrap_or(0.) as f32;
            let fill = mix(PANEL, accent, glow * 0.24);
            let lamp = container(column![
                row![
                    text(label)
                        .size(10)
                        .font(medium_font())
                        .color(mix(MUTED, accent, glow)),
                    space::horizontal(),
                    dot(mix(LINE, accent, glow), 6.)
                ]
                .align_y(Alignment::Center),
                space::vertical(),
                progress_bar(0. ..=1., phase)
                    .girth(2)
                    .style(move |_| progress_bar::Style {
                        background: LINE.into(),
                        bar: accent.scale_alpha(0.65).into(),
                        border: Border {
                            radius: 1.into(),
                            ..Default::default()
                        },
                    }),
            ])
            .padding(11)
            .width(Fill)
            .height(54)
            .style(move |_| container::Style {
                background: Some(fill.into()),
                border: Border {
                    radius: 7.into(),
                    width: 1.,
                    color: mix(LINE, accent, glow * 0.6),
                },
                ..Default::default()
            });
            lamps = lamps.push(lamp);
        }
        let device = self
            .input_label
            .split('·')
            .next()
            .unwrap_or("Audio input")
            .trim();
        let short_device: String = if device.chars().count() > 29 {
            device.chars().take(27).chain(['…']).collect()
        } else {
            device.into()
        };
        let input = column![
            row![
                text(short_device).size(10).color(MUTED),
                space::horizontal(),
                text(format!("{:.0} dB", self.snapshot.level_db))
                    .size(10)
                    .font(Font::MONOSPACE)
                    .color(MUTED)
            ],
            progress_bar(-60. ..=0., self.snapshot.level_db)
                .girth(3)
                .style(|_| progress_bar::Style {
                    background: LINE.into(),
                    bar: MINT.into(),
                    border: Border {
                        radius: 2.into(),
                        ..Default::default()
                    },
                }),
        ]
        .spacing(6);
        let controls = row![
            self.parameter_control(Parameter::Gain),
            self.parameter_control(Parameter::Offset)
        ]
        .spacing(24);
        let silence = row![
            text("Stop on silence").size(11).color(MUTED),
            space::horizontal(),
            toggler(self.config.live.stop_on_silence)
                .size(14)
                .on_toggle(Message::Silence),
        ]
        .align_y(Alignment::Center);
        let actions = row![
            button(
                container(
                    text(if self.running {
                        "Stop tracking"
                    } else {
                        "Start tracking"
                    })
                    .size(12)
                    .font(medium_font())
                )
                .center_x(Fill)
            )
            .padding([8, 12])
            .width(Fill)
            .on_press(Message::ToggleRun)
            .style(primary_button),
            button(container(text("Reset").size(11)).center_x(Fill))
                .padding([8, 12])
                .width(70)
                .on_press(Message::Reset)
                .style(quiet_button),
        ]
        .spacing(8);
        let footer: Element<'_, Message> = if self.error.is_some() {
            row![
                dot(ERROR, 4.),
                text("Check your audio input").size(10).color(ERROR),
                space::horizontal(),
                button(text("Details").size(10).color(ERROR))
                    .padding([2, 5])
                    .on_press(Message::Settings(true))
                    .style(quiet_button)
            ]
            .spacing(7)
            .align_y(Alignment::Center)
            .into()
        } else {
            let detail = match self.snapshot.quality {
                Quality::BeatNetConfidence(c) if self.snapshot.active(self.now) => {
                    format!("Particle coherence {:.0}%", c * 100.)
                }
                _ => "Atom · derived subdivision".into(),
            };
            row![
                text(detail).size(9).color(MUTED),
                space::horizontal(),
                text(if self.snapshot.sample_rate > 0 {
                    format!("{:.1} kHz", self.snapshot.sample_rate as f32 / 1000.)
                } else {
                    String::new()
                })
                .size(9)
                .font(Font::MONOSPACE)
                .color(MUTED)
            ]
            .into()
        };
        column![
            header,
            hero,
            lamps,
            input,
            divider(),
            controls,
            silence,
            space::vertical(),
            actions,
            footer
        ]
    }
    fn parameter_value(&self, parameter: Parameter) -> f64 {
        match parameter {
            Parameter::Gain => f64::from(self.config.live.gain_db),
            Parameter::Offset => self.config.live.offset_ms,
            Parameter::Threshold => f64::from(self.draft.config.live.silence_threshold_db),
            Parameter::Hold => f64::from(self.draft.config.live.silence_hold_ms),
        }
    }
    fn set_parameter(&mut self, parameter: Parameter, value: f64) {
        match parameter {
            Parameter::Gain => self.config.live.gain_db = value as f32,
            Parameter::Offset => self.config.live.offset_ms = value,
            Parameter::Threshold => self.draft.config.live.silence_threshold_db = value as f32,
            Parameter::Hold => self.draft.config.live.silence_hold_ms = value.round() as u32,
        }
        if matches!(parameter, Parameter::Gain | Parameter::Offset) {
            self.send(Command::Adjust(self.config.live));
        }
    }
    fn parameter_control(&self, parameter: Parameter) -> Element<'_, Message> {
        let spec = parameter.spec();
        let value = self.parameter_value(parameter);
        let display: Element<'_, Message> =
            match self.editor.as_ref().filter(|e| e.parameter == parameter) {
                Some(editor) => text_input("Value", &editor.text)
                    .id("knob-value")
                    .size(11)
                    .padding([3, 5])
                    .align_x(Alignment::Center)
                    .on_input(Message::ValueText)
                    .on_submit(Message::CommitValue)
                    .style(move |theme, status| {
                        let mut style = input_style(theme, status);
                        if editor.invalid {
                            style.border.color = ERROR;
                        }
                        style
                    })
                    .width(92)
                    .into(),
                None => button(text(spec.format(value)).size(11).font(Font::MONOSPACE))
                    .padding([3, 5])
                    .on_press(Message::EditValue(parameter))
                    .style(value_button)
                    .into(),
            };
        column![
            text(spec.label).size(10).color(MUTED),
            knob::Knob::new(spec, value, move |edit| Message::Knob(parameter, edit)),
            display,
        ]
        .spacing(0)
        .align_x(Alignment::Center)
        .width(Fill)
        .into()
    }
    fn audio_settings(&self) -> iced::widget::Column<'_, Message> {
        let draft = &self.draft;
        let host = draft
            .config
            .host
            .clone()
            .unwrap_or_else(audio::default_host_name);
        let hosts = audio::hosts();
        let available: Vec<_> = self
            .devices
            .iter()
            .filter(|d| d.host == host)
            .cloned()
            .map(|device| {
                let duplicate = self
                    .devices
                    .iter()
                    .filter(|other| other.host == device.host && other.name == device.name)
                    .count()
                    > 1;
                DeviceChoice(device, duplicate)
            })
            .collect();
        let selected = available
            .iter()
            .find(|d| {
                draft
                    .config
                    .input_id
                    .as_ref()
                    .is_some_and(|id| id == &d.0.id)
                    || draft.config.input_id.is_none()
                        && draft
                            .config
                            .input
                            .as_ref()
                            .is_some_and(|name| name == &d.0.name)
            })
            .cloned();
        let effective = selected
            .as_ref()
            .or_else(|| available.iter().find(|d| d.0.is_default));
        let max_channels = effective
            .and_then(|d| d.0.configs.iter().map(|c| c.channels()).max())
            .unwrap_or(2)
            .min(64);
        let mut channels = vec!["Mix".to_owned()];
        channels.extend((1..=max_channels).map(|n| n.to_string()));
        let mut body = column![].spacing(6);
        if let Some(error) = &self.error {
            body = body.push(
                column![
                    text(error).size(11).color(ERROR),
                    button(text("Dismiss").size(10))
                        .on_press(Message::Dismiss)
                        .style(quiet_button)
                ]
                .spacing(4),
            );
        }
        if hosts.len() > 1 {
            body = body.push(
                row![
                    text("Audio backend").size(10).color(MUTED),
                    pick_list(hosts, Some(host), Message::Host)
                        .text_size(11)
                        .padding([5, 8])
                        .style(field_style)
                        .width(Fill),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
        }
        body = body.push(
            column![
                row![
                    text("AUDIO INPUT").size(9).color(MUTED),
                    space::horizontal(),
                    button(text("Default").size(10))
                        .padding([2, 5])
                        .on_press(Message::DefaultInput)
                        .style(value_button),
                    button(text("Refresh").size(10))
                        .padding([2, 5])
                        .on_press(Message::Refresh)
                        .style(value_button)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                pick_list(available, selected, Message::Device)
                    .placeholder("System default input")
                    .text_size(12)
                    .padding([7, 9])
                    .style(field_style)
                    .width(Fill),
                row![
                    text("Channel").size(10).color(MUTED),
                    space::horizontal(),
                    pick_list(channels, Some(draft.channel.clone()), |s| Message::Field(
                        Field::Channel,
                        s
                    ))
                    .text_size(11)
                    .padding([4, 8])
                    .style(field_style)
                    .width(92)
                ]
                .align_y(Alignment::Center),
            ]
            .spacing(6),
        );
        body = body
            .push(divider())
            .push(
                column![
                    text("SILENCE GATE").size(9).color(MUTED),
                    row![
                        self.parameter_control(Parameter::Threshold),
                        self.parameter_control(Parameter::Hold)
                    ]
                    .spacing(24)
                ]
                .spacing(4),
            )
            .push(divider())
            .push(
                column![
                    row![
                        text("TRACKING RANGE").size(9).color(MUTED).width(112),
                        text("Min").size(10).color(MUTED).width(Fill),
                        text("Max").size(10).color(MUTED).width(Fill)
                    ]
                    .spacing(10),
                    row![
                        text("BPM").size(11).color(MUTED).width(112),
                        input_field(&draft.min_bpm, Field::MinBpm),
                        input_field(&draft.max_bpm, Field::MaxBpm)
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    row![
                        text("Beats / bar").size(11).color(MUTED).width(112),
                        input_field(&draft.min_meter, Field::MinMeter),
                        input_field(&draft.max_meter, Field::MaxMeter)
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                ]
                .spacing(8),
            );
        body
    }
    fn settings_view(&self) -> iced::widget::Column<'_, Message> {
        let body = match self.settings_tab {
            SettingsTab::Audio => self.audio_settings(),
            SettingsTab::Outputs => self.outputs_view(),
        };
        let mut result = column![
            row![
                text("Settings").size(20).font(medium_font()),
                space::horizontal(),
                button(text("Back").size(11))
                    .padding([5, 9])
                    .on_press(Message::Settings(false))
                    .style(quiet_button)
            ]
            .align_y(Alignment::Center),
            row![
                self.settings_tab_button("Audio", SettingsTab::Audio),
                self.settings_tab_button("Outputs", SettingsTab::Outputs),
            ]
            .spacing(6),
            scrollable(body.padding(iced::Padding {
                right: 8.,
                ..Default::default()
            }))
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(3)
                    .scroller_width(3)
                    .margin(1)
            ))
            .height(Fill),
        ];
        if let Some(error) = &self.settings_error {
            result = result.push(text(error).size(10).color(ERROR));
        }
        result.push(
            button(container(text("Apply settings").size(12)).center_x(Fill))
                .padding([8, 12])
                .width(Fill)
                .on_press(Message::Apply)
                .style(primary_button),
        )
    }
    fn settings_tab_button(&self, label: &'static str, tab: SettingsTab) -> Element<'_, Message> {
        let active = self.settings_tab == tab;
        button(container(text(label).size(11)).center_x(Fill))
            .on_press(Message::SettingsTab(tab))
            .padding([6, 8])
            .width(Fill)
            .style(move |theme, status| {
                if active {
                    primary_button(theme, status)
                } else {
                    quiet_button(theme, status)
                }
            })
            .into()
    }
}

/// Outputs can be reconfigured without invalidating the audio-derived clock.
fn audio_settings_changed(previous: &Config, next: &Config) -> bool {
    let mut previous = previous.clone();
    previous.outputs = next.outputs.clone();
    previous != *next
}

fn input_field(value: &str, field: Field) -> Element<'_, Message> {
    text_input("", value)
        .size(12)
        .padding([5, 8])
        .style(input_style)
        .on_input(move |s| Message::Field(field, s))
        .width(Fill)
        .into()
}
fn medium_font() -> Font {
    Font {
        weight: iced::font::Weight::Medium,
        ..Font::DEFAULT
    }
}
fn mix(a: Color, b: Color, amount: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: 1.,
    }
}
fn dot(color: Color, size: f32) -> Element<'static, Message> {
    container(space())
        .width(size)
        .height(size)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: (size / 2.).into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
fn divider() -> Element<'static, Message> {
    container(space())
        .height(1)
        .width(Fill)
        .style(|_| container::Style {
            background: Some(LINE.into()),
            ..Default::default()
        })
        .into()
}
fn quiet_button(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                LINE
            } else {
                PANEL
            }
            .into(),
        ),
        text_color: INK,
        border: Border {
            radius: 6.into(),
            width: 1.,
            color: LINE,
        },
        ..Default::default()
    }
}
fn primary_button(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                mix(ACCENT, INK, 0.2)
            } else {
                ACCENT
            }
            .into(),
        ),
        text_color: BACKGROUND,
        border: Border {
            radius: 6.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
fn field_style(_: &Theme, status: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: INK,
        placeholder_color: MUTED,
        handle_color: MUTED,
        background: PANEL.into(),
        border: Border {
            radius: 6.into(),
            width: 1.,
            color: if matches!(status, pick_list::Status::Active) {
                LINE
            } else {
                ACCENT.scale_alpha(0.5)
            },
        },
    }
}
fn value_button(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        text_color: if status == button::Status::Hovered {
            ACCENT
        } else {
            INK
        },
        ..Default::default()
    }
}

fn input_style(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    style.background = BACKGROUND.into();
    style.border.radius = 6.into();
    style.border.color = if matches!(status, text_input::Status::Focused { .. }) {
        ACCENT.scale_alpha(0.6)
    } else {
        LINE
    };
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings_app() -> App {
        let config = Config::default();
        let now = Instant::now();
        App {
            draft: Draft::new(config.clone()),
            config,
            engine: None,
            snapshot: RhythmSnapshot::empty(now, Tracking::Assisted),
            settings: true,
            settings_tab: SettingsTab::Audio,
            output_page: outputs::Page::Overview,
            output_status: vec![],
            output_status_at: now,
            midi_ports: vec![],
            midi_loading: false,
            midi_error: None,
            settings_error: None,
            running: false,
            error: None,
            devices: vec![],
            now,
            flashes: [None; 3],
            cursor: PulseCursor::default(),
            input_label: String::new(),
            editor: None,
        }
    }
    #[test]
    fn numeric_entry_commits_units_rejects_invalid_text_and_cancels_cleanly() {
        let mut app = settings_app();
        let _ = app.update(Message::EditValue(Parameter::Hold));
        let _ = app.update(Message::ValueText("0.2s".into()));
        let _ = app.update(Message::CommitValue);
        assert_eq!(app.draft.config.live.silence_hold_ms, 200);
        assert!(app.editor.is_none());
        // Draft changes do not leak into the live engine before Apply.
        assert_eq!(app.config.live.silence_hold_ms, 500);
        let _ = app.update(Message::EditValue(Parameter::Hold));
        let _ = app.update(Message::ValueText("garbage".into()));
        let _ = app.update(Message::CommitValue);
        assert!(app.editor.as_ref().unwrap().invalid);
        assert_eq!(app.draft.config.live.silence_hold_ms, 200);
        let _ = app.update(Message::CancelValue);
        assert!(app.editor.is_none());
        let _ = app.update(Message::EditValue(Parameter::Hold));
        let _ = app.update(Message::ValueText("900ms".into()));
        let _ = app.update(Message::EditorFocus(Parameter::Hold, false));
        assert!(app.editor.is_none());
        assert_eq!(app.draft.config.live.silence_hold_ms, 200);
    }
    #[test]
    fn output_only_changes_preserve_audio_and_apply_is_explicit() {
        let mut app = settings_app();
        let _ = app.update(Message::Output(outputs::Message::Enable(
            outputs::Page::Link,
            true,
        )));
        let _ = app.update(Message::Output(outputs::Message::AddOsc));
        let _ = app.update(Message::Output(outputs::Message::OscTarget(
            0,
            "127.0.0.1:7000".into(),
        )));
        let draft = app.draft.parse().unwrap();
        assert!(draft.outputs.link);
        assert_eq!(draft.outputs.osc.targets, ["127.0.0.1:7000"]);
        assert!(!app.config.outputs.link);
        assert!(!audio_settings_changed(&app.config, &draft));
        let mut audio = draft;
        audio.live.silence_hold_ms += 100;
        assert!(audio_settings_changed(&app.config, &audio));
        let _ = app.update(Message::Settings(false));
        let _ = app.update(Message::Settings(true));
        assert_eq!(app.draft.config.outputs, app.config.outputs);
    }
    #[test]
    fn invalid_output_draft_stays_in_settings_without_input_error() {
        let mut app = settings_app();
        let _ = app.update(Message::Output(outputs::Message::Enable(
            outputs::Page::Osc,
            true,
        )));
        let _ = app.update(Message::Apply);
        assert!(app.settings);
        assert!(app.settings_error.is_some());
        assert!(app.error.is_none());
        assert!(!app.config.outputs.osc.enabled);
    }
    #[test]
    fn midi_selections_support_multiple_ports_and_removal() {
        let mut app = settings_app();
        let first = MidiPort {
            id: Some("1".into()),
            name: "First".into(),
        };
        let second = MidiPort {
            id: Some("2".into()),
            name: "Second".into(),
        };
        for port in [first.clone(), second.clone(), first.clone()] {
            let _ = app.update(Message::Output(outputs::Message::MidiPort(port, true)));
        }
        assert_eq!(app.draft.config.outputs.midi.ports.len(), 2);
        let _ = app.update(Message::Output(outputs::Message::MidiPort(first, false)));
        assert_eq!(app.draft.config.outputs.midi.ports, [second]);
        assert!(app.config.outputs.midi.ports.is_empty());
    }
}
