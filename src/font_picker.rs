use ely_core::forms::{Choice, Combobox, InputEvent, TextInput};
use gpui::{Context, Entity, EventEmitter, Render, SharedString, Window, prelude::*};

const SYSTEM_FONT: &str = "System font";

pub struct FontChanged(pub SharedString);

impl EventEmitter<FontChanged> for FontPicker {}

pub struct FontPicker {
    family: SharedString,
    input: Entity<TextInput>,
    choices: Vec<Choice>,
}

impl FontPicker {
    pub fn new(family: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut names = font_kit::source::SystemSource::new()
            .all_families()
            .unwrap_or_else(|_| cx.text_system().all_font_names());
        names.retain(|name| !name.starts_with('.'));
        if !family.is_empty() && !names.iter().any(|name| name == family.as_str()) {
            names.push(family.to_string());
        }
        names.sort_by_cached_key(|name| name.to_lowercase());

        let choices = std::iter::once(Choice::new("", SYSTEM_FONT))
            .chain(
                names
                    .into_iter()
                    .map(|name| Choice::new(name.clone(), name)),
            )
            .collect();
        let input = cx.new(|cx| TextInput::new(window, cx).label("Font family"));
        cx.subscribe(&input, |_, input, event, cx| {
            if *event == InputEvent::Focus {
                input.update(cx, |input, cx| input.select(0..input.text().len(), cx));
            }
            cx.notify();
        })
        .detach();
        Self {
            family,
            input,
            choices,
        }
    }

    pub fn set_disabled(&self, disabled: bool, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_disabled(disabled, cx));
    }
}

impl Render for FontPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = cx.weak_entity();
        Combobox::new("font-family", &self.input, self.choices.iter().cloned())
            .selected(self.family.clone())
            .on_change(move |family, _, cx| {
                let _ = picker.update(cx, |this, cx| {
                    if this.family != *family {
                        this.family = family.clone();
                        cx.emit(FontChanged(family.clone()));
                        cx.notify();
                    }
                });
            })
    }
}
