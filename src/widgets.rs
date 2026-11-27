use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use gpui::{App, Asset, ImageCacheError, RenderImage, Window, img, prelude::*, px};

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
            resvg::usvg::Tree::from_data(&std::fs::read(path)?, &resvg::usvg::Options::default())
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
