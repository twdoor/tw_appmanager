use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext, Application, Bounds, Context, Entity, ImageSource, IntoElement,
    PathPromptOptions, Render, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, relative, size,
};
use gpui_component::{
    Root, Theme,
    input::{Enter, Input, InputEvent, InputState},
};

use crate::{
    appearance::Appearance,
    appimage::detect_executable,
    color_picker::{ColorPicker, PickerEvent, encode_color, parse_hex},
    config::{Preferences, View},
    desktop::ID,
    font_picker::FontPicker,
    library::{Draft, Entry, Kind, Settings, Store, Target},
    widgets::{Control, Glyphs, app_icon, glyph},
};

pub fn run(store: Store, paths: Vec<PathBuf>) -> Result<()> {
    let startup_error = Rc::new(RefCell::new(None));
    let window_error = startup_error.clone();
    Application::new()
        .with_assets(Glyphs)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            cx.bind_keys([gpui::KeyBinding::new(
                "shift-enter",
                Enter { secondary: false },
                Some("DescriptionEditor > Input"),
            )]);
            let bounds = Bounds::centered(None, size(px(840.), px(700.)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(660.), px(480.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("TW App Manager".into()),
                        ..TitlebarOptions::default()
                    }),
                    app_id: Some(ID.into()),
                    ..WindowOptions::default()
                },
                |window, cx| {
                    window.set_window_title("TW App Manager");
                    let view = cx.new(|cx| Manager::new(store, paths.into(), window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            match window {
                Ok(_) => {
                    cx.on_window_closed(|cx| {
                        if cx.windows().is_empty() {
                            cx.quit();
                        }
                    })
                    .detach();
                    cx.activate(true);
                }
                Err(error) => {
                    *window_error.borrow_mut() = Some(error);
                    cx.quit();
                }
            }
        });
    if let Some(error) = startup_error.borrow_mut().take() {
        return Err(error);
    }
    Ok(())
}

enum Page {
    Library,
    Edit(Box<Form>),
    Settings(Box<SettingsForm>),
    Remove(Box<Form>),
}

struct Manager {
    store: Store,
    apps: Vec<Entry>,
    page: Page,
    filter: Option<Kind>,
    search: Entity<InputState>,
    queue: VecDeque<PathBuf>,
    toast: Option<Toast>,
    preferences: Preferences,
    sidebar_drag: Option<(gpui::Pixels, f32)>,
    sidebar_animation: Option<SidebarAnimation>,
    focus: gpui::FocusHandle,
    scroll: gpui::ScrollHandle,
    scroll_motion: Option<ScrollMotion>,
    scroll_frame_pending: bool,
    color_picker: Option<Entity<ColorPicker>>,
    busy: bool,
    appearance: Appearance,
    appearance_error: Option<String>,
}

impl Manager {
    fn new(
        store: Store,
        queue: VecDeque<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = text_input("", "Search apps", window, cx);
        Self::commit_input(&search, window, cx);
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let path = store.config_path();
        let system_font = Theme::global(cx).font_family.clone();
        let (appearance, appearance_error) = match Appearance::load(&path) {
            Ok(appearance) => (appearance, None),
            Err(error) => (Appearance::defaults(), Some(format!("{error:#}"))),
        };
        appearance.apply(&system_font, window, cx);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            loop {
                // Rescan includes so atomic replacements and newly generated files are visible.
                executor.timer(Duration::from_millis(600)).await;
                let path = path.clone();
                let result = executor.spawn(async move { Appearance::load(&path) }).await;
                if view
                    .update_in(cx, |this, window, cx| match result {
                        Ok(appearance) => {
                            let updated = this.appearance != appearance;
                            if updated {
                                appearance.apply(&system_font, window, cx);
                                this.appearance = appearance;
                            }
                            if this.appearance_error.take().is_some() || updated {
                                cx.refresh_windows();
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            let message = format!("{error:#}");
                            if this.appearance_error.as_ref() != Some(&message) {
                                this.toast =
                                    Some(Toast::new(format!("Appearance: {message}"), true));
                                this.appearance_error = Some(message);
                                cx.notify();
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let preferences = store.preferences().unwrap_or_default();
        let clock = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            let mut interval = Duration::from_millis(180);
            loop {
                clock.timer(interval).await;
                let Ok(active) = view.update_in(cx, |this, _, cx| {
                    let animating = this.sidebar_animation.is_some();
                    if let Some(animation) = &this.sidebar_animation {
                        if animation.finished() {
                            this.sidebar_animation = None;
                        }
                        cx.notify();
                    }
                    let Some(toast) = &mut this.toast else {
                        return animating;
                    };
                    if toast.paused {
                        return animating;
                    }
                    let now = Instant::now();
                    toast.remaining = toast.remaining.saturating_sub(now - toast.updated);
                    toast.updated = now;
                    if toast.remaining.is_zero() {
                        this.toast = None;
                    }
                    cx.notify();
                    animating || this.toast.is_some()
                }) else {
                    break;
                };
                interval = Duration::from_millis(if active { 16 } else { 48 });
            }
        })
        .detach();
        let view = cx.weak_entity();
        cx.intercept_keystrokes(move |event, window, cx| {
            let key = &event.keystroke;
            let mods = key.modifiers;
            let shortcut = mods.control
                && !mods.alt
                && !mods.shift
                && !mods.platform
                && matches!(key.key.as_str(), "b" | "f" | "n" | "enter");
            let escape = !mods.modified()
                && key.key == "escape"
                && !event
                    .context_stack
                    .iter()
                    .any(|context| context.contains("PopupMenu") || context.contains("List"));
            if shortcut || escape {
                let _ = view.update(cx, |this, cx| this.shortcut(&key.key, window, cx));
            }
        })
        .detach();
        let mut this = Self {
            store,
            apps: Vec::new(),
            page: Page::Library,
            filter: None,
            search,
            queue,
            toast: appearance_error
                .as_ref()
                .map(|error| Toast::new(format!("Appearance: {error}"), true)),
            preferences,
            sidebar_drag: None,
            sidebar_animation: None,
            focus: cx.focus_handle(),
            scroll: gpui::ScrollHandle::new(),
            scroll_motion: None,
            scroll_frame_pending: false,
            color_picker: None,
            busy: false,
            appearance,
            appearance_error,
        };
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        this.reload();
        this.focus.focus(window);
        this.next_import(window, cx);
        this
    }

    fn reload(&mut self) {
        match self.store.entries() {
            Ok(apps) => self.apps = apps,
            Err(error) => self.error(error),
        }
    }

    fn error(&mut self, error: anyhow::Error) {
        self.toast = Some(Toast::new(format!("{error:#}"), true));
    }

    fn next_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_scroll();
        while let Some(path) = self.queue.pop_front() {
            let result = detect_executable(&path)
                .with_context(|| format!("Could not open {}", path.display()))
                .and_then(|kind| {
                    let source = path
                        .to_str()
                        .context("Choose a path with a UTF-8 filename")?;
                    Ok((kind, source))
                });
            match result {
                Ok((kind, source)) => {
                    self.page =
                        Page::Edit(Box::new(self.new_form(kind, source.to_owned(), window, cx)));
                    return;
                }
                Err(error) => self.error(error),
            }
        }
        self.page = Page::Library;
    }

    fn new_form(
        &mut self,
        kind: Kind,
        source: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Form {
        let mut draft = Draft::new(kind, source);
        if kind == Kind::AppImage {
            match self.store.settings() {
                Ok(settings) => draft.move_appimage = settings.image.move_to_directory,
                Err(error) => self.error(error),
            }
        }
        let form = Form::new(draft, window, cx);
        form.connect_inputs(window, cx);
        cx.observe(&form.icon, |_, _, cx| cx.notify()).detach();
        let initial = if form.from_file_manager {
            &form.name
        } else {
            &form.source
        };
        initial.update(cx, |input, cx| input.focus(window, cx));
        form
    }

    fn show_library(&mut self, filter: Option<Kind>, cx: &mut Context<Self>) {
        self.filter = filter;
        self.reset_scroll();
        self.page = Page::Library;
        self.queue.clear();
        self.reload();
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_scroll();
        self.page = Page::Edit(Box::new(self.new_form(
            self.filter.unwrap_or(Kind::AppImage),
            String::new(),
            window,
            cx,
        )));
        cx.notify();
    }

    fn load_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.store.settings().and_then(|settings| {
            let appearance = crate::config::local_appearance(&self.store.config_path())?;
            Ok(SettingsForm::new(settings, appearance, window, cx))
        });
        match result {
            Ok(form) => {
                self.focus.focus(window);
                self.page = Page::Settings(Box::new(form));
            }
            Err(error) => self.error(error),
        }
        cx.notify();
    }

    fn show_settings(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_scroll();
        self.load_settings(window, cx);
    }

    fn commit_input(state: &Entity<InputState>, window: &Window, cx: &mut Context<Self>) {
        cx.subscribe_in(state, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary: false }) {
                this.focus.focus(window);
                cx.notify();
            }
        })
        .detach();
    }

    fn edit(&mut self, entry: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_scroll();
        let mut draft = Draft::new(entry.target.kind(), entry.target.display());
        draft.id = Some(entry.id.clone());
        draft.name = entry.name.clone();
        draft.description = entry.description().unwrap_or_default().to_owned();
        draft.icon = entry.icon_override.clone();
        draft.terminal_command = entry.terminal_command.clone();
        draft.arguments = entry.arguments.clone();
        draft.working_directory = entry.working_directory.clone();
        let mut form = Form::new(draft, window, cx);
        form.connect_inputs(window, cx);
        form.entry = Some(entry.clone());
        cx.observe(&form.icon, |_, _, cx| cx.notify()).detach();
        self.focus.focus(window);
        self.page = Page::Edit(Box::new(form));
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Edit(form) = &self.page else { return };
        let draft = match form.read(cx) {
            Ok(draft) => draft,
            Err(error) => {
                self.error(error);
                cx.notify();
                return;
            }
        };
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.save(draft),
            |this, app, window, cx| {
                if let Some(icon) = app.icon {
                    ImageSource::from(icon).remove_asset(cx);
                }
                this.toast = Some(Toast::new(format!("Saved {}", app.name), false));
                this.reload();
                this.filter = None;
                this.search
                    .update(cx, |input, cx| input.set_value("", window, cx));
                this.next_import(window, cx);
            },
        );
    }

    fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Settings(form) = &self.page else {
            return;
        };
        let settings = form.read_settings(cx);
        let appearance = match form.read_theme(cx) {
            Ok(appearance) => appearance,
            Err(error) => {
                self.error(error);
                cx.notify();
                return;
            }
        };
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.save_settings(settings, appearance),
            move |this, (), window, cx| {
                this.toast = Some(Toast::new("Settings saved".into(), false));
                this.load_settings(window, cx);
            },
        );
    }

    fn remove(&mut self, delete_file: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Remove(form) = &self.page else {
            return;
        };
        let Some(entry) = &form.entry else { return };
        let id = entry.id.clone();
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.remove(&id, delete_file),
            |this, name, _, _| {
                this.toast = Some(Toast::new(format!("Removed {name}"), false));
                this.reload();
                this.page = Page::Library;
            },
        );
    }

    fn perform<T: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T> + Send + 'static,
        success: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.busy = true;
        let task = cx.background_spawn(async move { work() });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(value) => success(this, value, window, cx),
                    Err(error) => this.error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick_path(
        &mut self,
        field: Entity<InputState>,
        directory: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let response = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some("Select".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let selection = response
                .await
                .context("File picker closed unexpectedly")
                .flatten();
            let _ = view.update_in(cx, |this, window, cx| {
                match selection {
                    Ok(Some(paths)) => {
                        if let Some(path) = paths.first() {
                            if let Some(path) = path.to_str() {
                                field.update(cx, |field, cx| {
                                    field.set_value(path.to_owned(), window, cx)
                                });
                            } else {
                                this.error(anyhow::anyhow!("Choose a path with a UTF-8 filename"));
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => this.error(error.context("You can enter the path directly")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn row(&self) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(self.appearance.layout.spacing))
    }

    fn column(&self) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(self.appearance.layout.spacing * 2.))
    }

    fn page_width(&self, window: &Window) -> f32 {
        let layout = &self.appearance.layout;
        (f32::from(window.viewport_size().width)
            - self.visible_sidebar_width()
            - 2. * layout.page_padding
            - 2. * self.appearance.borders.window_width)
            .clamp(1., layout.content_width)
    }

    fn heading(&self, title: impl Into<SharedString>) -> gpui::Div {
        let size = self.appearance.typography.heading_size;
        div()
            .text_size(px(size))
            .line_height(px(size * 1.27))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(title.into())
    }

    fn input(&self, state: &Entity<InputState>) -> Input {
        let mut input = Input::new(state);
        input.style().border_widths = border_widths(self.appearance.borders.width);
        input
    }

    fn button(&self, id: impl Into<gpui::ElementId>) -> Control {
        let mut button = Control::new(id);
        button.style().border_widths = border_widths(self.appearance.borders.width);
        button
    }

    fn field(&self, label: &'static str, state: &Entity<InputState>, disabled: bool) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(self.appearance.layout.spacing * 0.5))
            .child(div().child(label))
            .child(self.input(state).disabled(disabled))
    }

    fn persist_preferences(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.store.save_preferences(&self.preferences) {
            self.error(error);
        }
        cx.notify();
    }

    fn sidebar_minimum(&self) -> f32 {
        (self.appearance.typography.font_size * 7. + 60.).clamp(155., 290.)
    }

    fn sidebar_width(&self) -> f32 {
        self.preferences
            .sidebar_width
            .unwrap_or(self.appearance.layout.sidebar_width)
            .clamp(self.sidebar_minimum(), 340.)
    }

    fn sidebar_target(&self) -> f32 {
        if self.preferences.sidebar_collapsed {
            60.
        } else {
            self.sidebar_width()
        }
    }

    fn visible_sidebar_width(&self) -> f32 {
        self.sidebar_animation
            .as_ref()
            .map_or_else(|| self.sidebar_target(), SidebarAnimation::width)
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool) {
        if self.preferences.sidebar_collapsed != collapsed {
            let current = self.visible_sidebar_width();
            self.preferences.sidebar_collapsed = collapsed;
            self.sidebar_animation = Some(SidebarAnimation {
                from: current,
                to: self.sidebar_target(),
                started: Instant::now(),
            });
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_collapsed(!self.preferences.sidebar_collapsed);
        self.persist_preferences(cx);
    }

    fn resize_sidebar(&mut self, width: f32) {
        let min = self.sidebar_minimum();
        if width < min {
            self.set_sidebar_collapsed(true);
        } else {
            self.preferences.sidebar_width = Some(width.clamp(min, 340.).round());
            self.set_sidebar_collapsed(false);
            if let Some(animation) = &mut self.sidebar_animation {
                animation.to = self.preferences.sidebar_width.unwrap_or(min);
            }
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.preferences.sidebar_collapsed
            || self.visible_sidebar_width() < self.sidebar_minimum();
        let filters = [
            (None, "All apps", "library"),
            (Some(Kind::AppImage), "AppImages", "package"),
            (Some(Kind::Native), "Executables", "terminal"),
            (Some(Kind::Web), "Web apps", "web"),
        ];
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .h_full()
            .w(px(self.visible_sidebar_width()))
            .overflow_hidden()
            .p_3()
            .gap_3()
            .bg(self.appearance.colors.sidebar.hsla())
            .child(
                self.row()
                    .justify_between()
                    .when(!collapsed, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .truncate()
                                .child(if self.sidebar_width() < 174. {
                                    "App Manager"
                                } else {
                                    "TW App Manager"
                                }),
                        )
                    })
                    .child(
                        self.button("collapse-sidebar")
                            .ghost()
                            .icon(if collapsed { "expand" } else { "collapse" })
                            .tooltip(if collapsed {
                                "Expand sidebar"
                            } else {
                                "Collapse sidebar"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(filters.into_iter().map(|(filter, label, icon)| {
                        let selected = matches!(self.page, Page::Library) && self.filter == filter;
                        self.button(label)
                            .icon(icon)
                            .tooltip(label)
                            .when(!collapsed, |button| {
                                button.label(label).w_full().justify_start()
                            })
                            .when(selected, |button| button.primary())
                            .when(!selected, |button| button.ghost())
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.show_library(filter, cx)),
                            )
                    })),
            )
            .child(div().flex_1())
            .child(
                self.button("settings")
                    .icon("settings")
                    .tooltip("Settings")
                    .when(!collapsed, |button| {
                        button.label("Settings").w_full().justify_start()
                    })
                    .when(matches!(self.page, Page::Settings(_)), |button| {
                        button.primary()
                    })
                    .when(!matches!(self.page, Page::Settings(_)), |button| {
                        button.ghost()
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(Self::show_settings)),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .w(px(7.))
                    .id("sidebar-resize")
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .hover(|this| {
                        this.bg(self.appearance.colors.accent_primary.hsla().opacity(0.3))
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            this.sidebar_drag =
                                Some((event.position.x, this.visible_sidebar_width()));
                            cx.stop_propagation();
                        }),
                    ),
            )
    }

    fn library(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.search.read(cx).value().to_lowercase();
        let apps: Vec<_> = self
            .apps
            .iter()
            .filter(|app| self.filter.is_none_or(|kind| app.target.kind() == kind))
            .filter(|app| {
                search.is_empty()
                    || app.name.to_lowercase().contains(&search)
                    || app
                        .description()
                        .is_some_and(|text| text.to_lowercase().contains(&search))
                    || app
                        .terminal_command
                        .as_ref()
                        .is_some_and(|command| command.to_lowercase().contains(&search))
            })
            .collect();
        let title = match self.filter {
            None => "All apps",
            Some(Kind::AppImage) => "AppImages",
            Some(Kind::Native) => "Linux programs",
            Some(Kind::Web) => "Web apps",
        };
        let grid = self.preferences.view == View::Grid;
        let gap = self.appearance.layout.spacing;
        let button_width = f32::from(Theme::global(cx).font_size * 2.);
        let inline_search = width - 2. * (button_width + gap);
        let wrap_toolbar = inline_search < 80.;
        let search_width = if wrap_toolbar { width } else { inline_search };
        let columns = ((width + gap) / (166. + gap)).floor().max(1.);
        let card_width = ((width - (columns - 1.) * gap) / columns).floor();
        self.column()
            .child(
                self.row()
                    .justify_between()
                    .child(self.heading(title))
                    .child(
                        self.button("add-app")
                            .primary()
                            .icon("plus")
                            .label("Add app")
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            .child(
                self.row()
                    .w(px(width))
                    .when(wrap_toolbar, |this| this.flex_wrap())
                    .child(
                        div()
                            .w(px(search_width))
                            .flex_none()
                            .overflow_hidden()
                            .child(self.input(&self.search).min_w_0().w_full()),
                    )
                    .child(
                        self.row().flex_none().children(
                            [
                                (View::List, "list", "List view"),
                                (View::Grid, "grid", "Grid view"),
                            ]
                            .into_iter()
                            .map(|(view, icon, label)| {
                                self.button(icon)
                                    .icon(icon)
                                    .tooltip(label)
                                    .when(self.preferences.view == view, |this| this.primary())
                                    .when(self.preferences.view != view, |this| this.ghost())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.preferences.view = view;
                                        this.persist_preferences(cx);
                                    }))
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(gap))
                    .w_full()
                    .min_w_0()
                    .when(grid, |this| this.flex_wrap())
                    .when(!grid, |this| this.flex_col())
                    .when(apps.is_empty(), |this| {
                        this.child(
                            self.card(div())
                                .w_full()
                                .p_4()
                                .text_color(self.appearance.colors.text_muted.hsla())
                                .child(if search.is_empty() {
                                    "No apps in this view yet."
                                } else {
                                    "No apps match your search."
                                }),
                        )
                    })
                    .children(
                        apps.into_iter()
                            .map(|app| self.app_card(app, grid.then_some(card_width), window, cx)),
                    ),
            )
    }

    fn card(&self, element: gpui::Div) -> gpui::Div {
        rounded(element, self.appearance.borders.card_radius)
            .bg(self.appearance.colors.surface.hsla())
            .when(self.appearance.borders.shadows, |this| this.shadow_sm())
    }

    fn icon(&self, source: Option<PathBuf>, kind: Kind, size: f32) -> gpui::Div {
        let palette = &self.appearance.colors;
        let (symbol, color) = match kind {
            Kind::AppImage => ("package", palette.accent_primary),
            Kind::Native => ("terminal", palette.accent_secondary),
            Kind::Web => ("web", palette.accent_tertiary),
        };
        let color = color.hsla();
        rounded(div(), self.appearance.borders.radius)
            .size(px(size))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .text_color(color)
            .when(source.is_none(), |this| {
                this.bg(palette.surface.hsla().blend(color.opacity(0.14)))
            })
            .child(match source {
                Some(path) => app_icon(path, size * 0.82).into_any_element(),
                None => glyph(symbol, size * 0.55)
                    .text_color(color)
                    .into_any_element(),
            })
    }

    fn app_card(
        &self,
        app: &Entry,
        grid_width: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = app.id.clone();
        let edit = app.clone();
        let kind = app.target.kind();
        let focus_id = SharedString::from(format!("focus-{}", app.id));
        let focus = window
            .use_keyed_state(focus_id, cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let palette = &self.appearance.colors;
        let focus_color = palette.border.focused.hsla();
        let hover = palette
            .surface
            .hsla()
            .blend(palette.accent_primary.hsla().opacity(0.13));
        let subtitle = app
            .description()
            .and_then(|text| text.lines().next())
            .unwrap_or(kind.label())
            .to_owned();
        let size = self.appearance.layout.icon_size * if grid_width.is_some() { 1.32 } else { 1. };
        let card = self
            .card(self.row())
            .p_3()
            .gap_3()
            .when_some(grid_width, |this, width| {
                this.w(px(width))
                    .min_h(px(128.))
                    .flex_col()
                    .justify_center()
            })
            .when(grid_width.is_none(), |this| this.w_full())
            .child(self.icon(app.icon.clone(), kind, size))
            .child(
                div()
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .when(grid_width.is_none(), |this| this.flex_1())
                    .when(grid_width.is_some(), |this| this.text_center())
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.text_muted.hsla())
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .id("app-card")
            .track_focus(&focus.tab_stop(true))
            .cursor_pointer()
            .hover(move |this| this.bg(hover))
            .focus(move |this| this.bg(hover.blend(focus_color.opacity(0.16))))
            .on_click(cx.listener(move |this, _, window, cx| this.edit(&edit, window, cx)))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    if let Err(error) = this.store.launch(&id, &[], false) {
                        this.error(error);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }),
            );
        div()
            .id(SharedString::from(app.id.clone()))
            .child(card)
            .into_any_element()
    }

    fn editor(&self, form: &Form, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = form.draft.kind;
        let new = form.draft.id.is_none();
        let source_label = match kind {
            Kind::AppImage => "AppImage file",
            Kind::Native => "x86-64 executable",
            Kind::Web => "Website URL",
        };
        let title = form
            .entry
            .as_ref()
            .map_or_else(|| "Add app".to_owned(), |app| app.name.clone());
        let selected_icon = optional_input(&form.icon, cx).map(PathBuf::from);
        let preview = if form.draft.use_default_icon
            || (form.draft.icon.is_some() && selected_icon.is_none())
        {
            None
        } else {
            let saved = form.entry.as_ref().and_then(|app| app.icon.clone());
            if selected_icon == form.draft.icon {
                saved.or(selected_icon)
            } else {
                selected_icon
                    .and_then(|path| self.store.expand(&path).ok())
                    .filter(|path| path.is_file())
                    .or(saved)
            }
        };
        self.column()
            .child(
                self.row()
                    .child(
                        self.button("back")
                            .ghost()
                            .icon("back")
                            .tooltip("Back")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(self.heading(title)),
            )
            .when(new && !form.from_file_manager, |this| {
                this.child(self.row().children(Kind::ALL.map(|kind| {
                    self.button(kind.label())
                        .label(kind.label())
                        .when(form.draft.kind == kind, |button| button.primary())
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let Page::Edit(current) = &mut this.page else {
                                return;
                            };
                            if current.draft.kind == kind {
                                return;
                            }
                            current.draft.kind = kind;
                            current
                                .source
                                .update(cx, |field, cx| field.set_value("", window, cx));
                            current.source.update(cx, |field, cx| {
                                field.set_placeholder(
                                    if kind == Kind::Web {
                                        "example.com"
                                    } else {
                                        "/path/to/file"
                                    },
                                    window,
                                    cx,
                                );
                            });
                            if kind == Kind::AppImage
                                && let Ok(settings) = this.store.settings()
                            {
                                current.draft.move_appimage = settings.image.move_to_directory;
                            }
                            cx.notify();
                        }))
                })))
            })
            .child(if kind == Kind::Web {
                self.field(source_label, &form.source, self.busy)
            } else {
                self.path_field("source", source_label, &form.source, false, cx)
            })
            .when(kind == Kind::AppImage, |this| {
                this.child(
                    self.row()
                        .child(
                            self.button("keep-appimage")
                                .label("Keep in place")
                                .when(!form.draft.move_appimage, |button| button.primary())
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.choose_move(false, cx)),
                                ),
                        )
                        .child(
                            self.button("move-appimage")
                                .label("Move to AppImages folder")
                                .when(form.draft.move_appimage, |button| button.primary())
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_move(true, cx))),
                        ),
                )
            })
            .child(self.field("Name", &form.name, self.busy))
            .child(
                self.field("Description", &form.description, self.busy)
                    .key_context("DescriptionEditor"),
            )
            .child(
                self.card(self.row())
                    .p_3()
                    .child(self.icon(preview, kind, self.appearance.layout.icon_size))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.path_field("icon", "Icon override", &form.icon, false, cx))
                            .when_some(
                                form.entry
                                    .as_ref()
                                    .filter(|app| app.icon_override.is_none())
                                    .and_then(|app| app.icon.as_ref()),
                                |this, icon| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(self.appearance.colors.text_muted.hsla())
                                            .truncate()
                                            .child(format!("Current icon: {}", icon.display())),
                                    )
                                },
                            ),
                    ),
            )
            .when(!new, |this| {
                this.child(
                    self.row().child(
                        self.button("reset-icon")
                            .label(if form.draft.use_default_icon {
                                "Restore current icon"
                            } else {
                                "Use default icon"
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Page::Edit(form) = &mut this.page {
                                    form.draft.use_default_icon = !form.draft.use_default_icon;
                                }
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(kind == Kind::Web && new, |this| {
                this.child(
                    self.row().child(
                        self.button("favicon")
                            .label(if form.draft.fetch_favicon {
                                "Favicon lookup on"
                            } else {
                                "Favicon lookup off"
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Page::Edit(form) = &mut this.page {
                                    form.draft.fetch_favicon = !form.draft.fetch_favicon;
                                }
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(self.field("Terminal command (optional)", &form.command, self.busy))
            .when(kind != Kind::Web, |this| {
                this.child(self.field("Launch arguments (optional)", &form.arguments, self.busy))
                    .child(self.path_field(
                        "working-directory",
                        "Working directory (optional)",
                        &form.directory,
                        true,
                        cx,
                    ))
            })
            .child(
                self.row()
                    .child(
                        self.button("save-app")
                            .primary()
                            .label(if self.busy {
                                "Saving…"
                            } else if new {
                                "Add app"
                            } else {
                                "Save changes"
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        self.button("cancel-app")
                            .ghost()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .when(form.entry.is_some(), |this| {
                        this.child(
                            self.button("remove-app")
                                .danger()
                                .label("Remove app")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Page::Edit(form) =
                                        std::mem::replace(&mut this.page, Page::Library)
                                    {
                                        this.page = Page::Remove(form);
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    fn choose_move(&mut self, move_appimage: bool, cx: &mut Context<Self>) {
        if let Page::Edit(form) = &mut self.page {
            form.draft.move_appimage = move_appimage;
        }
        cx.notify();
    }

    fn path_field(
        &self,
        id: &'static str,
        label: &'static str,
        state: &Entity<InputState>,
        directory: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let field = state.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(self.appearance.layout.spacing * 0.5))
            .child(div().child(label))
            .child(
                self.row()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.input(state).disabled(self.busy)),
                    )
                    .child(
                        self.button(id)
                            .label("Browse…")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_path(field.clone(), directory, window, cx);
                            })),
                    ),
            )
    }

    fn settings(&self, form: &SettingsForm, cx: &mut Context<Self>) -> impl IntoElement {
        self.column()
            .child(
                self.row()
                    .child(
                        self.button("back")
                            .ghost()
                            .icon("back")
                            .tooltip("Back")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(self.heading("Settings")),
            )
            .child(
                self.settings_group("General")
                    .child(self.path_field(
                        "icons-directory",
                        "Icons directory",
                        &form.icons,
                        true,
                        cx,
                    ))
                    .child(
                        self.setting_note(
                            "Leave empty to store icons in the app’s data directory.",
                        ),
                    ),
            )
            .child(
                self.settings_group("AppImages")
                    .child(self.path_field(
                        "appimage-directory",
                        "Move directory",
                        &form.folder,
                        true,
                        cx,
                    ))
                    .child(
                        self.row()
                            .child(
                                self.button("default-keep")
                                    .label("Keep in place")
                                    .when(!form.move_default, |this| this.primary())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_default_move(false, cx)
                                    })),
                            )
                            .child(
                                self.button("default-move")
                                    .label("Move on import")
                                    .when(form.move_default, |this| this.primary())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_default_move(true, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                self.settings_group("Web apps")
                    .child(self.path_field(
                        "web-browser",
                        "Browser executable",
                        &form.browser,
                        false,
                        cx,
                    ))
                    .child(
                        self.setting_note(
                            "Use a browser with app mode, such as Helium or Chromium.",
                        ),
                    ),
            )
            .child(
                self.settings_group("Executables")
                    .child(self.path_field(
                        "default-working-directory",
                        "Default working directory",
                        &form.working_directory,
                        true,
                        cx,
                    ))
                    .child(
                        self.setting_note("Leave empty to start each program from its own folder."),
                    ),
            )
            .child(self.appearance_settings(form, cx))
    }

    fn settings_group(&self, title: &'static str) -> gpui::Div {
        self.card(self.column())
            .p_4()
            .gap_3()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
    }

    fn settings_save_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().absolute().right_5().bottom_4().child(
            self.button("save-settings")
                .primary()
                .label(if self.busy {
                    "Saving…"
                } else {
                    "Save settings"
                })
                .shadow_md()
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, window, cx| this.save_settings(window, cx))),
        )
    }

    fn setting_note(&self, text: &'static str) -> gpui::Div {
        div()
            .text_sm()
            .text_color(self.appearance.colors.text_muted.hsla())
            .child(text)
    }

    fn appearance_settings(&self, form: &SettingsForm, cx: &mut Context<Self>) -> impl IntoElement {
        self.settings_group("Appearance")
            .child(self.row().justify_between().flex_wrap()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Palette"))
                .child(self.row().children([(true, "Follow theme"), (false, "Local colors")].map(|(linked, label)| {
                    self.button(label).label(label).disabled(self.busy)
                        .when(form.use_included_colors == linked, |this| this.primary())
                        .when(form.use_included_colors != linked, |this| this.ghost())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Page::Settings(form) = &mut this.page {
                                form.use_included_colors = linked;
                                cx.notify();
                            }
                        }))
                }))))
            .when(form.use_included_colors, |this| this.child(
                self.setting_note("Theme includes can supply the palette. Picking a color switches to local colors.")
            ))
            .child(div().flex().flex_col().gap_1().children(form.colors.chunks(2).map(|row| {
                self.row().children(row.iter().enumerate().map(|(index, color)| {
                    let key = color.key;
                    let value = color.value;
                    let label = color.label;
                    let outline = self.appearance.colors.border.unfocused.hsla();
                    let hover = self.appearance.colors.accent_primary.hsla();
                    let focus = color.focus.clone();
                    rounded(self.row(), self.appearance.borders.radius)
                        .flex_1().min_w_0().h(px(40.)).px_2()
                        .child(div().flex_none().size(px(24.)).rounded(px(self.appearance.borders.radius)).bg(value)
                            .border_1().border_color(outline))
                        .child(div().min_w_0().truncate().text_sm().child(label))
                        .id(key)
                        .track_focus(&focus).cursor_pointer()
                        .hover(move |this| this.bg(hover.opacity(0.16)))
                        .focus(move |this| this.bg(hover.opacity(0.22)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if !this.busy {
                                this.open_color_picker(key, label, value, window, cx);
                            }
                        }))
                        .when(index == 0 && row.len() == 1, |this| this.max_w(relative(0.5)))
                }))
            })))
            .child(self.row().items_start().flex_wrap()
                .child(div().flex_1().min_w(px(150.)).flex().flex_col()
                    .gap(px(self.appearance.layout.spacing * 0.5))
                    .child("Font family")
                    .child(form.font_family.clone()))
                .child(div().w(px(120.)).child(self.field("Font size", &form.font_size, self.busy)))
                .child(div().w(px(120.)).child(self.field("Corner radius", &form.corner_radius, self.busy))))
    }

    fn open_color_picker(
        &mut self,
        key: &'static str,
        label: &'static str,
        value: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker = cx.new(|cx| ColorPicker::new(key, label, value, window, cx));
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &PickerEvent, window, cx| {
                if let PickerEvent::Apply(key, value) = event
                    && let Page::Settings(form) = &mut this.page
                    && let Some(color) = form.colors.iter_mut().find(|color| color.key == *key)
                {
                    color.value = *value;
                    form.use_included_colors = false;
                }
                this.color_picker = None;
                this.focus.focus(window);
                cx.notify();
            },
        )
        .detach();
        self.color_picker = Some(picker);
        self.focus.focus(window);
        cx.notify();
    }

    fn apply_color(&self, cx: &mut Context<Self>) {
        if let Some(picker) = &self.color_picker {
            picker.update(cx, |picker, cx| picker.apply(cx));
        }
    }

    fn choose_default_move(&mut self, move_default: bool, cx: &mut Context<Self>) {
        if let Page::Settings(form) = &mut self.page {
            form.move_default = move_default;
        }
        cx.notify();
    }

    fn confirm_remove(&self, app: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let can_delete = matches!(app.target, Target::AppImage { managed: true, .. });
        self.column()
            .child(self.heading(format!("Remove {}?", app.name)))
            .child(
                div()
                    .text_color(self.appearance.colors.text_muted.hsla())
                    .child("The app’s menu launcher and terminal command will be removed."),
            )
            .when(can_delete, |this| {
                this.child(
                    div()
                        .text_color(self.appearance.colors.text_muted.hsla())
                        .text_sm()
                        .child(format!("AppImage file: {}", app.target.display())),
                )
            })
            .child(
                self.row()
                    .child(
                        self.button("keep-remove")
                            .label(if can_delete {
                                "Remove and keep file"
                            } else {
                                "Remove app"
                            })
                            .danger()
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.remove(false, window, cx)),
                            ),
                    )
                    .when(can_delete, |this| {
                        this.child(
                            self.button("delete-remove")
                                .label("Remove and delete file")
                                .danger()
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.remove(true, window, cx)
                                    }),
                                ),
                        )
                    })
                    .child(
                        self.button("cancel-remove")
                            .ghost()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    ),
            )
    }
}

impl Manager {
    fn reset_scroll(&mut self) {
        self.scroll_motion = None;
        self.color_picker = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    fn scroll_wheel(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gpui::ScrollDelta::Lines(delta) = event.delta else {
            self.scroll_motion = None;
            return;
        };
        let limit = self.scroll.max_offset().height;
        if delta.y == 0. || limit == px(0.) {
            return;
        }
        let current = self.scroll.offset().y;
        let target = (self
            .scroll_motion
            .as_ref()
            .map_or(current, |motion| motion.to)
            + px(delta.y * 36.))
        .clamp(-limit, px(0.));
        if target != current || self.scroll_motion.is_some() {
            self.scroll_motion = Some(ScrollMotion {
                from: current,
                to: target,
                started: Instant::now(),
            });
            self.request_scroll_frame(window, cx);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn request_scroll_frame(&mut self, window: &Window, cx: &Context<Self>) {
        if self.scroll_frame_pending {
            return;
        }
        self.scroll_frame_pending = true;
        let view = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.scroll_frame_pending = false;
                if let Some(motion) = &this.scroll_motion {
                    let progress = (motion.started.elapsed().as_secs_f32() / 0.17).min(1.);
                    let y =
                        motion.from + (motion.to - motion.from) * (1. - (1. - progress).powi(3));
                    this.scroll.set_offset(gpui::point(px(0.), y));
                    if progress < 1. {
                        this.request_scroll_frame(window, cx);
                    } else {
                        this.scroll_motion = None;
                    }
                    cx.notify();
                }
            });
        });
    }

    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_drag.take().is_some() {
            self.persist_preferences(cx);
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match &self.page {
            Page::Remove(..) => {
                if let Page::Remove(form) = std::mem::replace(&mut self.page, Page::Library) {
                    self.page = Page::Edit(form);
                    self.focus.focus(window);
                }
            }
            Page::Edit(_) => {
                self.next_import(window, cx);
                if self.queue.is_empty() && matches!(self.page, Page::Library) {
                    self.focus.focus(window);
                }
            }
            _ => {
                self.show_library(self.filter, cx);
                self.focus.focus(window);
            }
        }
        cx.notify();
    }

    fn keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || event.is_held {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if self.color_picker.is_some() {
            if event.keystroke.key == "enter"
                && !event.keystroke.modifiers.modified()
                && self.focus.is_focused(window)
            {
                self.apply_color(cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        let focus_free = window.focused(cx).is_none_or(|handle| handle == self.focus);
        match event.keystroke.key.as_str() {
            "enter" if !modifiers.modified() && focus_free => match self.page {
                Page::Edit(_) => self.save(window, cx),
                Page::Settings(_) => self.save_settings(window, cx),
                _ => return,
            },
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn shortcut(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.color_picker.is_some() {
            match key {
                "escape" => {
                    self.color_picker = None;
                    self.focus.focus(window);
                    cx.notify();
                }
                "enter" => self.apply_color(cx),
                _ => return,
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        match key {
            "escape" if !matches!(self.page, Page::Library) => self.cancel(window, cx),
            "escape" if self.toast.is_some() => {
                self.toast = None;
                cx.notify();
            }
            "b" => {
                self.toggle_sidebar(cx);
            }
            "n" if matches!(self.page, Page::Library) => self.add(window, cx),
            "f" if matches!(self.page, Page::Library) => {
                self.search.update(cx, |input, cx| input.focus(window, cx));
            }
            "enter" => match self.page {
                Page::Edit(_) => self.save(window, cx),
                Page::Settings(_) => self.save_settings(window, cx),
                _ => return,
            },
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn toast_widget(&self, toast: &Toast, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = &self.appearance.colors;
        let accent = if toast.error {
            palette.error
        } else {
            palette.accent_primary
        }
        .hsla();
        let ratio = toast.remaining.as_secs_f32() / toast.duration.as_secs_f32();
        let container = self
            .row()
            .px_3()
            .py_2()
            .min_h(px(44.))
            .child(glyph(if toast.error { "alert" } else { "check" }, 18.).text_color(accent))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(toast.message.clone()),
            )
            .child(div().size(px(22.)).flex_none().when(toast.paused, |this| {
                this.child(
                    self.button("dismiss-toast")
                        .compact()
                        .ghost()
                        .icon("close")
                        .tooltip("Dismiss notification")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        })),
                )
            }));
        self.card(div())
            .absolute()
            .bottom(px(if matches!(self.page, Page::Settings(_)) {
                60.
            } else {
                16.
            }))
            .right_4()
            .w(px(365.))
            .max_w(relative(0.88))
            .overflow_hidden()
            .shadow_md()
            .child(container)
            .child(
                div()
                    .h(px(3.))
                    .bg(accent.opacity(0.18))
                    .child(div().size_full().w(relative(ratio)).bg(accent)),
            )
            .id("toast")
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if let Some(toast) = &mut this.toast {
                    let now = Instant::now();
                    if !toast.paused {
                        toast.remaining = toast.remaining.saturating_sub(now - toast.updated);
                    }
                    toast.updated = now;
                    toast.paused = *hovered;
                    cx.notify();
                }
            }))
    }
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = self.page_width(window);
        let body = match &self.page {
            Page::Library => self.library(width, window, cx).into_any_element(),
            Page::Edit(form) => self.editor(form, cx).into_any_element(),
            Page::Settings(form) => self.settings(form, cx).into_any_element(),
            Page::Remove(form) => self
                .confirm_remove(
                    form.entry.as_ref().expect("remove opened for a saved app"),
                    cx,
                )
                .into_any_element(),
        };
        let mut root = div()
            .relative()
            .flex()
            .size_full()
            .text_color(self.appearance.colors.text_primary.hsla())
            .text_size(px(self.appearance.typography.font_size));
        root.style().border_widths = border_widths(self.appearance.borders.window_width);
        let palette = &self.appearance.colors.border;
        root.border_color(if window.is_window_active() {
            palette.focused.hsla()
        } else {
            palette.outer.hsla()
        })
        .id("manager")
        .track_focus(&self.focus)
        .on_key_down(cx.listener(Self::keyboard))
        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
            if let Some((origin, width)) = this.sidebar_drag {
                if event.pressed_button == Some(gpui::MouseButton::Left) {
                    this.resize_sidebar(width + f32::from(event.position.x - origin));
                    cx.notify();
                } else {
                    this.finish_resize(cx);
                }
            }
        }))
        .on_mouse_up(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_resize(cx)),
        )
        .on_mouse_up_out(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_resize(cx)),
        )
        .child(self.sidebar(cx))
        .child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .when(self.busy, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .bg(self.appearance.colors.accent_primary.hsla()),
                    )
                })
                .child(
                    div()
                        .id("page-scroll")
                        .track_scroll(&self.scroll)
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(px(self.appearance.layout.page_padding))
                        .when(matches!(self.page, Page::Settings(_)), |this| {
                            this.pb(px(self.appearance.layout.page_padding + 62.))
                        })
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                if !window.default_prevented() {
                                    this.focus.focus(window);
                                    cx.notify();
                                }
                            }),
                        )
                        .child(
                            div()
                                .w(px(width))
                                .mx_auto()
                                .on_scroll_wheel(cx.listener(Self::scroll_wheel))
                                .child(body),
                        ),
                )
                .when(matches!(self.page, Page::Settings(_)), |this| {
                    this.child(self.settings_save_button(cx))
                })
                .when_some(self.toast.as_ref(), |this, toast| {
                    this.child(self.toast_widget(toast, cx))
                }),
        )
        .when_some(self.color_picker.as_ref(), |this, picker| {
            this.child(picker.clone())
        })
    }
}

