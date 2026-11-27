use ely_core::{
    buttons::{Button, ButtonVariant, CopyButton},
    forms::{ColorPalette, ColorPicker as ElyColorPicker, Submit},
    overlays::Dialog,
    typography::Caption,
};
use gpui::{Context, EventEmitter, FocusHandle, Hsla, IntoElement, Render, Window, prelude::*};

pub enum PickerEvent {
    Preview(&'static str, Hsla),
    Apply(&'static str, Hsla),
    Cancel,
}

pub struct ColorPicker {
    key: &'static str,
    label: &'static str,
    color: Hsla,
    palette: Vec<(&'static str, Hsla)>,
    palette_focus: FocusHandle,
}

impl EventEmitter<PickerEvent> for ColorPicker {}

impl ColorPicker {
    pub fn new(
        key: &'static str,
        label: &'static str,
        color: Hsla,
        palette: Vec<(&'static str, Hsla)>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            label,
            color,
            palette,
            palette_focus: cx.focus_handle(),
        }
    }

    pub fn apply(&self, window: &mut Window, cx: &mut Context<Self>) {
        // Ely commits typed fields on Submit or blur. Submit the focused field
        // before reading the draft so clicking Use color also accepts typed edits.
        if let Some(focused) = window.focused(cx) {
            focused.dispatch_action(&Submit, window, cx);
        }
        let picker = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = picker.update(cx, |this, cx| {
                cx.emit(PickerEvent::Apply(this.key, this.color))
            });
        });
    }

    fn select(&mut self, color: Hsla, cx: &mut Context<Self>) {
        if self.color != color {
            self.color = color;
            cx.emit(PickerEvent::Preview(self.key, color));
            cx.notify();
        }
    }
}

impl Render for ColorPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = cx.weak_entity();
        let picker = dialog.clone();
        let palette = dialog.clone();
        let hex = encode_color(self.color);
        let apply = cx.listener(|this, _, window, cx| this.apply(window, cx));

        Dialog::new("color-picker", self.label, move |_, cx| {
            let _ = dialog.update(cx, |_, cx| cx.emit(PickerEvent::Cancel));
        })
        .held()
        .child(
            ElyColorPicker::new("color", self.color).on_change(move |color, _, cx| {
                let _ = picker.update(cx, |this, cx| this.select(color, cx));
            }),
        )
        .child(
            gpui::div()
                .track_focus(&self.palette_focus)
                .flex()
                .flex_col()
                .gap_2()
                .child(Caption::new("Palette colors"))
                .child(
                    ColorPalette::new("palette-colors", self.palette.iter().copied())
                        .columns(12)
                        .selected(self.color)
                        .on_change(move |color, window, cx| {
                            let _ = palette.update(cx, |this, cx| {
                                if !this.palette_focus.contains_focused(window, cx) {
                                    this.palette_focus.focus(window, cx);
                                }
                                this.select(color, cx);
                            });
                        }),
                ),
        )
        .action(move |_| CopyButton::new("copy-color-hex", hex))
        .action(|close| {
            Button::new("cancel-color", "Cancel")
                .variant(ButtonVariant::Ghost)
                .on_click(move |_, window, cx| close(window, cx))
        })
        .action(move |_| {
            Button::new("apply-color", "Use color")
                .primary()
                .on_click(apply)
        })
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
