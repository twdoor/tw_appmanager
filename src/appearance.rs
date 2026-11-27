use std::path::Path;

use anyhow::{Context, Result, ensure};
use ely_core::theme::{Palette, Theme};
use gpui::{App, Hsla, Window, WindowBackgroundAppearance, rgba};
use serde::{Deserialize, Deserializer};

use crate::config;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Appearance {
    pub colors: Colors,
    pub typography: Typography,
    pub layout: Layout,
    pub borders: Borders,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Colors {
    pub background: Color,
    pub sidebar: Color,
    pub surface: Color,
    pub text_primary: Color,
    pub text_muted: Color,
    pub accent_primary: Color,
    pub accent_secondary: Color,
    pub accent_tertiary: Color,
    pub accent_text: Color,
    pub error: Color,
    pub error_text: Color,
    pub border: BorderColors,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BorderColors {
    pub focused: Color,
    pub unfocused: Color,
    pub outer: Color,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Typography {
    pub font_family: String,
    pub font_size: f32,
    pub heading_size: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Layout {
    pub sidebar_width: f32,
    pub page_padding: f32,
    pub content_width: f32,
    pub spacing: f32,
    pub icon_size: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Borders {
    pub width: f32,
    pub window_width: f32,
    pub radius: f32,
    pub card_radius: f32,
    pub shadows: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color(u32);

impl Color {
    pub fn hsla(self) -> Hsla {
        rgba(self.0).into()
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let text = String::deserialize(deserializer)?;
        let hex = text.strip_prefix('#').unwrap_or_default();
        if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(D::Error::custom(format!(
                "expected #RRGGBB or #RRGGBBAA, got {text:?}"
            )));
        }
        let value = u32::from_str_radix(hex, 16).map_err(D::Error::custom)?;
        Ok(Self(if hex.len() == 6 {
            (value << 8) | 0xff
        } else {
            value
        }))
    }
}

impl Appearance {
    pub fn defaults() -> Self {
        let default =
            toml::from_str::<toml::Table>(config::DEFAULT_CONFIG).expect("valid bundled config");
        Self::deserialize(default["appearance"].clone()).expect("valid bundled appearance")
    }

    pub fn from_table(table: toml::Table) -> Result<Self> {
        let appearance = Self::deserialize(toml::Value::Table(table))?;
        appearance.validate()?;
        Ok(appearance)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let appearance = Self::deserialize(
            config::load(path)?
                .remove("appearance")
                .context("Missing [appearance]")?,
        )
        .with_context(|| format!("Invalid appearance in {}", path.display()))?;
        appearance.validate()?;
        Ok(appearance)
    }

    fn validate(&self) -> Result<()> {
        let limits = [
            ("typography.font_size", self.typography.font_size, 9., 32.),
            (
                "typography.heading_size",
                self.typography.heading_size,
                12.,
                64.,
            ),
            (
                "layout.sidebar_width",
                self.layout.sidebar_width,
                100.,
                300.,
            ),
            ("layout.page_padding", self.layout.page_padding, 0., 64.),
            (
                "layout.content_width",
                self.layout.content_width,
                300.,
                2000.,
            ),
            ("layout.spacing", self.layout.spacing, 2., 24.),
            ("layout.icon_size", self.layout.icon_size, 16., 96.),
            ("borders.width", self.borders.width, 0., 12.),
            ("borders.window_width", self.borders.window_width, 0., 16.),
            ("borders.radius", self.borders.radius, 0., 48.),
            ("borders.card_radius", self.borders.card_radius, 0., 48.),
        ];
        for (name, value, min, max) in limits {
            ensure!(
                (min..=max).contains(&value),
                "{name} must be between {min} and {max}"
            );
        }
        Ok(())
    }

    pub fn apply(&self, system_font: &gpui::SharedString, window: &mut Window, cx: &mut App) {
        let palette = &self.colors;
        Theme::update(cx, |theme| {
            let mut colors = Palette::dark(false);
            colors.bg = palette.background.hsla();
            colors.overlay = palette.surface.hsla();
            colors.sunken = palette.sidebar.hsla();
            colors.fg = palette.text_primary.hsla();
            colors.surface = colors.overlay.blend(colors.fg.opacity(0.10));
            colors.tooltip_bg = colors.surface;
            colors.tooltip_fg = colors.fg;
            colors.fg_muted = palette.text_muted.hsla();
            colors.fg_subtle = colors.fg_muted;
            colors.fg_disabled = colors.fg_muted.opacity(0.65);
            colors.border = gpui::transparent_black();
            colors.border_strong = colors.border;
            colors.focus = palette.border.focused.hsla();
            colors.accent = palette.accent_primary.hsla();
            colors.on_accent = palette.accent_text.hsla();
            colors.accent_hover = colors.accent.blend(colors.fg.opacity(0.12));
            colors.hover = colors.surface.blend(colors.accent.opacity(0.09));
            colors.active = colors.surface.blend(colors.accent.opacity(0.16));
            colors.selection = colors.accent.opacity(0.38);
            colors.danger = palette.error.hsla();
            theme.colors = colors;
            theme.font_family = if self.typography.font_family.is_empty() {
                system_font.clone()
            } else {
                self.typography.font_family.clone().into()
            };
            theme.font_scale = self.typography.font_size / 13.;
            theme.radius_scale = self.borders.radius / 6.;
        });
        window.set_background_appearance(if palette.background.hsla().is_opaque() {
            WindowBackgroundAppearance::Opaque
        } else {
            WindowBackgroundAppearance::Transparent
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn layers_new_optional_files_over_user_settings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = temp.path().join("config.toml");
        let generated = temp.path().join("generated.toml");
        fs::write(
            &config,
            r##"
[include.optional]
files = ["generated.toml"]
[appearance.colors]
background = "#112233"
[appearance.borders]
width = 3
"##,
        )?;
        let initial = Appearance::load(&config)?;
        assert_eq!(initial.colors.background, Color(0x112233ff));
        fs::write(
            &generated,
            r##"
[appearance.colors]
background = "#44556680"
[appearance.colors.border]
focused = "#AABBCC"
"##,
        )?;
        let synced = Appearance::load(&config)?;
        assert_eq!(synced.colors.background, Color(0x44556680));
        assert_eq!(synced.colors.border.focused, Color(0xaabbccff));
        assert_eq!(synced.borders, initial.borders);
        fs::write(&generated, "[appearance.colors]\nbackground = \"invalid\"")?;
        assert!(Appearance::load(&config).is_err());
        fs::remove_file(&generated)?;
        assert_eq!(Appearance::load(&config)?, initial);
        Ok(())
    }
}
