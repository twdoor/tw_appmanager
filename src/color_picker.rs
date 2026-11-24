use std::{cell::Cell, rc::Rc};

use gpui::{
    Bounds, ClipboardItem, Context, DispatchPhase, Entity, EventEmitter, FocusHandle, Hsla,
    IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, Window, canvas, div,
    hsla, prelude::*, px, relative,
};
use gpui_component::{
    ActiveTheme,
    input::{Input, InputEvent, InputState},
};

use crate::widgets::Control;

pub enum PickerEvent {
    Apply(&'static str, Hsla),
    Cancel,
}

pub struct ColorPicker {
    key: &'static str,
    label: &'static str,
    original: Hsla,
    color: Hsla,
    hex: Entity<InputState>,
    valid_hex: bool,
    copied: bool,
    tracks: [Rc<Cell<Bounds<Pixels>>>; 4],
    focus: [FocusHandle; 4],
    dragging: Option<usize>,
}

impl EventEmitter<PickerEvent> for ColorPicker {}

impl ColorPicker {
    pub fn new(
        key: &'static str,
        label: &'static str,
        color: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hex = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("#RRGGBB or #RRGGBBAA")
                .default_value(encode_color(color))
        });
        cx.subscribe_in(&hex, window, |this, input, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value();
                    // Slider updates already hold the exact color. Keep that precision until a
                    // user changes the text, rather than rounding on every drag.
                    if !this.valid_hex || text.as_str() != encode_color(this.color) {
                        if let Some(color) = parse_hex(text.as_str()) {
                            this.color = color;
                            this.valid_hex = true;
                        } else {
                            this.valid_hex = false;
                        }
                        this.copied = false;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => this.apply(cx),
                _ => {}
            }
        })
        .detach();
        Self {
            key,
            label,
            original: color,
            color,
            hex,
            valid_hex: true,
            copied: false,
            tracks: std::array::from_fn(|_| Rc::default()),
            focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            dragging: None,
        }
    }

    pub fn apply(&self, cx: &mut Context<Self>) {
        if self.valid_hex {
            cx.emit(PickerEvent::Apply(self.key, self.color));
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.valid_hex {
            cx.write_to_clipboard(ClipboardItem::new_string(encode_color(self.color)));
            self.copied = true;
            cx.notify();
        }
    }

    fn channel(&self, index: usize) -> f32 {
        [self.color.h, self.color.s, self.color.l, self.color.a][index]
    }

    fn adjust(&mut self, index: usize, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        let value = value.clamp(0., 1.);
        if self.channel(index) == value && self.valid_hex {
            return;
        }
        match index {
            0 => self.color.h = value,
            1 => self.color.s = value,
            2 => self.color.l = value,
            _ => self.color.a = value,
        }
        let hex = encode_color(self.color);
        if self.hex.read(cx).value().as_str() != hex {
            self.hex
                .update(cx, |input, cx| input.set_value(hex, window, cx));
        }
        self.valid_hex = true;
        self.copied = false;
        cx.notify();
    }

    fn adjust_at(&mut self, index: usize, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.tracks[index].get();
        if bounds.size.width > px(0.) {
            self.adjust(index, (x - bounds.left()) / bounds.size.width, window, cx);
        }
    }

    fn track(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.tracks[index].clone();
        let color = self.color;
        let channel = self.channel(index);
        let tone = |s, l| hsla(color.h, s, l, 1.);
        let stops: Vec<_> = match index {
            0 => (0..=6)
                .map(|step| hsla(step as f32 / 6., 1., 0.5, 1.))
                .collect(),
            1 => vec![tone(0., color.l), tone(1., color.l)],
            2 => vec![gpui::black(), tone(color.s, 0.5), gpui::white()],
            _ => vec![tone(color.s, color.l).opacity(0.), tone(color.s, color.l)],
        };
        let ring = cx.theme().ring;
        let base = cx.theme().background;
        div()
            .id(("picker-track", index))
            .relative()
            .w_full()
            .h(px(24.))
            .track_focus(&self.focus[index])
            .cursor_pointer()
            .border_1()
            .rounded(px(3.))
            .border_color(gpui::transparent_black())
            .focus(move |this| this.border_color(ring))
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(7.))
                    .right(px(7.))
                    .h(px(6.))
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .bg(base)
                    .children(stops.windows(2).map(|stops| {
                        div().flex_1().bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(stops[0], 0.),
                            gpui::linear_color_stop(stops[1], 1.),
                        ))
                    })),
            )
            .child(
                div()
                    .absolute()
                    .top(px(4.))
                    .left(px(7.))
                    .right(px(7.))
                    .h(px(14.))
                    .child(
                        canvas(move |layout, _, _| bounds.set(layout), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(channel))
                            .ml(px(-7.))
                            .size(px(14.))
                            .rounded_full()
                            .bg(gpui::white())
                            .border_1()
                            .border_color(gpui::black().opacity(0.45)),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus[index].focus(window);
                    this.dragging = Some(index);
                    this.adjust_at(index, event.position.x, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    let value = match event.keystroke.key.as_str() {
                        "left" | "down" => this.channel(index) - 0.01,
                        "right" | "up" => this.channel(index) + 0.01,
                        "home" => 0.,
                        "end" => 1.,
                        _ => return,
                    };
                    this.adjust(index, value, window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }),
            )
    }
}

