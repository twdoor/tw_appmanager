use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, ensure};
use gpui::{App, Hsla, Window, WindowBackgroundAppearance, px, rgba};
use gpui_component::Theme;
use serde::{Deserialize, Deserializer};
use toml::{Table, Value};

pub const DEFAULT_CONFIG: &str = include_str!("../config/appearance.toml");

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

#[derive(Default, Deserialize)]
#[serde(default)]
struct Includes {
    files: Vec<String>,
    optional: OptionalIncludes,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OptionalIncludes {
    files: Vec<String>,
}

impl Appearance {
    pub fn defaults() -> Self {
        toml::from_str(DEFAULT_CONFIG).expect("bundled appearance config is valid")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut merged: Table = toml::from_str(DEFAULT_CONFIG)?;
        let root = read(path, true)?.unwrap_or_else(|| DEFAULT_CONFIG.to_owned());
        merge_file(&mut merged, path, &root, 0)?;
        let appearance = Self::deserialize(Value::Table(merged))
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
        let theme = Theme::global_mut(cx);
        let colors = &mut theme.colors;
        colors.background = palette.background.hsla();
        colors.foreground = palette.text_primary.hsla();
        colors.muted = palette.surface.hsla();
        colors.muted_foreground = palette.text_muted.hsla();
        colors.border = palette.border.unfocused.hsla();
        colors.input = colors.border;
        colors.ring = palette.border.focused.hsla();
        colors.caret = colors.foreground;
        colors.selection = palette.accent_primary.hsla().opacity(0.38);
        colors.window_border = palette.border.outer.hsla();
        colors.title_bar = palette.sidebar.hsla();
        colors.title_bar_border = colors.border;
        colors.primary = palette.accent_primary.hsla();
        colors.primary_foreground = palette.accent_text.hsla();
        colors.primary_hover = colors.primary.blend(colors.foreground.opacity(0.12));
        colors.primary_active = colors.primary.blend(colors.background.opacity(0.18));
        colors.secondary = colors.muted;
        colors.secondary_foreground = colors.foreground;
        colors.secondary_hover = colors.secondary.blend(colors.foreground.opacity(0.10));
        colors.secondary_active = colors.secondary.blend(colors.foreground.opacity(0.16));
        colors.danger = palette.error.hsla();
        colors.danger_foreground = palette.error_text.hsla();
        colors.danger_hover = colors.danger.blend(colors.foreground.opacity(0.12));
        colors.danger_active = colors.danger.blend(colors.background.opacity(0.18));
        colors.accent = colors.secondary_hover;
        colors.accent_foreground = colors.foreground;
        colors.scrollbar = colors.background;
        colors.scrollbar_thumb = colors.border;
        colors.scrollbar_thumb_hover = colors.muted_foreground;
        theme.font_family = if self.typography.font_family.is_empty() {
            system_font.clone()
        } else {
            self.typography.font_family.clone().into()
        };
        theme.font_size = px(self.typography.font_size * (16. / 14.));
        theme.radius = px(self.borders.radius);
        theme.radius_lg = px(self.borders.card_radius);
        theme.shadow = self.borders.shadows;
        window.set_background_appearance(if palette.background.hsla().is_opaque() {
            WindowBackgroundAppearance::Opaque
        } else {
            WindowBackgroundAppearance::Transparent
        });
    }
}

fn read(path: &Path, optional: bool) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if optional && error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

fn merge_file(merged: &mut Table, path: &Path, text: &str, depth: usize) -> Result<()> {
    ensure!(
        depth < 16,
        "Appearance includes are nested too deeply at {}",
        path.display()
    );
    let mut layer: Table =
        toml::from_str(text).with_context(|| format!("Could not parse {}", path.display()))?;
    let includes = layer
        .remove("include")
        .map(Includes::deserialize)
        .transpose()
        .with_context(|| format!("Invalid includes in {}", path.display()))?
        .unwrap_or_default();
    merge(merged, layer);
    let parent = path
        .parent()
        .context("Appearance file has no parent directory")?;
    for (files, optional) in [(includes.files, false), (includes.optional.files, true)] {
        for file in files {
            let included = parent.join(file);
            if let Some(text) = read(&included, optional)? {
                merge_file(merged, &included, &text, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn merge(base: &mut Table, layer: Table) {
    for (key, value) in layer {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(base)), Value::Table(layer)) => merge(base, layer),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_new_optional_files_over_user_settings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = temp.path().join("appearance.toml");
        let generated = temp.path().join("generated.toml");
        fs::write(
            &config,
            r##"
[include.optional]
files = ["generated.toml"]
[colors]
background = "#112233"
[borders]
width = 3
"##,
        )?;
        let initial = Appearance::load(&config)?;
        assert_eq!(initial.colors.background, Color(0x112233ff));
        fs::write(
            &generated,
            r##"
[colors]
background = "#44556680"
[colors.border]
focused = "#AABBCC"
"##,
        )?;
        let synced = Appearance::load(&config)?;
        assert_eq!(synced.colors.background, Color(0x44556680));
        assert_eq!(synced.colors.border.focused, Color(0xaabbccff));
        assert_eq!(synced.borders, initial.borders);
        fs::write(&generated, "[colors]\nbackground = \"invalid\"")?;
        assert!(Appearance::load(&config).is_err());
        fs::remove_file(&generated)?;
        assert_eq!(Appearance::load(&config)?, initial);
        Ok(())
    }
}
