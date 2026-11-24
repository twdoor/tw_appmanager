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

pub fn migrate(
    path: &Path,
    defaults: &impl Serialize,
    transaction: &mut Transaction,
) -> Result<()> {
    let directory = path.parent().context("Missing config directory")?;
    let legacy_settings = directory.join("settings.toml");
    let legacy_appearance = directory.join("appearance.toml");
    let mut root = Table::try_from(defaults)?;
    let settings = read(&legacy_settings, true)?;
    if let Some(text) = &settings {
        merge(&mut root, parse(&legacy_settings, text)?);
    }
    let mut document: toml_edit::DocumentMut =
        format!("{}\n{DEFAULT_CONFIG}", toml::to_string(&root)?).parse()?;
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

fn table_item(values: &Table) -> Result<toml_edit::Item> {
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

pub fn load(path: &Path) -> Result<Table> {
    let mut merged = parse(path, DEFAULT_CONFIG)?;
    let root = read(path, false)?.context("Missing config")?;
    merge_file(&mut merged, path, &root, 0)?;
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

fn merge_file(merged: &mut Table, path: &Path, text: &str, depth: usize) -> Result<()> {
    ensure!(
        depth < 16,
        "Config includes are nested too deeply at {}",
        path.display()
    );
    let mut layer = parse(path, text)?;
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
    merge(merged, layer);
    let parent = path
        .parent()
        .context("Config file has no parent directory")?;
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