impl Render for ColorPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let surface = theme.muted;
        let muted = theme.muted_foreground;
        let error = theme.danger;
        let radius = theme.radius;
        let panel_radius = theme.radius_lg;
        let foreground = theme.foreground;
        let preview = |color, label| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .h(px(42.))
                        .rounded(radius)
                        .bg(color)
                        .border_1()
                        .border_color(foreground.opacity(0.20)),
                )
                .child(div().text_xs().text_color(muted).child(label))
        };
        let controls = ["Hue", "Saturation", "Lightness", "Opacity"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                let value = self.channel(index);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().justify_between().child(label).child(
                        div().text_color(muted).child(if index == 0 {
                            format!("{:.0}°", value * 360.)
                        } else {
                            format!("{:.0}%", value * 100.)
                        }),
                    ))
                    .child(self.track(index, cx))
            })
            .collect::<Vec<_>>();
        let drag = cx.listener(|this, event: &MouseMoveEvent, window, cx| {
            if let Some(index) = this.dragging {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.adjust_at(index, event.position.x, window, cx);
                } else {
                    this.dragging = None;
                }
            }
        });
        let release = cx.listener(|this, event: &MouseUpEvent, _, _| {
            if event.button == MouseButton::Left {
                this.dragging = None;
            }
        });
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.52))
            .id("color-overlay")
            .occlude()
            .child(
                // Continue a slider drag when the pointer crosses another control.
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event, phase, window, cx| {
                            if phase == DispatchPhase::Capture {
                                drag(event, window, cx);
                            }
                        });
                        window.on_mouse_event(move |event, phase, window, cx| {
                            if phase == DispatchPhase::Capture {
                                release(event, window, cx);
                            }
                        });
                    },
                )
                .absolute(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(350.))
                    .max_w(relative(0.92))
                    .p_4()
                    .gap_2()
                    .rounded(panel_radius)
                    .bg(surface)
                    .text_color(foreground)
                    .shadow_lg()
                    .text_sm()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.label),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(preview(self.original, "Current"))
                            .child(preview(self.color, "New")),
                    )
                    .children(controls)
                    .child(
                        Input::new(&self.hex)
                            .w_full()
                            .when(!self.valid_hex, |this| this.border_color(error))
                            .suffix(
                                Control::new("copy-hex")
                                    .compact()
                                    .ghost()
                                    .icon(if self.copied { "check" } else { "copy" })
                                    .tooltip(if self.copied { "Copied" } else { "Copy hex" })
                                    .disabled(!self.valid_hex)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy(cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .when(!self.valid_hex, |this| {
                                this.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(error)
                                        .child("Use 6 or 8 hex digits."),
                                )
                            })
                            .child(
                                Control::new("cancel-color")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Cancel)),
                                    ),
                            )
                            .child(
                                Control::new("apply-color")
                                    .primary()
                                    .label("Use color")
                                    .disabled(!self.valid_hex)
                                    .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                            ),
                    ),
            )
    }
}

pub fn parse_hex(value: &str) -> Option<Hsla> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let color = u32::from_str_radix(digits, 16).ok()?;
    Some(if digits.len() == 6 {
        gpui::rgb(color).into()
    } else {
        gpui::rgba(color).into()
    })
}

pub fn encode_color(color: Hsla) -> String {
    let color = gpui::Rgba::from(color);
    let [r, g, b, a] =
        [color.r, color.g, color.b, color.a].map(|channel| (channel * 255.).round() as u8);
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_color, parse_hex};

    #[test]
    fn hex_colors_round_trip_with_optional_alpha_and_prefix() {
        for (raw, canonical) in [
            ("#fa301d", "#fa301d"),
            ("151C4F80", "#151c4f80"),
            ("#000000", "#000000"),
        ] {
            assert_eq!(encode_color(parse_hex(raw).unwrap()), canonical);
        }
        for invalid in ["", "+12345", "#12345", "#11223Z", "#1122334", "##112233"] {
            assert!(parse_hex(invalid).is_none(), "{invalid}");
        }
    }
}
