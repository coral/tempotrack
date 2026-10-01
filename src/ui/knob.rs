//! A normalized, relative-drag parameter control. Mapping and gestures are independent
//! of rendering, so the same component works for linear and logarithmic parameters.
use iced::advanced::Renderer as _;
use iced::advanced::graphics::geometry::Renderer as _;
use iced::{
    Element, Event, Length, Point, Radians, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, renderer,
        widget::{self, Tree, tree},
    },
    keyboard::{self, Key, key::Named},
    widget::canvas::{self, Frame, Path, Stroke},
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub enum Unit {
    Decibels,
    Milliseconds,
}
#[derive(Debug, Clone, Copy)]
pub struct Parameter {
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub logarithmic: bool,
    pub bipolar: bool,
    pub unit: Unit,
}
impl Parameter {
    pub fn normalize(self, value: f64) -> f64 {
        let value = value.clamp(self.min, self.max);
        if self.logarithmic {
            (value / self.min).ln() / (self.max / self.min).ln()
        } else {
            (value - self.min) / (self.max - self.min)
        }
    }
    pub fn plain(self, normal: f64) -> f64 {
        let n = normal.clamp(0., 1.);
        if self.logarithmic {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        }
    }
    pub fn format(self, value: f64) -> String {
        match self.unit {
            Unit::Decibels if self.bipolar => format!("{value:+.1} dB"),
            Unit::Decibels => format!("{value:.1} dB"),
            Unit::Milliseconds if self.bipolar => format!("{value:+.0} ms"),
            Unit::Milliseconds if value >= 1000. => format!("{:.2} s", value / 1000.),
            Unit::Milliseconds => format!("{value:.0} ms"),
        }
    }
    pub fn parse(self, input: &str) -> Option<f64> {
        let text = input.trim().to_ascii_lowercase();
        let (number, multiplier) = match self.unit {
            Unit::Decibels => (
                text.strip_suffix("dbfs")
                    .or_else(|| text.strip_suffix("db"))
                    .unwrap_or(&text),
                1.,
            ),
            Unit::Milliseconds => {
                if let Some(s) = text.strip_suffix("ms") {
                    (s, 1.)
                } else if let Some(s) = text.strip_suffix('s') {
                    (s, 1000.)
                } else {
                    (text.as_str(), 1.)
                }
            }
        };
        let value = number.trim().parse::<f64>().ok()? * multiplier;
        value.is_finite().then(|| value.clamp(self.min, self.max))
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Edit {
    Begin,
    Change(f64),
    End,
    EnterValue,
}

#[derive(Debug, Default)]
struct Drag {
    value: f64,
    previous_y: f32,
    start_y: f32,
    active: bool,
    moved: bool,
}
impl Drag {
    fn begin(&mut self, value: f64, y: f32) {
        *self = Self {
            value,
            previous_y: y,
            start_y: y,
            active: true,
            moved: false,
        };
    }
    fn motion(&mut self, y: f32, fine: bool) -> Option<f64> {
        if !self.active {
            return None;
        }
        if !self.moved && (y - self.start_y).abs() < 3. {
            return None;
        }
        self.moved = true;
        let delta = f64::from(self.previous_y - y) / 220. * if fine { 0.1 } else { 1. };
        self.previous_y = y;
        self.value = (self.value + delta).clamp(0., 1.);
        Some(self.value)
    }
    fn end(&mut self) -> bool {
        std::mem::take(&mut self.active)
    }
}
#[derive(Default)]
struct State {
    drag: Drag,
    focused: bool,
    modifiers: keyboard::Modifiers,
    last_click: Option<(Instant, Point)>,
    finger: Option<iced::touch::Finger>,
}
impl widget::operation::Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}
pub struct Knob<'a, Message> {
    parameter: Parameter,
    value: f64,
    on_edit: Box<dyn Fn(Edit) -> Message + 'a>,
    id: widget::Id,
}
impl<'a, Message> Knob<'a, Message> {
    pub fn new(parameter: Parameter, value: f64, on_edit: impl Fn(Edit) -> Message + 'a) -> Self {
        Self {
            parameter,
            value,
            on_edit: Box::new(on_edit),
            id: widget::Id::new(parameter.label),
        }
    }
}
impl<Message> Knob<'_, Message> {
    fn handle(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) {
        let inside = cursor.is_over(bounds);
        let mut publish = |edit| shell.publish((self.on_edit)(edit));
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers
            }
            Event::Window(iced::window::Event::Unfocused) => {
                if state.drag.end() {
                    publish(Edit::End);
                }
                state.focused = false;
                state.finger = None;
                state.modifiers = keyboard::Modifiers::default();
                state.last_click = None;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if !inside {
                    state.focused = false;
                    return;
                }
                if state.drag.active {
                    return;
                }
                let Some(position) = cursor.position() else {
                    return;
                };
                state.focused = true;
                let now = Instant::now();
                let double = state.last_click.is_some_and(|(time, p)| {
                    now.duration_since(time) < Duration::from_millis(400)
                        && p.distance(position) < 6.
                });
                if double {
                    if state.drag.end() {
                        publish(Edit::End);
                    }
                    publish(Edit::Begin);
                    publish(Edit::Change(self.parameter.default));
                    publish(Edit::End);
                    state.last_click = None;
                } else {
                    state.last_click = Some((now, position));
                    state
                        .drag
                        .begin(self.parameter.normalize(self.value), position.y);
                    publish(Edit::Begin);
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::CursorMoved { position })
                if state.drag.active && state.finger.is_none() =>
            {
                if let Some(value) = state.drag.motion(position.y, state.modifiers.shift()) {
                    publish(Edit::Change(self.parameter.plain(value)));
                    state.last_click = None;
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.finger.is_none() =>
            {
                if state.drag.end() {
                    publish(Edit::End);
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if inside && !state.drag.active => {
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => f64::from(*y),
                    mouse::ScrollDelta::Pixels { y, .. } => f64::from(*y) / 40.,
                };
                let step = if state.modifiers.shift() { 0.001 } else { 0.01 };
                publish(Edit::Begin);
                publish(Edit::Change(
                    self.parameter
                        .plain(self.parameter.normalize(self.value) + lines * step),
                ));
                publish(Edit::End);
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if state.focused =>
            {
                if *key == Key::Named(Named::Escape) {
                    if state.drag.end() {
                        publish(Edit::End);
                    }
                    state.finger = None;
                    state.last_click = None;
                } else if !state.drag.active {
                    let step = if modifiers.shift() { 0.001 } else { 0.01 };
                    let value = self.parameter.normalize(self.value);
                    let new_value = match key {
                        Key::Named(Named::ArrowUp | Named::ArrowRight) => Some(value + step),
                        Key::Named(Named::ArrowDown | Named::ArrowLeft) => Some(value - step),
                        Key::Named(Named::PageUp) => Some(value + step * 10.),
                        Key::Named(Named::PageDown) => Some(value - step * 10.),
                        Key::Named(Named::Home) => Some(0.),
                        Key::Named(Named::End) => Some(1.),
                        Key::Named(Named::Enter) => {
                            publish(Edit::EnterValue);
                            Some(value)
                        }
                        _ => None,
                    };
                    if let Some(value) = new_value {
                        if *key != Key::Named(Named::Enter) {
                            publish(Edit::Begin);
                            publish(Edit::Change(self.parameter.plain(value)));
                            publish(Edit::End);
                        }
                    } else {
                        return;
                    }
                } else {
                    return;
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Touch(iced::touch::Event::FingerPressed { id, position })
                if bounds.contains(*position) && !state.drag.active =>
            {
                state.focused = true;
                state.finger = Some(*id);
                state
                    .drag
                    .begin(self.parameter.normalize(self.value), position.y);
                publish(Edit::Begin);
                shell.capture_event();
            }
            Event::Touch(iced::touch::Event::FingerMoved { id, position })
                if state.finger == Some(*id) =>
            {
                if let Some(value) = state.drag.motion(position.y, false) {
                    publish(Edit::Change(self.parameter.plain(value)));
                }
                shell.capture_event();
            }
            Event::Touch(
                iced::touch::Event::FingerLifted { id, .. }
                | iced::touch::Event::FingerLost { id, .. },
            ) if state.finger == Some(*id) => {
                if state.drag.end() {
                    publish(Edit::End);
                }
                state.finger = None;
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft) => {
                shell.request_redraw()
            }
            _ => {}
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Knob<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(68.), Length::Fixed(62.))
    }
    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(68., 62., Size::ZERO))
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        operation.focusable(
            Some(&self.id),
            layout.bounds(),
            tree.state.downcast_mut::<State>(),
        );
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        self.handle(
            tree.state.downcast_mut::<State>(),
            event,
            layout.bounds(),
            cursor,
            shell,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) || tree.state.downcast_ref::<State>().drag.active {
            mouse::Interaction::ResizingVertically
        } else {
            mouse::Interaction::default()
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let hover = cursor.is_over(bounds);
        let active = state.drag.active && state.drag.moved;
        let normal = self.parameter.normalize(self.value) as f32;
        let mut frame = Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2., 31.);
        let angle = |n: f32| (-225. + 270. * n).to_radians();
        let arc = |from: f32, to: f32| {
            Path::new(|p| {
                p.arc(canvas::path::Arc {
                    center,
                    radius: 25.,
                    start_angle: Radians(angle(from)),
                    end_angle: Radians(angle(to)),
                })
            })
        };
        frame.stroke(
            &arc(0., 1.),
            Stroke::default().with_width(2.5).with_color(super::LINE),
        );
        let origin = if self.parameter.bipolar {
            self.parameter.normalize(0.) as f32
        } else {
            0.
        };
        let accent = if active { super::INK } else { super::ACCENT };
        if (normal - origin).abs() > 0.0001 {
            frame.stroke(
                &arc(origin.min(normal), origin.max(normal)),
                Stroke::default().with_width(2.5).with_color(accent),
            );
        }
        frame.fill(&Path::circle(center, 19.5), super::PANEL);
        frame.stroke(
            &Path::circle(center, 19.5),
            Stroke::default()
                .with_width(1.)
                .with_color(if state.focused || hover {
                    super::MUTED
                } else {
                    super::LINE
                }),
        );
        let point = |radius: f32| {
            Point::new(
                center.x + angle(normal).cos() * radius,
                center.y + angle(normal).sin() * radius,
            )
        };
        frame.stroke(
            &Path::line(point(9.), point(15.5)),
            Stroke::default()
                .with_width(2.5)
                .with_color(if active || hover {
                    super::INK
                } else {
                    super::ACCENT
                }),
        );
        if self.parameter.bipolar {
            frame.stroke(
                &Path::line(Point::new(center.x, 1.), Point::new(center.x, 4.)),
                Stroke::default().with_width(1.5).with_color(super::MUTED),
            );
        }
        let geometry = frame.into_geometry();
        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            renderer.draw_geometry(geometry)
        });
    }
}
impl<'a, Message: 'a> From<Knob<'a, Message>> for Element<'a, Message> {
    fn from(knob: Knob<'a, Message>) -> Self {
        Self::new(knob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drag_is_relative_fine_continuous_and_clamped() {
        let mut a = Drag::default();
        let mut b = Drag::default();
        a.begin(0.5, 30.);
        b.begin(0.5, 100.);
        assert_eq!(a.motion(30., false), None);
        assert_eq!(a.motion(29., false), None);
        assert_eq!(a.motion(8., false), Some(0.6));
        assert_eq!(b.motion(78., false), Some(0.6));
        assert!((a.motion(-14., true).unwrap() - 0.61).abs() < 1e-10);
        assert!((a.motion(-36., false).unwrap() - 0.71).abs() < 1e-10);
        assert_eq!(a.motion(-10000., false), Some(1.));
        assert!(a.motion(-9999., true).unwrap() < 1.);
        assert!(a.end());
        assert!(!a.end());
        assert_eq!(a.motion(0., false), None);
    }
    #[test]
    fn mapping_units_and_validation() {
        let p = Parameter {
            label: "Hold",
            min: 10.,
            max: 10000.,
            default: 500.,
            logarithmic: true,
            bipolar: false,
            unit: Unit::Milliseconds,
        };
        for value in [10., 500., 10000.] {
            assert!((p.plain(p.normalize(value)) - value).abs() < 1e-8);
        }
        assert_eq!(p.parse("0.2s"), Some(200.));
        assert_eq!(p.parse("-42 ms"), Some(10.));
        assert_eq!(p.parse("20s"), Some(10000.));
        for invalid in ["NaN", "inf", "oops", "2Hz"] {
            assert_eq!(p.parse(invalid), None);
        }
        let gain = Parameter {
            label: "Gain",
            min: -24.,
            max: 24.,
            default: 0.,
            logarithmic: false,
            bipolar: true,
            unit: Unit::Decibels,
        };
        assert_eq!(gain.normalize(0.), 0.5);
        assert_eq!(gain.parse("+3dB"), Some(3.));
        assert_eq!(gain.plain(0.), -24.);
        assert_eq!(gain.plain(1.), 24.);
    }
    fn test_knob() -> Knob<'static, Edit> {
        Knob::new(
            Parameter {
                label: "Test",
                min: 0.,
                max: 1.,
                default: 0.25,
                logarithmic: false,
                bipolar: false,
                unit: Unit::Decibels,
            },
            0.5,
            |e| e,
        )
    }
    fn handle(
        knob: &Knob<'_, Edit>,
        state: &mut State,
        event: Event,
        cursor: mouse::Cursor,
    ) -> Vec<Edit> {
        let mut messages = vec![];
        knob.handle(
            state,
            &event,
            Rectangle::new(Point::ORIGIN, Size::new(68., 62.)),
            cursor,
            &mut Shell::new(&mut messages),
        );
        messages
    }
    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }
    fn at(y: f32) -> mouse::Cursor {
        mouse::Cursor::Available(Point::new(30., y))
    }
    #[test]
    fn events_keep_one_gesture_outside_bounds_and_cancel_on_deactivation() {
        let knob = test_knob();
        let mut state = State::default();
        assert_eq!(handle(&knob, &mut state, press(), at(30.)), [Edit::Begin]);
        assert_eq!(
            handle(
                &knob,
                &mut state,
                Event::Mouse(mouse::Event::CursorMoved {
                    position: Point::new(600., -14.)
                }),
                mouse::Cursor::Unavailable
            ),
            [Edit::Change(0.7)]
        );
        assert_eq!(
            handle(
                &knob,
                &mut state,
                Event::Window(iced::window::Event::Unfocused),
                mouse::Cursor::Unavailable
            ),
            [Edit::End]
        );
        assert!(handle(&knob, &mut state, release(), at(30.)).is_empty());
        assert!(!state.drag.active);
        assert!(!state.focused);
    }
    #[test]
    fn double_click_resets_and_right_click_is_inert() {
        let knob = test_knob();
        let mut state = State::default();
        assert_eq!(handle(&knob, &mut state, press(), at(30.)), [Edit::Begin]);
        assert_eq!(handle(&knob, &mut state, release(), at(30.)), [Edit::End]);
        assert_eq!(
            handle(&knob, &mut state, press(), at(30.)),
            [Edit::Begin, Edit::Change(0.25), Edit::End]
        );
        assert!(handle(&knob, &mut state, release(), at(30.)).is_empty());
        assert!(
            handle(
                &knob,
                &mut state,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
                at(30.)
            )
            .is_empty()
        );
    }
    #[test]
    fn pixel_wheel_preserves_fractional_motion_and_shift_precision() {
        let knob = test_knob();
        let mut state = State::default();
        let event = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0., y: 0.5 },
        });
        assert_eq!(
            handle(&knob, &mut state, event.clone(), at(30.)),
            [Edit::Begin, Edit::Change(0.500125), Edit::End]
        );
        handle(
            &knob,
            &mut state,
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::SHIFT,
            )),
            at(30.),
        );
        assert_eq!(
            handle(&knob, &mut state, event, at(30.)),
            [Edit::Begin, Edit::Change(0.5000125), Edit::End]
        );
    }
    #[test]
    fn touch_tracks_only_owning_finger_and_lost_finger_ends_gesture() {
        use iced::touch::{Event as Touch, Finger};
        let knob = test_knob();
        let mut state = State::default();
        let position = Point::new(30., 30.);
        assert_eq!(
            handle(
                &knob,
                &mut state,
                Event::Touch(Touch::FingerPressed {
                    id: Finger(1),
                    position
                }),
                mouse::Cursor::Unavailable
            ),
            [Edit::Begin]
        );
        assert!(
            handle(
                &knob,
                &mut state,
                Event::Touch(Touch::FingerMoved {
                    id: Finger(2),
                    position: Point::ORIGIN
                }),
                mouse::Cursor::Unavailable
            )
            .is_empty()
        );
        assert_eq!(
            handle(
                &knob,
                &mut state,
                Event::Touch(Touch::FingerLost {
                    id: Finger(1),
                    position
                }),
                mouse::Cursor::Unavailable
            ),
            [Edit::End]
        );
        assert_eq!(state.finger, None);
    }
}