struct ScrollMotion {
    from: gpui::Pixels,
    to: gpui::Pixels,
    started: Instant,
}

struct SidebarAnimation {
    from: f32,
    to: f32,
    started: Instant,
}

impl SidebarAnimation {
    fn width(&self) -> f32 {
        let progress = (self.started.elapsed().as_secs_f32() / 0.18).min(1.);
        self.from + (self.to - self.from) * (1. - (1. - progress).powi(3))
    }

    fn finished(&self) -> bool {
        self.started.elapsed() >= Duration::from_millis(180)
    }
}

struct Toast {
    message: String,
    error: bool,
    paused: bool,
    duration: Duration,
    remaining: Duration,
    updated: Instant,
}

impl Toast {
    fn new(message: String, error: bool) -> Self {
        let duration = Duration::from_secs(if error { 9 } else { 5 });
        Self {
            message,
            error,
            paused: false,
            duration,
            remaining: duration,
            updated: Instant::now(),
        }
    }
}

struct Form {
    draft: Draft,
    from_file_manager: bool,
    name: Entity<InputState>,
    source: Entity<InputState>,
    description: Entity<InputState>,
    entry: Option<Entry>,
    icon: Entity<InputState>,
    command: Entity<InputState>,
    arguments: Entity<InputState>,
    directory: Entity<InputState>,
}

