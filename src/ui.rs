use std::{cell::RefCell, collections::VecDeque, path::PathBuf, rc::Rc, time::Duration};

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext, Application, Bounds, Context, Entity, ImageSource, IntoElement,
    PathPromptOptions, Render, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
    div, img, prelude::*, px, size,
};
use gpui_component::{
    Disableable, Root, Theme,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
};

use crate::{
    appearance::Appearance,
    appimage::detect_executable,
    desktop::ID,
    library::{Draft, Entry, Kind, Settings, Store, Target},
};

pub fn run(store: Store, paths: Vec<PathBuf>) -> Result<()> {
    let startup_error = Rc::new(RefCell::new(None));
    let window_error = startup_error.clone();
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1020.), px(700.)), cx);
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
    Edit(Form),
    Settings(SettingsForm),
    Remove(Entry),
}

struct Manager {
    store: Store,
    apps: Vec<Entry>,
    page: Page,
    filter: Option<Kind>,
    search: Entity<InputState>,
    queue: VecDeque<PathBuf>,
    status: Option<(String, bool)>,
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
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let path = store.appearance_path();
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
        let mut this = Self {
            store,
            apps: Vec::new(),
            page: Page::Library,
            filter: None,
            search,
            queue,
            status: None,
            busy: false,
            appearance,
            appearance_error,
        };
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        this.reload();
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
        self.status = Some((format!("{error:#}"), true));
    }

    fn next_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                    self.page = Page::Edit(self.new_form(kind, source.to_owned(), window, cx));
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
                Ok(settings) => draft.move_appimage = settings.move_appimages_by_default,
                Err(error) => self.error(error),
            }
        }
        let form = Form::new(draft, window, cx);
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
        self.page = Page::Library;
        self.status = None;
        self.queue.clear();
        self.reload();
        cx.notify();
    }

    fn add(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        self.page = Page::Edit(self.new_form(
            self.filter.unwrap_or(Kind::AppImage),
            String::new(),
            window,
            cx,
        ));
        cx.notify();
    }

    fn show_settings(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        match self.store.settings() {
            Ok(settings) => self.page = Page::Settings(SettingsForm::new(settings, window, cx)),
            Err(error) => self.error(error),
        }
        cx.notify();
    }

    fn edit(&mut self, entry: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        let mut draft = Draft::new(entry.target.kind(), entry.target.display());
        draft.id = Some(entry.id.clone());
        draft.name = entry.name.clone();
        draft.terminal_command = entry.terminal_command.clone();
        draft.arguments = entry.arguments.clone();
        draft.working_directory = entry.working_directory.clone();
        self.page = Page::Edit(Form::new(draft, window, cx));
        cx.notify();
    }

    fn save(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                this.status = Some((format!("Saved {}", app.name), false));
                this.reload();
                this.filter = None;
                this.search
                    .update(cx, |input, cx| input.set_value("", window, cx));
                this.next_import(window, cx);
            },
        );
    }

    fn save_settings(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Settings(form) = &self.page else {
            return;
        };
        let settings = Settings {
            appimage_directory: PathBuf::from(form.folder.read(cx).value().as_str()),
            browser: PathBuf::from(form.browser.read(cx).value().as_str()),
            move_appimages_by_default: form.move_default,
        };
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.save_settings(settings),
            |this, (), _, _| {
                this.status = Some(("Settings saved".into(), false));
            },
        );
    }

    fn import_legacy(&mut self, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.import_legacy_webapps(),
            |this, count, _, _| {
                let message = match count {
                    0 => "No new web apps to import".into(),
                    1 => "Imported 1 web app".into(),
                    _ => format!("Imported {count} web apps"),
                };
                this.status = Some((message, false));
                this.reload();
            },
        );
    }

    fn remove(&mut self, delete_file: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Remove(entry) = &self.page else {
            return;
        };
        let id = entry.id.clone();
        let store = self.store.clone();
        self.perform(
            window,
            cx,
            move || store.remove(&id, delete_file),
            |this, name, _, _| {
                this.status = Some((format!("Removed {name}"), false));
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
        self.status = None;
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

    fn button(&self, id: impl Into<gpui::ElementId>) -> Button {
        let mut button = Button::new(id);
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

    fn frame(&self, mut element: gpui::Div) -> gpui::Div {
        element.style().border_widths = border_widths(self.appearance.borders.width);
        element
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filters = [
            (None, "All apps"),
            (Some(Kind::AppImage), "AppImages"),
            (Some(Kind::Native), "Linux programs"),
            (Some(Kind::Web), "Web apps"),
        ];
        div()
            .flex()
            .flex_col()
            .w(px(self.appearance.layout.sidebar_width))
            .flex_none()
            .h_full()
            .p_4()
            .gap(px(self.appearance.layout.spacing * 3.))
            .bg(self.appearance.colors.sidebar.hsla())
            .map(|mut this| {
                this.style().border_widths.right = Some(px(self.appearance.borders.width).into());
                this
            })
            .border_color(self.appearance.colors.border.unfocused.hsla())
            .child(
                div()
                    .text_size(px(self.appearance.typography.font_size + 2.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("TW App Manager"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(self.appearance.layout.spacing * 0.5))
                    .children(filters.into_iter().map(|(filter, label)| {
                        let selected = matches!(self.page, Page::Library) && self.filter == filter;
                        self.button(label)
                            .label(label)
                            .when(!selected, |button| button.ghost())
                            .w_full()
                            .justify_start()
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.show_library(filter, cx)),
                            )
                    })),
            )
            .child(div().flex_1())
            .child(
                self.button("settings")
                    .label("Settings")
                    .w_full()
                    .justify_start()
                    .when(!matches!(self.page, Page::Settings(_)), |button| {
                        button.ghost()
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(Self::show_settings)),
            )
    }

    fn library(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.search.read(cx).value().to_lowercase();
        let apps: Vec<_> = self
            .apps
            .iter()
            .filter(|app| self.filter.is_none_or(|kind| app.target.kind() == kind))
            .filter(|app| {
                search.is_empty()
                    || app.name.to_lowercase().contains(&search)
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
        let empty = if !search.is_empty() {
            "No apps match your search."
        } else {
            "No apps here yet. Select Add app to get started."
        };
        self.column()
            .child(
                self.row()
                    .justify_between()
                    .child(self.heading(title))
                    .child(
                        self.button("add-app")
                            .primary()
                            .label("Add app")
                            .on_click(cx.listener(Self::add)),
                    ),
            )
            .child(self.input(&self.search).w_full())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(self.appearance.layout.spacing))
                    .when(apps.is_empty(), |this| {
                        this.child(
                            div()
                                .map(|this| rounded(this, self.appearance.borders.card_radius))
                                .map(|this| self.frame(this))
                                .border_color(self.appearance.colors.border.unfocused.hsla())
                                .bg(self.appearance.colors.surface.hsla())
                                .p_5()
                                .text_color(self.appearance.colors.text_muted.hsla())
                                .child(empty),
                        )
                    })
                    .children(apps.into_iter().map(|app| self.app_row(app, cx))),
            )
    }

    fn app_row(&self, app: &Entry, cx: &mut Context<Self>) -> gpui::Div {
        let edit_app = app.clone();
        let remove_app = app.clone();
        let id = app.id.clone();
        let kind = app.target.kind();
        let palette = &self.appearance.colors;
        let (symbol, color) = match kind {
            Kind::AppImage => ("A", palette.accent_primary),
            Kind::Native => ("L", palette.accent_secondary),
            Kind::Web => ("W", palette.accent_tertiary),
        };
        let color = color.hsla();
        let tile = palette.surface.hsla().blend(color.opacity(0.18));
        let mut subtitle = kind.label().to_owned();
        if let Some(command) = &app.terminal_command {
            subtitle.push_str(&format!("  ·  {command}"));
        }

        self.row()
            .p_3()
            .gap(px(self.appearance.layout.spacing * 1.5))
            .map(|this| rounded(this, self.appearance.borders.card_radius))
            .bg(self.appearance.colors.surface.hsla())
            .map(|this| self.frame(this))
            .border_color(self.appearance.colors.border.unfocused.hsla())
            .child(
                div()
                    .size(px(self.appearance.layout.icon_size))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|this| rounded(this, self.appearance.borders.radius))
                    .bg(tile)
                    .text_color(color)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(match &app.icon {
                        Some(path) => img(path.clone())
                            .size(px(self.appearance.layout.icon_size * 0.725))
                            .into_any_element(),
                        None => div().child(symbol).into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(self.appearance.colors.text_muted.hsla())
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .child(
                self.button(SharedString::from(format!("launch-{}", app.id)))
                    .label("Launch")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.status = None;
                        if let Err(error) = this.store.launch(&id, &[], false) {
                            this.error(error);
                        }
                        cx.notify();
                    })),
            )
            .child(
                self.button(SharedString::from(format!("edit-{}", app.id)))
                    .ghost()
                    .label("Edit")
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit(&edit_app, window, cx)),
                    ),
            )
            .child(
                self.button(SharedString::from(format!("remove-{}", app.id)))
                    .ghost()
                    .label("Remove")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.status = None;
                        this.page = Page::Remove(remove_app.clone());
                        cx.notify();
                    })),
            )
    }

    fn editor(&self, form: &Form, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = form.draft.kind;
        let new = form.draft.id.is_none();
        let source_label = match kind {
            Kind::AppImage => "AppImage file",
            Kind::Native => "x86-64 executable",
            Kind::Web => "Website URL",
        };
        let title = if new { "Add app" } else { "Edit app" };
        self.column()
            .child(self.heading(title))
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
                                current.draft.move_appimage = settings.move_appimages_by_default;
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
            .child(self.path_field("icon", "Icon file (optional)", &form.icon, false, cx))
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
                            .on_click(cx.listener(Self::save)),
                    )
                    .child(
                        self.button("cancel-app")
                            .ghost()
                            .label("Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.status = None;
                                this.next_import(window, cx);
                                cx.notify();
                            })),
                    ),
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
            .child(self.heading("Settings"))
            .child(self.path_field(
                "appimage-directory",
                "AppImages folder",
                &form.folder,
                true,
                cx,
            ))
            .child(
                self.row()
                    .child(
                        self.button("default-keep")
                            .label("Keep by default")
                            .when(!form.move_default, |button| button.primary())
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_default_move(false, cx)),
                            ),
                    )
                    .child(
                        self.button("default-move")
                            .label("Move by default")
                            .when(form.move_default, |button| button.primary())
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.choose_default_move(true, cx)),
                            ),
                    ),
            )
            .child(self.path_field("web-browser", "Web app browser", &form.browser, false, cx))
            .child(
                div().text_color(self.appearance.colors.text_muted.hsla()).text_sm().child(
                    "Choose an executable that supports app mode, such as Helium or Chromium.",
                ),
            )
            .child(
                self.row().child(
                    self.button("save-settings")
                        .primary()
                        .label("Save settings")
                        .disabled(self.busy)
                        .on_click(cx.listener(Self::save_settings)),
                ),
            )
            .child(
                div()
                    .map(|mut this| {
                    this.style().border_widths.top = Some(px(self.appearance.borders.width).into());
                    this
                })
                    .border_color(self.appearance.colors.border.unfocused.hsla())
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap(px(self.appearance.layout.spacing))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Existing web apps"),
                    )
                    .child(div().text_color(self.appearance.colors.text_muted.hsla()).text_sm().child(
                        "Bring apps created with the original webapp command into this library.",
                    ))
                    .child(
                        self.row().child(
                            self.button("import-webapps")
                                .label("Import web apps")
                                .disabled(self.busy)
                                .on_click(cx.listener(Self::import_legacy)),
                        ),
                    ),
            )
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
                            .on_click(
                                cx.listener(|this, _, _, cx| this.show_library(this.filter, cx)),
                            ),
                    ),
            )
    }
}

