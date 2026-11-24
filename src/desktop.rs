use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

pub const ID: &str = "io.github.twdoor.TwAppManager";
pub const APPIMAGE_TYPES: &str =
    "application/vnd.appimage;application/x-iso9660-appimage;application/x-appimage;";

pub fn text(value: &str) -> String {
    let content = value.trim_matches(' ');
    let leading = value.len() - value.trim_start_matches(' ').len();
    let trailing = value.len() - leading - content.len();
    let escaped = content
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!(
        "{}{escaped}{}",
        "\\s".repeat(leading),
        "\\s".repeat(trailing)
    )
}

fn exec_argument(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        return argument.to_owned();
    }
    let argument = argument.replace('%', "%%");
    let mut result = String::from("\"");
    for character in argument.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            result.push('\\');
        }
        result.push(character);
    }
    result.push('"');
    // Exec quoting is interpreted after the desktop-entry string escaping.
    result.replace('\\', "\\\\")
}

pub fn exec(executable: &Path, arguments: &[&str]) -> Result<String> {
    let executable = executable.to_str().context("Launcher path must be UTF-8")?;
    anyhow::ensure!(
        !executable.contains(['=', '\n', '\r', '\t']),
        "Launcher path contains characters unsupported by .desktop files"
    );
    let mut words = Vec::new();
    // GIO checks the first word before expanding literal percent signs.
    if executable.contains('%') {
        words.extend(["env".into(), "--".into()]);
    }
    words.push(exec_argument(executable));
    words.extend(arguments.iter().map(|argument| exec_argument(argument)));
    Ok(words.join(" "))
}

pub fn manager_entry(executable: &Path) -> Result<String> {
    let command = exec(executable, &["--open"])?;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=TW App Manager\n\
         Comment=Manage AppImages, native programs, and web apps\n\
         Exec={command} %F\nIcon=application-x-executable\nTerminal=false\n\
         Categories=Utility;\nMimeType={APPIMAGE_TYPES}\nX-TwAppManager-Id={ID}\n"
    ))
}

pub fn current_appimage_defaults() -> Vec<(&'static str, String)> {
    APPIMAGE_TYPES
        .split_terminator(';')
        .filter_map(|mime| default_handler(mime).map(|id| (mime, id)))
        .collect()
}

pub fn restore_appimage_defaults(defaults: Vec<(&str, String)>) {
    // Registering a new handler can change the fallback when there is no saved preference.
    for (mime, id) in defaults {
        if default_handler(mime).as_deref() == Some(&id) {
            continue;
        }
        let restored = Command::new("gio")
            .args(["mime", mime, &id])
            .output()
            .is_ok_and(|output| output.status.success());
        if !restored {
            eprintln!("Could not preserve {id} as the default for {mime}");
        }
    }
}

fn default_handler(mime: &str) -> Option<String> {
    let output = Command::new("gio")
        .env("LC_ALL", "C")
        .args(["mime", mime])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = std::str::from_utf8(&output.stdout).ok()?;
    let first = stdout.lines().next()?;
    if !first.starts_with("Default application ") {
        return None;
    }
    let (_, id) = first.rsplit_once(": ")?;
    id.ends_with(".desktop").then(|| id.to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdoptedLauncher {
    pub desktop_id: String,
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    pub comment: Option<String>,
    pub categories: Option<String>,
    pub mime_types: Option<String>,
    pub window_class: Option<String>,
    pub field_code: Option<String>,
}

pub struct GearLeverEntry {
    pub name: String,
    pub source: PathBuf,
    pub icon: Option<PathBuf>,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub launcher: AdoptedLauncher,
}

impl GearLeverEntry {
    pub fn parse(id: &str, content: &str) -> Result<Self> {
        let fields: BTreeMap<_, _> = content
            .lines()
            .skip_while(|line| *line != "[Desktop Entry]")
            .skip(1)
            .take_while(|line| !line.starts_with('['))
            .filter_map(|line| line.split_once('='))
            .collect();
        let get = |key| fields.get(key).copied().filter(|value| !value.is_empty());
        ensure!(
            get("X-AppImage-Name").is_some(),
            "Expected a Gear Lever AppImage launcher"
        );
        ensure!(
            get("Terminal") != Some("true"),
            "Terminal launchers require manual import"
        );
        let name = get("Name").context("Missing desktop Name")?.to_owned();
        let source = PathBuf::from(get("TryExec").context("Missing AppImage path")?);
        ensure!(source.is_absolute(), "Expected an absolute AppImage path");
        let command = shell_words::split(get("Exec").context("Missing desktop Exec")?)?;
        let mut command = command.into_iter().peekable();
        let mut environment = BTreeMap::new();
        if command
            .peek()
            .is_some_and(|part| part == "env" || part == "/usr/bin/env")
        {
            command.next();
            while let Some((key, value)) = command.peek().and_then(|part| part.split_once('=')) {
                ensure!(
                    !key.is_empty()
                        && key
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                    "Unsupported environment assignment"
                );
                environment.insert(key.to_owned(), value.to_owned());
                command.next();
            }
        }
        let executable = PathBuf::from(command.next().context("Missing launcher executable")?);
        ensure!(
            executable.is_absolute(),
            "Expected an absolute launcher executable"
        );
        let executable = (executable != source).then_some(executable);
        let mut arguments = Vec::new();
        let mut field_code = None;
        for argument in command {
            ensure!(
                field_code.is_none(),
                "Expected desktop field code at the end of Exec"
            );
            if matches!(argument.as_str(), "%f" | "%F" | "%u" | "%U") {
                field_code = Some(argument);
            } else {
                ensure!(
                    !argument.replace("%%", "").contains('%'),
                    "Unsupported desktop Exec argument"
                );
                arguments.push(argument.replace("%%", "%"));
            }
        }
        Ok(Self {
            name,
            source,
            icon: get("Icon")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            working_directory: get("Path").map(PathBuf::from),
            arguments,
            launcher: AdoptedLauncher {
                desktop_id: id.to_owned(),
                executable,
                environment,
                comment: get("Comment").map(str::to_owned),
                categories: get("Categories")
                    .filter(|value| *value != ";")
                    .map(str::to_owned),
                mime_types: get("MimeType").map(str::to_owned),
                window_class: get("StartupWMClass").map(str::to_owned),
                field_code,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn desktop_text_keeps_internal_spaces_literal() {
        assert_eq!(text("T3 Code (Alpha)"), "T3 Code (Alpha)");
        assert_eq!(text("  Two  spaces "), r"\s\sTwo  spaces\s");
        assert_eq!(text("  "), r"\s\s");
        assert_eq!(text("A\\B\tC\nD"), r"A\\B\tC\nD");
    }
}
