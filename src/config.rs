use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::transaction::Transaction;

pub const DEFAULT_CONFIG: &str = include_str!("../config/config.toml");

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    List,
    Grid,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub view: View,
    pub sidebar_collapsed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
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

pub fn migrate(path: &Path, transaction: &mut Transaction) -> Result<()> {
    let directory = path.parent().context("Missing config directory")?;
    let legacy_settings = directory.join("settings.toml");
    let legacy_appearance = directory.join("appearance.toml");
    let mut document: toml_edit::DocumentMut = DEFAULT_CONFIG.parse()?;
    let settings = read(&legacy_settings, true)?;
    if let Some(text) = &settings {
        let mut settings = parse(&legacy_settings, text)?;
        upgrade_apps(&mut settings)?;
        if let Some(apps) = settings.get("apps").and_then(Value::as_table) {
            document["apps"] = table_item(apps)?;
        }
    }
    let appearance = read(&legacy_appearance, true)?;
    if let Some(text) = &appearance {
        let mut old = parse(&legacy_appearance, text)?;
        if let Some(includes) = old.remove("include") {
            document["include"] =
                table_item(includes.as_table().context("Expected an include table")?)?;
        }
        let mut defaults = parse(path, DEFAULT_CONFIG)?;
        let appearance = defaults["appearance"]
            .as_table_mut()
            .context("Missing appearance defaults")?;
        merge(appearance, old);
        document["appearance"] = table_item(appearance)?;
    }
    transaction.write(path, document.to_string().as_bytes(), 0o600, false)?;
    for (old, text) in [(legacy_settings, settings), (legacy_appearance, appearance)] {
        if let Some(text) = text {
            transaction.write(
                &old.with_extension("toml.bak"),
                text.as_bytes(),
                0o600,
                false,
            )?;
            transaction.remove(&old)?;
        }
    }
    Ok(())
}

pub fn table_item(values: &Table) -> Result<toml_edit::Item> {
    let mut table = toml_edit::Table::new();
    for (key, value) in values {
        let item = match value {
            Value::Table(nested) => table_item(nested)?,
            _ => toml_edit::Item::Value(value.serialize(toml_edit::ser::ValueSerializer::new())?),
        };
        table.insert(key, item);
    }
    table.set_implicit(true);
    Ok(toml_edit::Item::Table(table))
}

pub fn update_table(item: &mut toml_edit::Item, values: &Table) -> Result<()> {
    if item.is_none() {
        *item = table_item(values)?;
        return Ok(());
    }
    for (key, value) in values {
        match value {
            Value::Table(children) => update_table(&mut item[key], children)?,
            _ => {
                let mut edited = value.serialize(toml_edit::ser::ValueSerializer::new())?;
                if let Some(current) = item.get(key).and_then(toml_edit::Item::as_value) {
                    *edited.decor_mut() = current.decor().clone();
                }
                item[key] = toml_edit::Item::Value(edited);
            }
        }
    }
    Ok(())
}

pub fn upgrade_existing(path: &Path, transaction: &mut Transaction) -> Result<()> {
    let mut doc = document(path)?;
    let mut settings = parse(path, &doc.to_string())?;
    if !upgrade_apps(&mut settings)? {
        return Ok(());
    }
    for (old, _, _) in LEGACY_KEYS {
        doc.remove(old);
    }
    doc["apps"] = table_item(
        settings["apps"]
            .as_table()
            .context("Invalid apps settings")?,
    )?;
    transaction.write(path, doc.to_string().as_bytes(), 0o600, true)
}

const LEGACY_KEYS: [(&str, &str, &str); 3] = [
    ("appimage_directory", "image", "move_directory"),
    ("move_appimages_by_default", "image", "move_to_directory"),
    ("browser", "web", "browser_path"),
];

fn upgrade_apps(root: &mut Table) -> Result<bool> {
    let mut changed = false;
    for (old, section, key) in LEGACY_KEYS {
        let Some(value) = root.remove(old) else {
            continue;
        };
        changed = true;
        let apps = root
            .entry("apps")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .context("Expected an apps table")?;
        let target = apps
            .entry(section)
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .context("Expected an app type table")?;
        target.entry(key).or_insert(value);
    }
    if changed {
        let mut defaults: Table = toml::from_str(DEFAULT_CONFIG)?;
        let mut apps = defaults
            .remove("apps")
            .and_then(|value| value.as_table().cloned())
            .context("Missing app defaults")?;
        let configured = root
            .remove("apps")
            .and_then(|value| value.as_table().cloned())
            .context("Expected an apps table")?;
        merge(&mut apps, configured);
        root.insert("apps".into(), Value::Table(apps));
    }
    Ok(changed)
}

pub fn local_appearance(path: &Path) -> Result<Table> {
    let mut defaults = parse(path, DEFAULT_CONFIG)?;
    merge(&mut defaults, parse(path, &fs::read_to_string(path)?)?);
    match defaults.remove("appearance") {
        Some(Value::Table(appearance)) => Ok(appearance),
        _ => anyhow::bail!("Expected an appearance table"),
    }
}

