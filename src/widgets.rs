use std::{borrow::Cow, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use gpui::{
    App, Asset, AssetSource, ClickEvent, Div, ElementId, ImageCacheError, IntoElement, RenderImage,
    RenderOnce, SharedString, Stateful, StyleRefinement, Window, div, img, prelude::*, px, svg,
};
use gpui_component::{ActiveTheme, tooltip::Tooltip};

pub struct Glyphs;

impl AssetSource for Glyphs {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let drawing = match path {
            "library" => "M4 5h16v14H4z M4 10h16 M9 10v9",
            "package" => "M12 3l8 4.5v9L12 21l-8-4.5v-9z M4 7.5l8 4.5 8-4.5 M12 12v9 M8 5.25l8 4.5",
            "terminal" => "M3 5h18v14H3z M7 9l3 3-3 3 M13 15h4",
            "web" => "M3 6h18v12H3z M3 10h18 M6 8h.01 M9 8h.01",
            "settings" => "M4 7h16 M4 12h16 M4 17h16 M8 5v4 M15 10v4 M11 15v4",
            "collapse" => "M3 5h18v14H3z M9 5v14 M16 9l-3 3 3 3",
            "expand" => "M3 5h18v14H3z M9 5v14 M13 9l3 3-3 3",
            "grid" => "M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z",
            "list" => "M9 6h11 M9 12h11 M9 18h11 M4 6h.01 M4 12h.01 M4 18h.01",
            "plus" => "M12 5v14 M5 12h14",
            "close" => "M6 6l12 12 M18 6L6 18",
            "check" => "M5 12l4 4L19 6",
            "alert" => "M12 8v5 M12 16h.01 M3 19h18L12 4z",
            "back" => "M19 12H5 M11 6l-6 6 6 6",
            _ => return Ok(None),
        };
        Ok(Some(Cow::Owned(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="{drawing}" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#
        ).into_bytes())))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub fn glyph(name: &'static str, size: f32) -> gpui::Svg {
    svg().path(name).size(px(size)).flex_none()
}

pub fn app_icon(path: PathBuf, size: f32) -> gpui::Img {
    let is_svg = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
    if !is_svg {
        return img(path)
            .size(px(size))
            .object_fit(gpui::ObjectFit::Contain);
    }
    img(move |window: &mut Window, cx: &mut App| {
        let pixels = (size * window.scale_factor()).ceil().max(1.) as u32;
        let version = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        window.use_asset::<VectorIcon>(&(path.clone(), pixels, version), cx)
    })
    .size(px(size))
    .object_fit(gpui::ObjectFit::Contain)
}

struct VectorIcon;

impl Asset for VectorIcon {
    type Source = (PathBuf, u32, Option<std::time::SystemTime>);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        (path, pixels, _): Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        Self::rasterize(path, pixels)
    }
}

impl VectorIcon {
    async fn rasterize(path: PathBuf, pixels: u32) -> <Self as Asset>::Output {
        let tree =
            resvg::usvg::Tree::from_data(&std::fs::read(path)?, &resvg::usvg::Options::default())?;
        let dimensions = tree.size();
        let scale = pixels as f32 / dimensions.width().max(dimensions.height());
        let width = (dimensions.width() * scale).ceil().max(1.) as u32;
        let height = (dimensions.height() * scale).ceil().max(1.) as u32;
        let mut buffer =
            resvg::tiny_skia::Pixmap::new(width, height).context("Invalid SVG dimensions")?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut buffer.as_mut(),
        );
        let png = buffer.encode_png().context("Could not render SVG")?;
        let mut image =
            image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.into_rgba8();
        for pixel in image.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Ok(Arc::new(RenderImage::new([image::Frame::new(image)])))
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Neutral,
    Primary,
    Ghost,
    Danger,
}

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Control {
    base: Stateful<Div>,
    id: ElementId,
    tone: Tone,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    tooltip: Option<SharedString>,
    disabled: bool,
    click: Option<Click>,
}

impl Control {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            base: div().id(id.clone()),
            id,
            tone: Tone::Neutral,
            label: None,
            icon: None,
            tooltip: None,
            disabled: false,
            click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }

    pub fn tooltip(mut self, label: impl Into<SharedString>) -> Self {
        self.tooltip = Some(label.into());
        self
    }

    pub fn primary(mut self) -> Self {
        self.tone = Tone::Primary;
        self
    }

    pub fn ghost(mut self) -> Self {
        self.tone = Tone::Ghost;
        self
    }

    pub fn danger(mut self) -> Self {
        self.tone = Tone::Danger;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.click = Some(Box::new(click));
        self
    }
}

impl Styled for Control {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for Control {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, hover_bg, hover_fg) = match self.tone {
            Tone::Neutral => (
                theme.secondary,
                theme.foreground,
                theme.secondary_hover,
                theme.foreground,
            ),
            Tone::Primary => (
                theme.primary,
                theme.primary_foreground,
                theme.primary_hover,
                theme.primary_foreground,
            ),
            Tone::Ghost => (
                theme.transparent,
                theme.muted_foreground,
                theme.secondary_hover,
                theme.foreground,
            ),
            Tone::Danger => (
                theme.transparent,
                theme.danger.blend(theme.foreground.opacity(0.45)),
                theme.danger,
                theme.danger_foreground,
            ),
        };
        let focus_ring = theme.ring;
        let radius = theme.radius;
        let foreground = if self.disabled { fg.opacity(0.5) } else { fg };
        let font_size = theme.font_size * 0.875;
        let focus = window
            .use_keyed_state(self.id, cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        self.base
            .flex()
            .items_center()
            .gap_2()
            .h_8()
            .when(self.label.is_none(), |this| this.w_8().justify_center())
            .when(self.label.is_some(), |this| this.px_3())
            .rounded(radius)
            .bg(bg)
            .text_color(foreground)
            .text_size(font_size)
            .border_color(gpui::transparent_black())
            .when(!self.disabled, |this| {
                this.track_focus(&focus.tab_stop(true))
                    .cursor_pointer()
                    .hover(move |this| this.bg(hover_bg).text_color(hover_fg))
                    .focus(move |this| this.border_color(focus_ring))
                    .when_some(self.click, |this, click| this.on_click(click))
            })
            .when_some(self.tooltip, |this, text| {
                this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            })
            .when_some(self.icon, |this, icon| {
                this.child(glyph(icon, 16.).text_color(foreground))
            })
            .when_some(self.label, |this, label| {
                this.child(div().min_w_0().truncate().child(label))
            })
    }
}
