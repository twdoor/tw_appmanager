use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use gpui::{App, Asset, ImageCacheError, RenderImage, Window, img, prelude::*, px};

use crate::appearance::Color;

pub fn brand_icon(accent: Color) -> gpui::Img {
    let size = 24.;
    img(move |window: &mut Window, cx: &mut App| {
        let pixels = (size * window.scale_factor()).ceil().max(1.) as u32;
        window.use_asset::<BrandIcon>(&(pixels, accent.as_rgba_hex()), cx)
    })
    .size(px(size))
    .flex_none()
}

struct BrandIcon;

impl Asset for BrandIcon {
    type Source = (u32, u32);
    type Output = <VectorIcon as Asset>::Output;

    fn load(
        (pixels, accent): Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let accent: gpui::Hsla = gpui::rgba(accent).into();
            let shade = |gray: f32| {
                let rgb: gpui::Rgba = gpui::Hsla {
                    l: (accent.l + gray) / 2.,
                    ..accent
                }
                .into();
                let channel = |value: f32| (value * 255.).round() as u8;
                format!(
                    "#{:02x}{:02x}{:02x}",
                    channel(rgb.r),
                    channel(rgb.g),
                    channel(rgb.b)
                )
            };
            let svg = include_str!("../twck_albedo.svg")
                .replace("#5F5F5F", &shade(95. / 255.))
                .replace("#C2C2C2", &shade(194. / 255.));
            VectorIcon::rasterize(svg.as_bytes(), pixels)
        }
    }
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
        async move { Self::rasterize(&std::fs::read(path)?, pixels) }
    }
}

impl VectorIcon {
    fn rasterize(svg: &[u8], pixels: u32) -> <Self as Asset>::Output {
        let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
            .context("Could not parse SVG")?;
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