impl Render for Manager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.page {
            Page::Library => self.library(cx).into_any_element(),
            Page::Edit(form) => self.editor(form, cx).into_any_element(),
            Page::Settings(form) => self.settings(form, cx).into_any_element(),
            Page::Remove(app) => self.confirm_remove(app, cx).into_any_element(),
        };
        div()
            .flex()
            .size_full()
            .map(|mut element| {
                element.style().border_widths = border_widths(self.appearance.borders.window_width);
                let palette = &self.appearance.colors.border;
                element.border_color(if window.is_window_active() {
                    palette.focused.hsla()
                } else {
                    palette.outer.hsla()
                })
            })
            .text_color(self.appearance.colors.text_primary.hsla())
            .text_size(px(self.appearance.typography.font_size))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .when_some(self.appearance_error.clone(), |this, message| {
                        this.child(
                            div()
                                .px(px(self.appearance.layout.page_padding))
                                .py_2()
                                .bg(self.appearance.colors.error.hsla())
                                .text_color(self.appearance.colors.error_text.hsla())
                                .text_sm()
                                .child(format!("Appearance: {message}")),
                        )
                    })
                    .when_some(self.status.clone(), |this, (message, is_error)| {
                        this.child(
                            div()
                                .px(px(self.appearance.layout.page_padding))
                                .py_2()
                                .map(|mut this| {
                                    this.style().border_widths.bottom =
                                        Some(px(self.appearance.borders.width).into());
                                    this
                                })
                                .border_color(self.appearance.colors.border.unfocused.hsla())
                                .bg(if is_error {
                                    self.appearance.colors.error.hsla()
                                } else {
                                    self.appearance.colors.surface.hsla()
                                })
                                .text_sm()
                                .text_color(if is_error {
                                    self.appearance.colors.error_text.hsla()
                                } else {
                                    self.appearance.colors.accent_primary.hsla()
                                })
                                .child(message),
                        )
                    })
                    .when(self.busy, |this| {
                        this.child(
                            div()
                                .h(px(2.))
                                .bg(self.appearance.colors.accent_primary.hsla()),
                        )
                    })
                    .child(
                        div()
                            .id("page-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(self.appearance.layout.page_padding))
                            .child(
                                div()
                                    .max_w(px(self.appearance.layout.content_width))
                                    .mx_auto()
                                    .child(body),
                            ),
                    ),
            )
    }
}

struct Form {
    draft: Draft,
    from_file_manager: bool,
    name: Entity<InputState>,
    source: Entity<InputState>,
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
            source: text_input(&draft.source, source_hint, window, cx),
            icon: text_input("", "Choose an image", window, cx),
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

    fn read(&self, cx: &App) -> Result<Draft> {
        let mut draft = self.draft.clone();
        draft.name = self.name.read(cx).value().to_string();
        draft.source = self.source.read(cx).value().to_string();
        draft.icon = optional_input(&self.icon, cx).map(PathBuf::from);
        if draft.icon.is_some() {
            draft.use_default_icon = false;
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

struct SettingsForm {
    browser: Entity<InputState>,
    folder: Entity<InputState>,
    move_default: bool,
}

impl SettingsForm {
    fn new(settings: Settings, window: &mut Window, cx: &mut App) -> Self {
        Self {
            browser: text_input(
                &settings.browser.to_string_lossy(),
                "/usr/bin/helium",
                window,
                cx,
            ),
            folder: text_input(
                &settings.appimage_directory.to_string_lossy(),
                "~/Applications",
                window,
                cx,
            ),
            move_default: settings.move_appimages_by_default,
        }
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