pub fn load(path: &Path) -> Result<Table> {
    let mut merged = parse(path, DEFAULT_CONFIG)?;
    let root = read(path, false)?.context("Missing config")?;
    let document = parse(path, &root)?;
    let included_colors = document
        .get("appearance")
        .and_then(|appearance| appearance.get("use_included_colors"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    merge_file(&mut merged, path, document, 0, included_colors)?;
    Ok(merged)
}

pub fn document(path: &Path) -> Result<toml_edit::DocumentMut> {
    let raw = fs::read_to_string(path)?;
    raw.parse()
        .with_context(|| format!("Could not parse {}", path.display()))
}

fn read(path: &Path, optional: bool) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if optional && error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

fn parse(path: &Path, text: &str) -> Result<Table> {
    toml::from_str(text).with_context(|| format!("Could not parse {}", path.display()))
}

fn merge_file(
    merged: &mut Table,
    path: &Path,
    mut layer: Table,
    depth: usize,
    included_colors: bool,
) -> Result<()> {
    ensure!(
        depth < 16,
        "Config includes are nested too deeply at {}",
        path.display()
    );
    let includes = layer
        .remove("include")
        .map(Includes::deserialize)
        .transpose()
        .with_context(|| format!("Invalid includes in {}", path.display()))?
        .unwrap_or_default();
    // Preserve generated palettes from installations that used the standalone appearance file.
    if depth > 0 {
        let legacy: Table = ["colors", "typography", "layout", "borders"]
            .into_iter()
            .filter_map(|key| layer.remove(key).map(|value| (key.to_owned(), value)))
            .collect();
        if !legacy.is_empty() {
            let mut appearance = legacy;
            if let Some(Value::Table(current)) = layer.remove("appearance") {
                merge(&mut appearance, current);
            }
            layer.insert("appearance".into(), Value::Table(appearance));
        }
    }
    if depth > 0
        && let Some(appearance) = layer.get_mut("appearance").and_then(Value::as_table_mut)
    {
        appearance.remove("use_included_colors");
        if !included_colors {
            appearance.remove("colors");
        }
    }
    merge(merged, layer);
    let parent = path
        .parent()
        .context("Config file has no parent directory")?;
    for (files, optional) in [(includes.files, false), (includes.optional.files, true)] {
        for file in files {
            let included = parent.join(file);
            if let Some(text) = read(&included, optional)? {
                merge_file(
                    merged,
                    &included,
                    parse(&included, &text)?,
                    depth + 1,
                    included_colors,
                )?;
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
    fn moves_flat_settings_without_overwriting_explicit_sections() -> Result<()> {
        let mut config: Table = toml::from_str(
            r#"
appimage_directory = "/old"
move_appimages_by_default = true
browser = "/usr/bin/browser"

[apps.image]
move_directory = "/new"

[extensions]
enabled = true
"#,
        )?;
        assert!(upgrade_apps(&mut config)?);
        assert_eq!(
            config["apps"]["image"]["move_directory"].as_str(),
            Some("/new")
        );
        assert_eq!(
            config["apps"]["image"]["move_to_directory"].as_bool(),
            Some(true)
        );
        assert_eq!(
            config["apps"]["web"]["browser_path"].as_str(),
            Some("/usr/bin/browser")
        );
        assert_eq!(config["apps"]["icons_directory"].as_str(), Some(""));
        assert!(config.contains_key("extensions"));
        assert!(!upgrade_apps(&mut config)?);
        Ok(())
    }

    #[test]
    fn settings_edits_preserve_other_entries_and_comments() -> Result<()> {
        let mut document: toml_edit::DocumentMut = r#"[apps]
icons_directory = "" # Custom icons
plugin = true
"#
        .parse()?;
        let values: Table = toml::from_str(
            r#"icons_directory = "~/Icons"
[image]
move_to_directory = true"#,
        )?;
        update_table(&mut document["apps"], &values)?;
        let saved = document.to_string();
        let parsed: Table = toml::from_str(&saved)?;
        assert!(saved.contains("# Custom icons"));
        assert_eq!(parsed["apps"]["plugin"].as_bool(), Some(true));
        assert_eq!(
            parsed["apps"]["image"]["move_to_directory"].as_bool(),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn local_palette_retains_other_included_settings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            r#"
[include]
files = ["theme.toml"]
[appearance]
use_included_colors = false
"#,
        )?;
        fs::write(
            temp.path().join("theme.toml"),
            r##"
[appearance.colors]
background = "#123456"
[appearance.layout]
spacing = 9.0
"##,
        )?;
        let loaded = load(&path)?;
        assert_eq!(
            loaded["appearance"]["colors"]["background"].as_str(),
            Some("#17191e")
        );
        assert_eq!(
            loaded["appearance"]["layout"]["spacing"].as_float(),
            Some(9.)
        );
        Ok(())
    }
}
