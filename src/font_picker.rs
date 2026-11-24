use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, Context, Corner, Entity, FocusHandle, IntoElement, MouseButton, Pixels, Render,
    SharedString, Task, Window, anchored, canvas, deferred, div, point, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, IconName, IndexPath,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
};

use crate::widgets::glyph;

const SYSTEM_FONT: &str = "System font";

pub struct FontPicker {
    family: SharedString,
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    list: Option<Entity<ListState<Fonts>>>,
}

impl FontPicker {
    pub fn new(family: SharedString, cx: &mut Context<Self>) -> Self {
        Self {
            family,
            focus: cx.focus_handle().tab_stop(true),
            bounds: Rc::default(),
            list: None,
        }
    }

    pub fn family(&self) -> &str {
        self.family.as_str()
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.list.take().is_some() {
            self.focus.focus(window);
        } else {
            let fonts = Fonts::new(cx, self.family.clone());
            let list = cx.new(|cx| ListState::new(fonts, window, cx).searchable(true));
            cx.subscribe_in(&list, window, |this, list, event, window, cx| {
                match event {
                    ListEvent::Confirm(index) => {
                        let Some(font) = list.read(cx).delegate().get(*index) else {
                            return;
                        };
                        this.family = font.clone();
                    }
                    ListEvent::Cancel => {}
                    ListEvent::Select(_) => return,
                }
                this.list = None;
                this.focus.focus(window);
                cx.notify();
            })
            .detach();
            list.update(cx, |list, cx| list.focus(window, cx));
            self.list = Some(list);
        }
        cx.notify();
    }
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        let border = theme.input;
        let ring = theme.ring;
        let radius = theme.radius;
        let muted = theme.muted_foreground;
        let bounds = self.bounds.clone();
        let family = if self.family.is_empty() {
            SYSTEM_FONT.into()
        } else {
            self.family.clone()
        };

        div()
            .relative()
            .child(
                div()
                    .id("font-family")
                    .w_full()
                    .h_8()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_sm()
                    .bg(background)
                    .rounded(radius)
                    .border_1()
                    .border_color(if self.list.is_some() { ring } else { border })
                    .track_focus(&self.focus)
                    .focus(move |this| this.border_color(ring))
                    .cursor_pointer()
                    .child(div().min_w_0().truncate().child(family))
                    .child(glyph("icons/chevron-down.svg", 14.).text_color(muted))
                    .child(
                        canvas(move |layout, _, _| bounds.set(layout), |_, _, _, _| ())
                            .absolute()
                            .inset_0(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
            )
            .when_some(self.list.as_ref(), |this, list| {
                let height = px(270.);
                let field = self.bounds.get();
                let below = field.bottom() + height + px(12.) <= window.viewport_size().height;
                let (corner, y) = if below {
                    (Corner::TopLeft, field.bottom() + px(4.))
                } else {
                    (Corner::BottomLeft, field.top() - px(4.))
                };
                this.child(
                    deferred(
                        anchored()
                            .anchor(corner)
                            .position(point(field.left(), y))
                            .snap_to_window_with_margin(px(8.))
                            .child(
                                div()
                                    .id("font-list")
                                    .w(field.size.width)
                                    .h(height)
                                    .overflow_hidden()
                                    .bg(background)
                                    .border_1()
                                    .border_color(border)
                                    .rounded(radius)
                                    .shadow_md()
                                    .occlude()
                                    .on_mouse_down_out(cx.listener(
                                        |this, event: &gpui::MouseDownEvent, _, cx| {
                                            if event.button == MouseButton::Left
                                                && !this.bounds.get().contains(&event.position)
                                            {
                                                this.list = None;
                                                cx.notify();
                                            }
                                        },
                                    ))
                                    .child(
                                        List::new(list)
                                            .search_placeholder("Search fonts")
                                            .max_h(height - px(2.)),
                                    ),
                            ),
                    )
                    .with_priority(1),
                )
            })
    }
}

struct Fonts {
    families: Vec<SharedString>,
    matches: Vec<usize>,
    current: SharedString,
}

impl Fonts {
    fn new(cx: &App, current: SharedString) -> Self {
        let mut names = font_kit::source::SystemSource::new()
            .all_families()
            .unwrap_or_else(|_| cx.text_system().all_font_names());
        names.retain(|name| !name.starts_with('.'));
        if !current.is_empty() && !names.iter().any(|name| name == current.as_str()) {
            names.push(current.to_string());
        }
        names.sort_by_cached_key(|name| name.to_lowercase());
        let families: Vec<_> = std::iter::once(SharedString::default())
            .chain(names.into_iter().map(SharedString::from))
            .collect();
        Self {
            matches: (0..families.len()).collect(),
            families,
            current,
        }
    }

    fn get(&self, index: IndexPath) -> Option<&SharedString> {
        self.matches.get(index.row).map(|&row| &self.families[row])
    }
}

impl ListDelegate for Fonts {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matches.len()
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let font = self.get(index)?;
        let name = if font.is_empty() {
            SYSTEM_FONT.into()
        } else {
            font.clone()
        };
        Some(
            ListItem::new(index.row)
                .check_icon(IconName::Check)
                .confirmed(*font == self.current)
                .child(div().min_w_0().truncate().child(name)),
        )
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.matches = self
            .families
            .iter()
            .enumerate()
            .filter_map(|(i, name)| {
                let name = if name.is_empty() {
                    SYSTEM_FONT
                } else {
                    name.as_str()
                };
                name.to_lowercase().contains(&query).then_some(i)
            })
            .collect();
        Task::ready(())
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No matching fonts")
    }
}