impl Form {
    fn new(draft: Draft, window: &mut Window, cx: &mut App) -> Self {
        let arguments = shell_words::join(&draft.arguments);
        let directory = draft
            .working_directory
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source_hint = if draft.kind == Kind::Web {
            "example.com"
        } else {
            "/path/to/file"
        };
        Self {
            from_file_manager: draft.id.is_none() && !draft.source.is_empty(),
            name: text_input(&draft.name, "Use detected name", window, cx),
            description: cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder("Optional description")
                    .default_value(draft.description.clone())
            }),
            entry: None,
            source: text_input(&draft.source, source_hint, window, cx),
            icon: text_input(
                &draft
                    .icon
                    .as_ref()
                    .map(|path| path.to_string_lossy())
                    .unwrap_or_default(),
                "Use app icon",
                window,
                cx,
            ),
            command: text_input(
                draft.terminal_command.as_deref().unwrap_or_default(),
                "e.g. notes",
                window,
                cx,
            ),
            arguments: text_input(&arguments, "e.g. --profile work", window, cx),
            directory: text_input(&directory, "Use executable’s folder", window, cx),
            draft,
        }
    }

    fn connect_inputs(&self, window: &Window, cx: &mut Context<Manager>) {
        for input in [
            &self.source,
            &self.name,
            &self.icon,
            &self.command,
            &self.arguments,
            &self.directory,
        ] {
            Manager::commit_input(input, window, cx);
        }
    }

    fn read(&self, cx: &App) -> Result<Draft> {
        let mut draft = self.draft.clone();
        draft.name = self.name.read(cx).value().to_string();
        draft.description = self.description.read(cx).value().to_string();
        draft.source = self.source.read(cx).value().to_string();
        draft.icon = optional_input(&self.icon, cx).map(PathBuf::from);
        if draft.icon != self.draft.icon {
            draft.use_default_icon = draft.icon.is_none();
        }
        draft.terminal_command =
            optional_input(&self.command, cx).map(|value| value.trim().to_owned());
        if draft.kind != Kind::Web {
            draft.arguments = shell_words::split(self.arguments.read(cx).value().as_str())
                .context("Check the quotes in your launch arguments")?;
            draft.working_directory = optional_input(&self.directory, cx).map(PathBuf::from);
        }
        Ok(draft)
    }
}

