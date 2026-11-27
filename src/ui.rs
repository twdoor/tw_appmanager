use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use ely_core::{
    Assets,
    buttons::{Button, ButtonVariant, IconButton, SegmentedControl},
    feedback::EmptyState,
    forms::{FormField, Input, InputEvent, TextInput},
    layout::Section,
    navigation::NavItem,
    primitives::{FocusScope, Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Mode, Theme},
};
use gpui::{
    App, AppContext, Bounds, Context, Entity, ImageSource, IntoElement, PathPromptOptions, Render,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    relative, size,
};

use crate::{
    APP_NAME,
    appearance::Appearance,
    appimage::detect_executable,
    color_picker::{ColorPicker, PickerEvent, encode_color, parse_hex},
    config::{Preferences, View},
    desktop::ID,
    font_picker::{FontChanged, FontPicker},
    library::{Draft, Entry, Kind, Settings, Store, Target},
    widgets::{app_icon, brand_icon},
};

pub fn run(store: Store, paths: Vec<PathBuf>) -> Result<()> {
    let startup_error = Rc::new(RefCell::new(None));
    let window_error = startup_error.clone();
    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(error) = ely_core::init(cx) {
                *window_error.borrow_mut() = Some(error.context("Could not initialize Ely"));
                cx.quit();
                return;
            }
            Theme::set_mode_now(Mode::Dark, cx);
            let bounds = Bounds::centered(None, size(px(840.), px(700.)), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(660.), px(480.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_NAME.into()),
                        ..TitlebarOptions::default()
                    }),
                    app_id: Some(ID.into()),
                    ..WindowOptions::default()
                },
                |window, cx| {
                    window.set_window_title(APP_NAME);
                    cx.new(|cx| Manager::new(store, paths.into(), window, cx))
                },
            );
            match window {
                Ok(_) => {
                    cx.on_window_closed(|cx, _| {
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
    search: Entity<TextInput>,
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
    system_font: SharedString,
    appearance_revision: u64,
    pending_setting: Option<(TextSetting, gpui::Task<()>)>,
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
        search.update(cx, |input, cx| input.set_label("Search apps", cx));
        Self::commit_input(&search, window, cx);
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let path = store.config_path();
        let system_font = window.text_style().font_family.clone();
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
                let Ok(revision) = view.update_in(cx, |this, _, _| this.appearance_revision) else {
                    break;
                };
                let path = path.clone();
                let result = executor.spawn(async move { Appearance::load(&path) }).await;
                if view
                    .update_in(cx, |this, window, cx| {
                        if this.color_picker.is_some() || this.appearance_revision != revision {
                            return;
                        }
                        match result {
                            Ok(appearance) => {
                                let updated = this.appearance != appearance;
                                if updated {
                                    appearance.apply(&this.system_font, window, cx);
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
            if shortcut {
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
            system_font,
            appearance_revision: 0,
            pending_setting: None,
            appearance_error,
        };
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        let view = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = view.update(cx, |this, cx| this.flush_setting(window, cx));
            true
        });
        this.reload();
        this.focus.focus(window, cx);
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
        window.focus(&initial.read(cx).focus().clone(), cx);
        form
    }

    fn show_library(&mut self, filter: Option<Kind>, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_setting(window, cx);
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
        self.flush_setting(window, cx);
        let result = self.store.settings().and_then(|settings| {
            let appearance = crate::config::local_appearance(&self.store.config_path())?;
            Ok(SettingsForm::new(settings, appearance, window, cx))
        });
        match result {
            Ok(form) => {
                self.connect_settings(&form, window, cx);
                self.focus.focus(window, cx);
                self.page = Page::Settings(Box::new(form));
            }
            Err(error) => self.error(error),
        }
        cx.notify();
    }

    fn commit_input(state: &Entity<TextInput>, window: &Window, cx: &mut Context<Self>) {
        cx.subscribe_in(state, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Submit) {
                this.focus.focus(window, cx);
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
        self.focus.focus(window, cx);
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
                this.search.update(cx, |input, cx| input.set_text("", cx));
                this.next_import(window, cx);
            },
        );
    }

    fn connect_settings(&self, form: &SettingsForm, window: &Window, cx: &mut Context<Self>) {
        for (setting, field) in form.text_fields() {
            cx.subscribe_in(field, window, move |this, field, event, window, cx| {
                let Page::Settings(form) = &this.page else {
                    return;
                };
                if !form.text_fields().iter().any(|(_, input)| *input == field) {
                    return;
                }
                match event {
                    InputEvent::Changed => this.schedule_setting(setting, window, cx),
                    InputEvent::Blur | InputEvent::Submit => {
                        this.commit_setting(setting, true, window, cx);
                    }
                    InputEvent::Focus => {}
                }
            })
            .detach();
        }
        cx.subscribe_in(
            &form.font_family,
            window,
            |this, picker, family: &FontChanged, window, cx| {
                if matches!(&this.page, Page::Settings(form) if &form.font_family == picker) {
                    this.save_appearance(
                        toml::toml! {
                            [typography]
                            font_family = (family.0.as_str())
                        },
                        window,
                        cx,
                    );
                }
            },
        )
        .detach();
    }

    fn schedule_setting(
        &mut self,
        setting: TextSetting,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_setting
            .as_ref()
            .is_some_and(|(pending, _)| *pending != setting)
        {
            self.flush_setting(window, cx);
        }
        let clock = cx.background_executor().clone();
        self.pending_setting = Some((
            setting,
            cx.spawn_in(window, async move |view, cx| {
                clock.timer(Duration::from_millis(450)).await;
                let _ = view.update_in(cx, |this, window, cx| {
                    this.commit_setting(setting, false, window, cx);
                });
            }),
        ));
    }

    fn flush_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((setting, _)) = self.pending_setting.take() {
            self.commit_setting(setting, true, window, cx);
        }
    }

    fn commit_setting(
        &mut self,
        setting: TextSetting,
        report_error: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending_setting
            .as_ref()
            .is_some_and(|(pending, _)| *pending == setting)
        {
            self.pending_setting = None;
        }
        let Page::Settings(form) = &self.page else {
            return;
        };
        let Some((_, field)) = form
            .text_fields()
            .into_iter()
            .find(|(key, _)| *key == setting)
        else {
            return;
        };
        let text = field.read(cx).text().trim().to_owned();
        if text == form.saved_text[setting as usize] {
            return;
        }
        let result = (|| match setting {
            TextSetting::Browser => self.store.save_settings(
                Some(toml::toml! { [web] browser_path = (text.as_str()) }),
                None,
            ),
            TextSetting::AppImages => self.store.save_settings(
                Some(toml::toml! { [image] move_directory = (text.as_str()) }),
                None,
            ),
            TextSetting::Icons => self.store.save_settings(
                Some(toml::toml! { icons_directory = (text.as_str()) }),
                None,
            ),
            TextSetting::WorkingDirectory => self.store.save_settings(
                Some(toml::toml! { [executable] working_directory = (text.as_str()) }),
                None,
            ),
            TextSetting::FontSize => {
                let size: f64 = text.parse().context("Font size requires a number")?;
                self.store
                    .save_settings(None, Some(toml::toml! { [typography] font_size = (size) }))?;
                self.refresh_appearance(window, cx)
            }
            TextSetting::CornerRadius => {
                let radius: f64 = text.parse().context("Corner radius requires a number")?;
                self.store.save_settings(
                    None,
                    Some(toml::toml! {
                        [borders]
                        radius = (radius)
                        card_radius = (radius)
                    }),
                )?;
                self.refresh_appearance(window, cx)
            }
        })();
        match result {
            Ok(()) => {
                if let Page::Settings(form) = &mut self.page {
                    form.saved_text[setting as usize] = text;
                }
            }
            Err(error) if report_error => {
                self.error(error);
                cx.notify();
            }
            Err(_) => {}
        }
    }

    fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.appearance_revision = self.appearance_revision.wrapping_add(1);
        if self.appearance != appearance {
            appearance.apply(&self.system_font, window, cx);
            self.appearance = appearance;
            cx.notify();
        }
    }

    fn refresh_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        let appearance = Appearance::load(&self.store.config_path())?;
        self.set_appearance(appearance, window, cx);
        Ok(())
    }

    fn save_appearance(
        &mut self,
        patch: toml::Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self
            .store
            .save_settings(None, Some(patch))
            .and_then(|()| self.refresh_appearance(window, cx))
        {
            Ok(()) => true,
            Err(error) => {
                self.error(error);
                cx.notify();
                false
            }
        }
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

    fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        let fields: &[_] = match &self.page {
            Page::Edit(form) => &[
                &form.source,
                &form.name,
                &form.description,
                &form.icon,
                &form.command,
                &form.arguments,
                &form.directory,
            ],
            Page::Settings(form) => {
                form.font_family
                    .update(cx, |picker, cx| picker.set_disabled(busy, cx));
                &[
                    &form.browser,
                    &form.folder,
                    &form.icons,
                    &form.working_directory,
                    &form.font_size,
                    &form.corner_radius,
                ]
            }
            _ => &[],
        };
        for input in fields {
            input.update(cx, |input, cx| input.set_disabled(busy, cx));
        }
    }

    fn perform<T: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T> + Send + 'static,
        success: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.set_busy(true, cx);
        let task = cx.background_spawn(async move { work() });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.set_busy(false, cx);
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
        field: Entity<TextInput>,
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
            let _ = view.update_in(cx, |this, _, cx| {
                match selection {
                    Ok(Some(paths)) => {
                        if let Some(path) = paths.first() {
                            if let Some(path) = path.to_str() {
                                field.update(cx, |field, cx| field.set_text(path.to_owned(), cx));
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

    fn field(&self, label: &'static str, state: &Entity<TextInput>) -> FormField {
        FormField::new(label, label).child(Input::new(state))
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
        let items = [
            (None, "All apps", IconName::Library),
            (Some(Kind::AppImage), "AppImages", IconName::Package),
            (Some(Kind::Native), "Executables", IconName::Terminal),
            (Some(Kind::Web), "Web apps", IconName::Globe),
        ];
        let view = cx.weak_entity();
        let settings = cx.weak_entity();
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
                            self.row()
                                .gap_2()
                                .child(brand_icon(self.appearance.colors.accent_primary))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(APP_NAME),
                                ),
                        )
                    })
                    .child(
                        IconButton::new("collapse-sidebar", IconName::PanelLeft)
                            .variant(ButtonVariant::Ghost)
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
                    .children(items.into_iter().map(|(filter, label, icon)| {
                        let view = view.clone();
                        NavItem::new(label, icon, label)
                            .folded(collapsed)
                            .active(matches!(self.page, Page::Library) && self.filter == filter)
                            .on_click(move |window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    if !this.busy {
                                        this.show_library(filter, window, cx);
                                    }
                                });
                            })
                    })),
            )
            .child(div().flex_1())
            .child(
                NavItem::new("settings", IconName::Settings, "Settings")
                    .folded(collapsed)
                    .active(matches!(self.page, Page::Settings(_)))
                    .on_click(move |window, cx| {
                        let _ = settings.update(cx, |this, cx| {
                            if !this.busy {
                                this.reset_scroll();
                                this.load_settings(window, cx);
                            }
                        });
                    }),
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
        let search = self.search.read(cx).text().to_lowercase();
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
        let view = cx.weak_entity();
        let columns = ((width + gap) / (166. + gap)).floor().max(1.);
        let card_width = ((width - (columns - 1.) * gap) / columns).floor();
        self.column()
            .child(
                self.row()
                    .justify_between()
                    .child(self.heading(title))
                    .child(
                        Button::new("add-app", "Add app")
                            .primary()
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            .child(
                self.row()
                    .py_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search).clearable().prefix(
                                Icon::new(IconName::Search)
                                    .size(IconSize::Sm)
                                    .color(self.appearance.colors.text_muted.hsla()),
                            ),
                        ),
                    )
                    .child(
                        SegmentedControl::new("library-view", if grid { "grid" } else { "list" })
                            .segment("list", "List", Some(IconName::List))
                            .segment("grid", "Grid", Some(IconName::LayoutGrid))
                            .on_change(move |selection, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.preferences.view = if selection.as_ref() == "grid" {
                                        View::Grid
                                    } else {
                                        View::List
                                    };
                                    this.persist_preferences(cx);
                                });
                            }),
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
                        this.child(EmptyState::new(
                            "empty-library",
                            if search.is_empty() {
                                IconName::Package
                            } else {
                                IconName::Search
                            },
                            if search.is_empty() {
                                "No apps in this view"
                            } else {
                                "No apps match your search"
                            },
                        ))
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
            Kind::AppImage => (IconName::Package, palette.accent_primary),
            Kind::Native => (IconName::Terminal, palette.accent_secondary),
            Kind::Web => (IconName::Globe, palette.accent_tertiary),
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
                None => Icon::new(symbol)
                    .scale(size * 0.55 / 16.)
                    .color(color)
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
        let mut card = self.card(self.row());
        card.style().border_widths = border_widths(self.appearance.borders.width);
        let card = card
            .border_color(gpui::transparent_black())
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
            .focus_visible(move |this| this.border_color(focus_color).bg(hover))
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
                        IconButton::new("back", IconName::ArrowLeft)
                            .variant(ButtonVariant::Ghost)
                            .tooltip("Back")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(self.heading(title)),
            )
            .when(new && !form.from_file_manager, |this| {
                this.child(self.row().children(Kind::ALL.map(|kind| {
                    Button::new(kind.label(), kind.label())
                        .when(form.draft.kind == kind, |button| button.primary())
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Page::Edit(current) = &mut this.page else {
                                return;
                            };
                            if current.draft.kind == kind {
                                return;
                            }
                            current.draft.kind = kind;
                            current
                                .source
                                .update(cx, |field, cx| field.set_text("", cx));
                            current.source.update(cx, |field, cx| {
                                field.set_placeholder(
                                    if kind == Kind::Web {
                                        "example.com"
                                    } else {
                                        "/path/to/file"
                                    },
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
                self.field(source_label, &form.source)
            } else {
                self.path_field("source", source_label, &form.source, false, cx)
            })
            .when(kind == Kind::AppImage, |this| {
                this.child(
                    self.row()
                        .child(
                            Button::new("keep-appimage", "Keep in place")
                                .when(!form.draft.move_appimage, |button| button.primary())
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.choose_move(false, cx)),
                                ),
                        )
                        .child(
                            Button::new("move-appimage", "Move to AppImages folder")
                                .when(form.draft.move_appimage, |button| button.primary())
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.choose_move(true, cx))),
                        ),
                )
            })
            .child(self.field("Name", &form.name))
            .child(self.field("Description", &form.description))
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
                        Button::new(
                            "reset-icon",
                            if form.draft.use_default_icon {
                                "Restore current icon"
                            } else {
                                "Use default icon"
                            },
                        )
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
                        Button::new(
                            "favicon",
                            if form.draft.fetch_favicon {
                                "Favicon lookup on"
                            } else {
                                "Favicon lookup off"
                            },
                        )
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
            .child(self.field("Terminal command (optional)", &form.command))
            .when(kind != Kind::Web, |this| {
                this.child(self.field("Launch arguments (optional)", &form.arguments))
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
                        Button::new(
                            "save-app",
                            if self.busy {
                                "Saving…"
                            } else if new {
                                "Add app"
                            } else {
                                "Save changes"
                            },
                        )
                        .primary()
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        Button::new("cancel-app", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel(window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .when(form.entry.is_some(), |this| {
                        this.child(
                            Button::new("remove-app", "Remove app")
                                .variant(ButtonVariant::Danger)
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
        state: &Entity<TextInput>,
        directory: bool,
        cx: &mut Context<Self>,
    ) -> FormField {
        let field = state.clone();
        FormField::new(label, label).child(
            self.row()
                .child(div().flex_1().min_w_0().child(Input::new(state)))
                .child(
                    Button::new(id, "Browse…")
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
                        IconButton::new("back", IconName::ArrowLeft)
                            .variant(ButtonVariant::Ghost)
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
                                Button::new("default-keep", "Keep in place")
                                    .when(!form.move_default, |this| this.primary())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_default_move(false, cx)
                                    })),
                            )
                            .child(
                                Button::new("default-move", "Move on import")
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

    fn settings_group(&self, title: &'static str) -> Section {
        Section::new(title)
            .p_5()
            .rounded(px(self.appearance.borders.card_radius))
            .bg(self.appearance.colors.surface.hsla())
            .when(self.appearance.borders.shadows, |section| {
                section.shadow_sm()
            })
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
                    Button::new(label, label).disabled(self.busy)
                        .when(form.use_included_colors == linked, |this| this.primary())
                        .when(form.use_included_colors != linked, |this| this.variant(ButtonVariant::Ghost))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.save_appearance(toml::toml! { use_included_colors = (linked) }, window, cx)
                                && let Page::Settings(form) = &mut this.page
                            {
                                form.use_included_colors = linked;
                                cx.notify();
                            }
                        }))
                }))))
            .when(form.use_included_colors, |this| this.child(
                self.setting_note("Theme includes can supply the palette. Keeping a color switches to local colors.")
            ))
            .child(div().flex().flex_col().gap_1().children(form.colors.chunks(2).map(|row| {
                self.row().children(row.iter().enumerate().map(|(index, color)| {
                    let key = color.key;
                    let value = color.value;
                    let label = color.label;
                    let outline = self.appearance.colors.border.unfocused.hsla().opacity(0.25);
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
                .child(div().w(px(120.)).child(self.field("Font size", &form.font_size)))
                .child(div().w(px(120.)).child(self.field("Corner radius", &form.corner_radius))))
    }

    fn open_color_picker(
        &mut self,
        key: &'static str,
        label: &'static str,
        value: gpui::Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_setting(window, cx);
        let Page::Settings(form) = &self.page else {
            return;
        };
        let palette = form
            .colors
            .iter()
            .map(|color| (color.label, color.value))
            .collect();
        let base = crate::config::load_with(
            &self.store.config_path(),
            toml::toml! { [appearance] use_included_colors = false },
        )
        .and_then(|mut config| match config.remove("appearance") {
            Some(toml::Value::Table(table)) => Ok(table),
            _ => anyhow::bail!("Missing appearance settings"),
        });
        let preview = match base {
            Ok(base) => base,
            Err(error) => {
                self.error(error);
                cx.notify();
                return;
            }
        };
        let original = self.appearance.clone();
        let picker = cx.new(|cx| ColorPicker::new(key, label, value, palette, cx));
        cx.subscribe_in(
            &picker,
            window,
            move |this, active, event: &PickerEvent, window, cx| {
                if this.color_picker.as_ref() != Some(active) {
                    return;
                }
                if let PickerEvent::Preview(key, value) = *event {
                    let mut draft = preview.clone();
                    crate::config::merge(&mut draft, color_patch(key, value));
                    if let Ok(appearance) = Appearance::from_table(draft) {
                        this.set_appearance(appearance, window, cx);
                    }
                    return;
                }

                this.color_picker = None;
                if let PickerEvent::Apply(key, picked) = *event
                    && encode_color(picked) != encode_color(value)
                    && this.save_appearance(color_patch(key, picked), window, cx)
                    && let Page::Settings(form) = &mut this.page
                    && let Some(color) = form.colors.iter_mut().find(|color| color.key == key)
                {
                    color.value = picked;
                    form.use_included_colors = false;
                    color.focus.focus(window, cx);
                } else if let Err(error) = this.refresh_appearance(window, cx) {
                    this.set_appearance(original.clone(), window, cx);
                    this.error(error);
                }
                cx.notify();
            },
        )
        .detach();
        self.color_picker = Some(picker);
        cx.notify();
    }

    fn choose_default_move(&mut self, move_default: bool, cx: &mut Context<Self>) {
        match self.store.save_settings(
            Some(toml::toml! {
                [image]
                move_to_directory = (move_default)
            }),
            None,
        ) {
            Ok(()) => {
                if let Page::Settings(form) = &mut self.page {
                    form.move_default = move_default;
                }
            }
            Err(error) => self.error(error),
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
                        Button::new(
                            "keep-remove",
                            if can_delete {
                                "Remove and keep file"
                            } else {
                                "Remove app"
                            },
                        )
                        .variant(ButtonVariant::Danger)
                        .disabled(self.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.remove(false, window, cx)),
                        ),
                    )
                    .when(can_delete, |this| {
                        this.child(
                            Button::new("delete-remove", "Remove and delete file")
                                .variant(ButtonVariant::Danger)
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.remove(true, window, cx)
                                    }),
                                ),
                        )
                    })
                    .child(
                        Button::new("cancel-remove", "Cancel")
                            .variant(ButtonVariant::Ghost)
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
        event: &gpui::ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gpui::ScrollDelta::Lines(delta) = event.delta else {
            self.scroll_motion = None;
            return;
        };
        let limit = self.scroll.max_offset().y;
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
                    self.focus.focus(window, cx);
                }
            }
            Page::Edit(_) => {
                self.next_import(window, cx);
                if self.queue.is_empty() && matches!(self.page, Page::Library) {
                    self.focus.focus(window, cx);
                }
            }
            _ => {
                self.show_library(self.filter, window, cx);
                self.focus.focus(window, cx);
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
        if event.keystroke.key == "escape" && !modifiers.modified() {
            self.shortcut("escape", window, cx);
            return;
        }
        if self.color_picker.is_some() {
            return;
        }
        let focus_free = window.focused(cx).is_none_or(|handle| handle == self.focus);
        match event.keystroke.key.as_str() {
            "enter" if !modifiers.modified() && focus_free => match self.page {
                Page::Edit(_) => self.save(window, cx),
                Page::Settings(_) => self.flush_setting(window, cx),
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
        if let Some(picker) = &self.color_picker {
            if key != "enter" {
                return;
            }
            picker.update(cx, |picker, cx| picker.apply(window, cx));
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
                window.focus(&self.search.read(cx).focus().clone(), cx);
            }
            "enter" => match self.page {
                Page::Edit(_) => self.save(window, cx),
                Page::Settings(_) => self.flush_setting(window, cx),
                _ => return,
            },
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn toast_widget(&self, toast: &Toast, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = &self.appearance.colors;
        let (accent, icon_color, icon) = if toast.error {
            (palette.error, palette.error_text, IconName::TriangleAlert)
        } else {
            (palette.accent_primary, palette.accent_text, IconName::Check)
        };
        let accent = accent.hsla();
        let ratio = toast.remaining.as_secs_f32() / toast.duration.as_secs_f32();
        let container = self
            .row()
            .px_3()
            .py_2()
            .min_h(px(44.))
            .child(
                div()
                    .size_6()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(accent)
                    .child(Icon::new(icon).size(IconSize::Sm).color(icon_color.hsla())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .child(toast.message.clone()),
            )
            .child(div().size_6().flex_none().when(toast.paused, |this| {
                this.child(
                    IconButton::new("dismiss-toast", IconName::X)
                        .size(ControlSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .tooltip("Dismiss notification")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        })),
                )
            }));
        self.card(div())
            .absolute()
            .bottom_4()
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
            .bg(self.appearance.colors.background.hsla())
            .font_family(cx.theme().font_family.clone())
            .text_color(self.appearance.colors.text_primary.hsla())
            .text_size(px(self.appearance.typography.font_size));
        root.style().border_widths = border_widths(self.appearance.borders.window_width);
        let palette = &self.appearance.colors.border;
        let body = root
            .border_color(if window.is_window_active() {
                palette.focused.hsla()
            } else {
                palette.outer.hsla()
            })
            .id("manager")
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
                            .child(
                                div()
                                    .w(px(width))
                                    .mx_auto()
                                    .on_scroll(cx.listener(Self::scroll_wheel))
                                    .child(body),
                            ),
                    )
                    .when_some(self.toast.as_ref(), |this, toast| {
                        this.child(self.toast_widget(toast, cx))
                    }),
            )
            .when_some(self.color_picker.as_ref(), |this, picker| {
                this.child(picker.clone())
            });
        div()
            .size_full()
            .on_key_down(cx.listener(Self::keyboard))
            .child(FocusScope::new(&self.focus).root().size_full().child(body))
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
    name: Entity<TextInput>,
    source: Entity<TextInput>,
    description: Entity<TextInput>,
    entry: Option<Entry>,
    icon: Entity<TextInput>,
    command: Entity<TextInput>,
    arguments: Entity<TextInput>,
    directory: Entity<TextInput>,
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
                let mut input = TextInput::new(window, cx)
                    .multi_line(2, 6)
                    .placeholder("Optional description");
                input.set_text(draft.description.clone(), cx);
                input
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
        draft.name = self.name.read(cx).text().to_string();
        draft.description = self.description.read(cx).text().to_string();
        draft.source = self.source.read(cx).text().to_string();
        draft.icon = optional_input(&self.icon, cx).map(PathBuf::from);
        if draft.icon != self.draft.icon {
            draft.use_default_icon = draft.icon.is_none();
        }
        draft.terminal_command =
            optional_input(&self.command, cx).map(|value| value.trim().to_owned());
        if draft.kind != Kind::Web {
            draft.arguments = shell_words::split(self.arguments.read(cx).text())
                .context("Check the quotes in your launch arguments")?;
            draft.working_directory = optional_input(&self.directory, cx).map(PathBuf::from);
        }
        Ok(draft)
    }
}

struct ThemeColor {
    key: &'static str,
    label: &'static str,
    value: gpui::Hsla,
    focus: gpui::FocusHandle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextSetting {
    Browser,
    AppImages,
    Icons,
    WorkingDirectory,
    FontSize,
    CornerRadius,
}

struct SettingsForm {
    browser: Entity<TextInput>,
    folder: Entity<TextInput>,
    icons: Entity<TextInput>,
    working_directory: Entity<TextInput>,
    move_default: bool,
    use_included_colors: bool,
    colors: Vec<ThemeColor>,
    font_family: Entity<FontPicker>,
    font_size: Entity<TextInput>,
    corner_radius: Entity<TextInput>,
    saved_text: [String; 6],
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
        let font_family =
            cx.new(|cx| FontPicker::new(current_family.to_owned().into(), window, cx));
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
        let saved_text = [
            &browser,
            &folder,
            &icons,
            &working_directory,
            &font_size,
            &corner_radius,
        ]
        .map(|field| field.read(cx).text().trim().to_owned());
        Self {
            saved_text,
            move_default: settings.image.move_to_directory,
            use_included_colors: appearance
                .get("use_included_colors")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true),
            browser,
            folder,
            icons,
            working_directory,
            colors,
            font_family,
            font_size,
            corner_radius,
        }
    }

    fn text_fields(&self) -> [(TextSetting, &Entity<TextInput>); 6] {
        [
            (TextSetting::Browser, &self.browser),
            (TextSetting::AppImages, &self.folder),
            (TextSetting::Icons, &self.icons),
            (TextSetting::WorkingDirectory, &self.working_directory),
            (TextSetting::FontSize, &self.font_size),
            (TextSetting::CornerRadius, &self.corner_radius),
        ]
    }
}

fn color_patch(key: &str, color: gpui::Hsla) -> toml::Table {
    toml::Table::from_iter([
        ("use_included_colors".into(), false.into()),
        (
            "colors".into(),
            toml::Value::Table(toml::Table::from_iter([(
                key.into(),
                encode_color(color).into(),
            )])),
        ),
    ])
}

fn text_input(
    value: &str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TextInput> {
    cx.new(|cx| {
        let mut input = TextInput::new(window, cx).placeholder(placeholder);
        input.set_text(value, cx);
        input
    })
}

fn optional_input(state: &Entity<TextInput>, cx: &App) -> Option<String> {
    let value = state.read(cx).text().to_string();
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
