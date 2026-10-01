//! Settings-only controls for independent clock destinations.
use super::{
    ACCENT, App, ERROR, INK, MUTED, Message as AppMessage, PANEL, input_style, medium_font,
    quiet_button, value_button,
};
use iced::{
    Alignment, Border, Element, Fill, Task,
    widget::{button, checkbox, column, container, row, space, text, text_input, toggler},
};
use tempotrack::output::{config::MidiPort, midi};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Page {
    Overview,
    Link,
    Osc,
    Midi,
    RtpMidi,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Clock outputs",
            Self::Link => "Ableton Link",
            Self::Osc => "OSC",
            Self::Midi => "MIDI Clock",
            Self::RtpMidi => "Network MIDI",
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Self::Overview => "",
            Self::Link => "link",
            Self::Osc => "osc:",
            Self::Midi => "midi:",
            Self::RtpMidi => "rtpmidi",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum Message {
    Page(Page),
    Enable(Page, bool),
    AddOsc,
    RemoveOsc(usize),
    OscTarget(usize, String),
    OscPrefix(String),
    RefreshMidi,
    MidiPort(MidiPort, bool),
    VirtualMidi(bool),
    RtpName(String),
    RtpPort(String),
}

impl App {
    pub(super) fn refresh_midi(&mut self) -> Task<AppMessage> {
        self.midi_loading = true;
        Task::perform(
            async { midi::list_ports().map_err(|error| error.to_string()) },
            AppMessage::MidiPorts,
        )
    }
    pub(super) fn update_output(&mut self, message: Message) -> Task<AppMessage> {
        let outputs = &mut self.draft.config.outputs;
        match message {
            Message::Page(page) => {
                self.output_page = page;
                if page == Page::Midi && !self.midi_loading {
                    return self.refresh_midi();
                }
            }
            Message::Enable(page, enabled) => match page {
                Page::Link => outputs.link = enabled,
                Page::Osc => outputs.osc.enabled = enabled,
                Page::Midi => outputs.midi.enabled = enabled,
                Page::RtpMidi => outputs.rtpmidi.enabled = enabled,
                Page::Overview => {}
            },
            Message::AddOsc => outputs.osc.targets.push(String::new()),
            Message::RemoveOsc(index) => {
                if index < outputs.osc.targets.len() {
                    outputs.osc.targets.remove(index);
                }
            }
            Message::OscTarget(index, value) => {
                if let Some(target) = outputs.osc.targets.get_mut(index) {
                    *target = value;
                }
            }
            Message::OscPrefix(value) => outputs.osc.prefix = value,
            Message::RefreshMidi => return self.refresh_midi(),
            Message::MidiPort(port, selected) => {
                outputs.midi.ports.retain(|other| {
                    !same_port(other, &port)
                        && resolve_port(other, &self.midi_ports)
                            .is_none_or(|available| !same_port(available, &port))
                });
                if selected {
                    outputs.midi.ports.push(port);
                }
            }
            Message::VirtualMidi(enabled) => outputs.midi.virtual_port = enabled,
            Message::RtpName(value) => outputs.rtpmidi.name = value,
            Message::RtpPort(value) => self.draft.rtp_port = value,
        }
        self.settings_error = None;
        Task::none()
    }
    pub(super) fn outputs_view(&self) -> iced::widget::Column<'_, AppMessage> {
        let outputs = &self.draft.config.outputs;
        if self.output_page == Page::Overview {
            let mut body = column![
                text("Send your audio-derived tempo to apps and devices.")
                    .size(11)
                    .color(MUTED),
            ]
            .spacing(8);
            for (page, enabled, hint) in [
                (Page::Link, outputs.link, "Resolume and other Link apps"),
                (Page::Osc, outputs.osc.enabled, "Tempo and pulses over UDP"),
                (
                    Page::Midi,
                    outputs.midi.enabled,
                    "Local ports and virtual MIDI",
                ),
                (
                    Page::RtpMidi,
                    outputs.rtpmidi.enabled,
                    "RTP-MIDI over your network",
                ),
            ] {
                let status = self.output_summary(page, enabled);
                body = body.push(
                    container(
                        row![
                            button(
                                column![
                                    text(page.title()).size(12).font(medium_font()).color(INK),
                                    text(hint).size(9).color(MUTED),
                                    text(status.0).size(9).color(status.1),
                                ]
                                .spacing(3)
                            )
                            .padding(0)
                            .width(Fill)
                            .on_press(AppMessage::Output(Message::Page(page)))
                            .style(value_button),
                            toggler(enabled).size(19).on_toggle(move |enabled| {
                                AppMessage::Output(Message::Enable(page, enabled))
                            }),
                            button(text("›").size(18).color(MUTED))
                                .padding([4, 5])
                                .on_press(AppMessage::Output(Message::Page(page)))
                                .style(value_button)
                        ]
                        .spacing(9)
                        .align_y(Alignment::Center),
                    )
                    .padding(7)
                    .style(|_| container::Style {
                        background: Some(PANEL.into()),
                        border: Border {
                            radius: 7.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                );
            }
            return body;
        }

        let page = self.output_page;
        let enabled = match page {
            Page::Link => outputs.link,
            Page::Osc => outputs.osc.enabled,
            Page::Midi => outputs.midi.enabled,
            Page::RtpMidi => outputs.rtpmidi.enabled,
            Page::Overview => unreachable!(),
        };
        let mut body = column![
            row![
                button(text("‹ Outputs").size(10))
                    .padding([3, 0])
                    .on_press(AppMessage::Output(Message::Page(Page::Overview)))
                    .style(value_button),
                space::horizontal(),
                toggler(enabled)
                    .label("Enabled")
                    .text_size(10)
                    .size(19)
                    .on_toggle(move |enabled| AppMessage::Output(Message::Enable(page, enabled))),
            ]
            .align_y(Alignment::Center),
            text(page.title()).size(17).font(medium_font()),
        ]
        .spacing(10);
        match page {
            Page::Link => {
                body = body.push(
                    text("Enable Link in Resolume or another app on the same network. TempoTrack publishes tempo and beat alignment from your audio.")
                        .size(11).color(MUTED)
                );
            }
            Page::Osc => {
                body = body
                    .push(
                        text("Add each receiver’s host and UDP port.")
                            .size(11)
                            .color(MUTED),
                    )
                    .push(caption("DESTINATIONS"));
                for (index, target) in outputs.osc.targets.iter().enumerate() {
                    body = body.push(
                        row![
                            text_input("192.168.1.20:7000", target)
                                .size(11)
                                .padding([7, 8])
                                .style(input_style)
                                .on_input(move |value| AppMessage::Output(Message::OscTarget(
                                    index, value
                                ))),
                            button(text("Remove").size(10))
                                .padding([7, 6])
                                .style(quiet_button)
                                .on_press(AppMessage::Output(Message::RemoveOsc(index)))
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    );
                }
                body = body
                    .push(button(text("+ Add destination").size(11))
                        .padding([5, 8]).style(quiet_button)
                        .on_press(AppMessage::Output(Message::AddOsc)))
                    .push(caption("ADDRESS PREFIX"))
                    .push(text_input("/tempotrack", &outputs.osc.prefix)
                        .size(11).padding([7, 8]).style(input_style)
                        .on_input(|value| AppMessage::Output(Message::OscPrefix(value))))
                    .push(text("Sends /bpm, /phase, /beat, /bar, /atom and /active. Configure your receiver’s OSC mapping to match.")
                        .size(10).color(MUTED));
            }
            Page::Midi => {
                body = body.push(
                    row![
                        caption("OUTPUT PORTS"),
                        space::horizontal(),
                        button(
                            text(if self.midi_loading {
                                "Refreshing…"
                            } else {
                                "Refresh"
                            })
                            .size(10)
                        )
                        .padding([3, 6])
                        .style(quiet_button)
                        .on_press_maybe(
                            (!self.midi_loading)
                                .then_some(AppMessage::Output(Message::RefreshMidi))
                        )
                    ]
                    .align_y(Alignment::Center),
                );
                for port in &self.midi_ports {
                    body = body.push(self.midi_port_control(port, false));
                }
                // Keep disconnected choices visible so users can deselect them.
                for port in &outputs.midi.ports {
                    if resolve_port(port, &self.midi_ports).is_none() {
                        body = body.push(self.midi_port_control(port, true));
                    }
                }
                if self.midi_ports.is_empty() && !self.midi_loading {
                    body = body.push(
                        text("No hardware MIDI outputs found.")
                            .size(10)
                            .color(MUTED),
                    );
                }
                if let Some(error) = &self.midi_error {
                    body = body.push(text(error).size(10).color(ERROR));
                }
                body = body
                    .push(checkbox(outputs.midi.virtual_port)
                        .label("TempoTrack Clock · virtual port")
                        .size(15).text_size(11)
                        .on_toggle(|enabled| AppMessage::Output(Message::VirtualMidi(enabled))))
                    .push(text("Select any number of destinations. Sends clock ticks at 24 PPQN; no playback commands.")
                        .size(10).color(MUTED));
            }
            Page::RtpMidi => {
                body = body
                    .push(text("Discoverable RTP-MIDI session. Connect to it from your receiver’s network MIDI setup.")
                        .size(11).color(MUTED))
                    .push(caption("SESSION NAME"))
                    .push(text_input("TempoTrack", &outputs.rtpmidi.name)
                        .size(11).padding([7, 8]).style(input_style)
                        .on_input(|value| AppMessage::Output(Message::RtpName(value))))
                    .push(caption("CONTROL PORT"))
                    .push(text_input("5004", &self.draft.rtp_port)
                        .size(11).padding([7, 8]).style(input_style)
                        .on_input(|value| AppMessage::Output(Message::RtpPort(value))))
                    .push(text("The next port carries MIDI data. Clock is sent to every connected participant.")
                        .size(10).color(MUTED));
            }
            Page::Overview => unreachable!(),
        }
        body = body.push(caption("STATUS"));
        let statuses: Vec<_> = self
            .output_status
            .iter()
            .filter(|status| status.id.starts_with(page.prefix()))
            .collect();
        if statuses.is_empty() {
            body = body.push(
                text(if enabled {
                    "Apply settings to enable this output."
                } else {
                    "Disabled"
                })
                .size(10)
                .color(MUTED),
            );
        } else {
            for status in statuses {
                let label = if let Some(target) = status.id.strip_prefix("osc:") {
                    Some(target.to_owned())
                } else {
                    status.id.strip_prefix("midi:").map(|id| {
                        if id == "virtual" {
                            "TempoTrack Clock · virtual".to_owned()
                        } else {
                            self.config
                                .outputs
                                .midi
                                .ports
                                .iter()
                                .find(|port| port.id.as_deref() == Some(id) || port.name == id)
                                .map_or_else(|| id.to_owned(), |port| port.name.clone())
                        }
                    })
                };
                let mut detail = column![].spacing(3);
                if let Some(label) = label {
                    detail = detail.push(text(label).size(10).color(INK));
                }
                detail = detail.push(text(&status.detail).size(10).color(if status.failed {
                    ERROR
                } else {
                    MUTED
                }));
                body = body.push(detail);
            }
        }
        if outputs != &self.config.outputs {
            body = body.push(
                text("Changes take effect when you apply settings.")
                    .size(10)
                    .color(ACCENT),
            );
        }
        body
    }
    fn output_summary(&self, page: Page, enabled: bool) -> (String, iced::Color) {
        let draft = &self.draft.config.outputs;
        let configured = &self.config.outputs;
        let pending = match page {
            Page::Link => draft.link != configured.link,
            Page::Osc => draft.osc != configured.osc,
            Page::Midi => draft.midi != configured.midi,
            Page::RtpMidi => draft.rtpmidi != configured.rtpmidi,
            Page::Overview => false,
        };
        if pending {
            return ("Pending Apply".into(), ACCENT);
        }
        if !enabled {
            return ("Disabled".into(), MUTED);
        }
        let statuses: Vec<_> = self
            .output_status
            .iter()
            .filter(|status| status.id.starts_with(page.prefix()))
            .collect();
        if statuses.iter().any(|status| status.failed) {
            ("Connection needs attention".into(), ERROR)
        } else if statuses.len() == 1 {
            // The detailed status remains available on the configuration page.
            let detail = &statuses[0].detail;
            let short: String = detail.chars().take(37).collect();
            (
                if detail.chars().count() > 37 {
                    format!("{short}…")
                } else {
                    short
                },
                MUTED,
            )
        } else if statuses.is_empty() {
            ("Starting…".into(), MUTED)
        } else {
            (format!("{} destinations", statuses.len()), MUTED)
        }
    }
    fn midi_port_control(&self, port: &MidiPort, missing: bool) -> Element<'_, AppMessage> {
        let selected = self
            .draft
            .config
            .outputs
            .midi
            .ports
            .iter()
            .any(|selection| {
                if missing {
                    same_port(selection, port)
                } else {
                    resolve_port(selection, &self.midi_ports)
                        .is_some_and(|available| same_port(available, port))
                }
            });
        let label = if missing {
            format!("{} · unavailable", port.name)
        } else if self
            .midi_ports
            .iter()
            .filter(|other| other.name == port.name)
            .count()
            > 1
        {
            format!(
                "{} · {}",
                port.name,
                port.id.as_deref().unwrap_or("unknown ID")
            )
        } else {
            port.name.clone()
        };
        let port = port.clone();
        checkbox(selected)
            .label(label)
            .size(15)
            .text_size(11)
            .on_toggle(move |selected| {
                AppMessage::Output(Message::MidiPort(port.clone(), selected))
            })
            .into()
    }
}

fn caption(label: &'static str) -> Element<'static, AppMessage> {
    text(label).size(9).color(MUTED).into()
}
fn same_port(a: &MidiPort, b: &MidiPort) -> bool {
    match (&a.id, &b.id) {
        (Some(a), Some(b)) => a == b,
        _ => a.name == b.name,
    }
}

// Match the connector's ID-first, unambiguous-name fallback so refreshed ports
// do not appear disconnected when the operating system has reassigned IDs.
fn resolve_port<'a>(selection: &MidiPort, ports: &'a [MidiPort]) -> Option<&'a MidiPort> {
    if let Some(id) = &selection.id
        && let Some(port) = ports.iter().find(|port| port.id.as_ref() == Some(id))
    {
        return Some(port);
    }
    let mut names = ports.iter().filter(|port| port.name == selection.name);
    match (names.next(), names.next()) {
        (Some(port), None) => Some(port),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refreshed_ports_keep_unique_names_but_never_guess_ambiguous_names() {
        let selection = MidiPort {
            id: Some("old".into()),
            name: "Output".into(),
        };
        let first = MidiPort {
            id: Some("new".into()),
            name: "Output".into(),
        };
        let second = MidiPort {
            id: Some("second".into()),
            name: "Output".into(),
        };
        assert_eq!(
            resolve_port(&selection, std::slice::from_ref(&first)),
            Some(&first)
        );
        assert!(resolve_port(&selection, &[first.clone(), second.clone()]).is_none());
        assert_eq!(resolve_port(&first, &[first.clone(), second]), Some(&first));
    }
}