struct ThemeColor {
    key: &'static str,
    label: &'static str,
    original: gpui::Hsla,
    value: gpui::Hsla,
    focus: gpui::FocusHandle,
}

struct SettingsForm {
    original: Settings,
    browser: Entity<InputState>,
    folder: Entity<InputState>,
    icons: Entity<InputState>,
    working_directory: Entity<InputState>,
    move_default: bool,
    local_theme: toml::Table,
    use_included_colors: bool,
    colors: Vec<ThemeColor>,
    font_family: Entity<FontPicker>,
    font_size: Entity<InputState>,
    corner_radius: Entity<InputState>,
}

impl SettingsForm {
    fn new(
        settings: Settings,
        appearance: toml::Table,
        window: &mut Window,
        cx: &mut Context<Manager>,
    ) -> Self {
        let browser = text_input(
            &settings.web.browser_path.to_string_lossy(),
            "/usr/bin/helium",
            window,
            cx,
        );
        let folder = text_input(
            &settings.image.move_directory.to_string_lossy(),
            "~/Applications",
            window,
            cx,
        );
        let icons = text_input(
            &settings.icons_directory.to_string_lossy(),
            "App data directory",
            window,
            cx,
        );
        let working_directory = text_input(
            &settings.executable.working_directory.to_string_lossy(),
            "Program’s own folder",
            window,
            cx,
        );
        for input in [&browser, &folder, &icons, &working_directory] {
            Manager::commit_input(input, window, cx);
        }
        let colors = [
            ("background", "Background"),
            ("sidebar", "Sidebar"),
            ("surface", "Cards"),
            ("text_primary", "Text"),
            ("text_muted", "Muted text"),
            ("accent_primary", "Primary accent"),
            ("accent_secondary", "Secondary accent"),
            ("accent_tertiary", "Tertiary accent"),
            ("accent_text", "Text on accent"),
            ("error", "Error"),
            ("error_text", "Text on error"),
        ]
        .into_iter()
        .filter_map(|(key, label)| {
            let value = parse_hex(appearance.get("colors")?.get(key)?.as_str()?)?;
            Some(ThemeColor {
                key,
                label,
                original: value,
                value,
                focus: cx.focus_handle().tab_stop(true),
            })
        })
        .collect();
        let typography = appearance.get("typography");
        let current_family = typography
            .and_then(|table| table.get("font_family"))
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        let font_family = cx.new(|cx| FontPicker::new(current_family.to_owned().into(), cx));
        let font_size = text_input(
            &typography
                .and_then(|table| table.get("font_size"))
                .map(ToString::to_string)
                .unwrap_or_else(|| "14".into()),
            "14",
            window,
            cx,
        );
        let corner_radius = text_input(
            &appearance
                .get("borders")
                .and_then(|borders| borders.get("card_radius"))
                .map(ToString::to_string)
                .unwrap_or_else(|| "6".into()),
            "6",
            window,
            cx,
        );
        Manager::commit_input(&corner_radius, window, cx);
        Manager::commit_input(&font_size, window, cx);
        Self {
            move_default: settings.image.move_to_directory,
            use_included_colors: appearance
                .get("use_included_colors")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true),
            original: settings,
            browser,
            folder,
            icons,
            working_directory,
            local_theme: appearance,
            colors,
            font_family,
            font_size,
            corner_radius,
        }
    }

    fn read_settings(&self, cx: &App) -> Option<Settings> {
        let mut settings = self.original.clone();
        settings.web.browser_path = PathBuf::from(self.browser.read(cx).value().as_str().trim());
        settings.image.move_directory = PathBuf::from(self.folder.read(cx).value().as_str().trim());
        settings.icons_directory = PathBuf::from(self.icons.read(cx).value().as_str().trim());
        settings.executable.working_directory =
            PathBuf::from(self.working_directory.read(cx).value().as_str().trim());
        settings.image.move_to_directory = self.move_default;
        (settings != self.original).then_some(settings)
    }

    fn read_theme(&self, cx: &App) -> Result<Option<toml::Table>> {
        let mut updated = self.local_theme.clone();
        let was_included = self
            .local_theme
            .get("use_included_colors")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if was_included != self.use_included_colors {
            updated.insert(
                "use_included_colors".into(),
                self.use_included_colors.into(),
            );
        }
        let colors = updated
            .get_mut("colors")
            .and_then(toml::Value::as_table_mut)
            .context("Missing palette")?;
        for color in &self.colors {
            if color.value != color.original {
                colors.insert(color.key.to_owned(), encode_color(color.value).into());
            }
        }
        let typography = updated
            .get_mut("typography")
            .and_then(toml::Value::as_table_mut)
            .context("Missing typography")?;
        let family = self.font_family.read(cx).family();
        if typography.get("font_family").and_then(toml::Value::as_str) != Some(family) {
            typography.insert("font_family".into(), family.into());
        }
        let size: f64 = self
            .font_size
            .read(cx)
            .value()
            .trim()
            .parse()
            .context("Font size requires a number")?;
        let previous = typography.get("font_size").and_then(|value| {
            value
                .as_float()
                .or_else(|| value.as_integer().map(|size| size as f64))
        });
        if previous != Some(size) {
            typography.insert("font_size".into(), size.into());
        }
        let radius: f64 = self
            .corner_radius
            .read(cx)
            .value()
            .trim()
            .parse()
            .context("Corner radius requires a number")?;
        let borders = updated
            .get_mut("borders")
            .and_then(toml::Value::as_table_mut)
            .context("Missing borders table")?;
        let previous = borders.get("card_radius").and_then(|value| {
            value
                .as_float()
                .or_else(|| value.as_integer().map(|n| n as f64))
        });
        if previous != Some(radius) {
            borders.insert("radius".into(), radius.into());
            borders.insert("card_radius".into(), radius.into());
        }
        Ok((updated != self.local_theme).then_some(updated))
    }
}

fn text_input(
    value: &str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value.to_owned())
    })
}

fn optional_input(state: &Entity<InputState>, cx: &App) -> Option<String> {
    let value = state.read(cx).value().to_string();
    (!value.is_empty()).then_some(value)
}

fn border_widths(width: f32) -> gpui::EdgesRefinement<gpui::AbsoluteLength> {
    let width = Some(px(width).into());
    gpui::EdgesRefinement {
        top: width,
        right: width,
        bottom: width,
        left: width,
    }
}

fn rounded(mut element: gpui::Div, radius: f32) -> gpui::Div {
    element.style().corner_radii = gpui::CornersRefinement {
        top_left: Some(px(radius).into()),
        top_right: Some(px(radius).into()),
        bottom_left: Some(px(radius).into()),
        bottom_right: Some(px(radius).into()),
    };
    element
}
