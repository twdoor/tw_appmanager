use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::{
    appimage, config, desktop,
    icons::{self, Icon},
    transaction::Transaction,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[value(name = "appimage", alias = "app-image")]
    AppImage,
    Native,
    #[value(skip)]
    Web,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::AppImage, Self::Native, Self::Web];

    pub fn tag(self) -> &'static str {
        match self {
            Self::AppImage => "appimage",
            Self::Native => "executable",
            Self::Web => "web",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AppImage => "AppImage",
            Self::Native => "Linux program",
            Self::Web => "Web app",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    AppImage {
        path: PathBuf,
        #[serde(default)]
        managed: bool,
    },
    Native {
        path: PathBuf,
    },
    Web {
        url: String,
    },
}

impl Target {
    pub fn kind(&self) -> Kind {
        match self {
            Self::AppImage { .. } => Kind::AppImage,
            Self::Native { .. } => Kind::Native,
            Self::Web { .. } => Kind::Web,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::AppImage { path, .. } | Self::Native { path } => path.display().to_string(),
            Self::Web { url, .. } => url.clone(),
        }
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::AppImage { path, .. } | Self::Native { path } => Some(path),
            Self::Web { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub appimage_version: Option<String>,
    pub icon_override: Option<PathBuf>,
    #[serde(flatten)]
    pub target: Target,
    pub icon: Option<PathBuf>,
    pub icon_directory: Option<PathBuf>,
    pub terminal_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<desktop::AdoptedLauncher>,
}

impl Entry {
    pub fn description(&self) -> Option<&str> {
        self.description
            .as_deref()
            .or_else(|| self.launcher.as_ref()?.comment.as_deref())
    }
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub id: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub source: String,
    pub icon: Option<PathBuf>,
    pub terminal_command: Option<String>,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub move_appimage: bool,
    pub fetch_favicon: bool,
    pub use_default_icon: bool,
}

impl Draft {
    pub fn new(kind: Kind, source: String) -> Self {
        Self {
            id: None,
            kind,
            name: String::new(),
            description: String::new(),
            source,
            icon: None,
            terminal_command: None,
            arguments: Vec::new(),
            working_directory: None,
            move_appimage: false,
            fetch_favicon: true,
            use_default_icon: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub icons_directory: PathBuf,
    pub image: ImageSettings,
    pub web: WebSettings,
    pub executable: ExecutableSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImageSettings {
    pub move_directory: PathBuf,
    pub move_to_directory: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebSettings {
    pub browser_path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExecutableSettings {
    pub working_directory: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Catalog {
    #[serde(default)]
    apps: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub struct Store {
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    applications: PathBuf,
    bin: PathBuf,
    executable: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required")?;
        let config = xdg("XDG_CONFIG_HOME", home.join(".config"));
        let data = xdg("XDG_DATA_HOME", home.join(".local/share"));
        let store = Self {
            bin: home.join(".local/bin"),
            home,
            // Keep existing catalogs, theme includes, and managed files accessible.
            config: config.join("tw-appmanager"),
            data: data.join("tw-appmanager"),
            applications: data.join("applications"),
            executable: env::current_exe()?,
        };
        let path = store.config_path();
        if !path.try_exists()? {
            let _lock = store.lock()?;
            if !path.try_exists()? {
                let mut transaction = Transaction::default();
                config::migrate(&path, &mut transaction)?;
                transaction.commit();
            }
        }
        let needs_upgrade = config::document(&path).is_ok_and(|doc| {
            ["appimage_directory", "move_appimages_by_default", "browser"]
                .iter()
                .any(|key| doc.contains_key(key))
        });
        if needs_upgrade {
            let _lock = store.lock()?;
            let mut transaction = Transaction::default();
            config::upgrade_existing(&path, &mut transaction)?;
            transaction.commit();
        }
        Ok(store)
    }

    pub fn config_path(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn settings(&self) -> Result<Settings> {
        config::load(&self.config_path())?
            .remove("apps")
            .map(Settings::deserialize)
            .transpose()
            .map(|value| value.unwrap_or_default())
            .map_err(Into::into)
    }

    pub fn preferences(&self) -> Result<config::Preferences> {
        config::load(&self.config_path())?
            .remove("ui")
            .map(config::Preferences::deserialize)
            .transpose()
            .map(|value| value.unwrap_or_default())
            .map_err(Into::into)
    }

    pub fn save_preferences(&self, preferences: &config::Preferences) -> Result<()> {
        let _lock = self.lock()?;
        self.edit_config(|document| {
            for (key, value) in toml_edit::ser::to_document(preferences)?.into_table() {
                document["ui"][&key] = value;
            }
            Ok(())
        })
    }

    fn edit_config(
        &self,
        edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
    ) -> Result<()> {
        let path = self.config_path();
        let mut document = config::document(&path)?;
        edit(&mut document)?;
        let mut transaction = Transaction::default();
        transaction.write(&path, document.to_string().as_bytes(), 0o600, true)?;
        transaction.commit();
        Ok(())
    }

    pub fn save_settings(
        &self,
        apps: Option<toml::Table>,
        appearance: Option<toml::Table>,
    ) -> Result<()> {
        if apps.is_none() && appearance.is_none() {
            return Ok(());
        }
        let _lock = self.lock()?;
        if let Some(apps) = &apps {
            let old = self.settings()?;
            let mut updated = toml::Table::try_from(&old)?;
            config::merge(&mut updated, apps.clone());
            let settings = Settings::deserialize(toml::Value::Table(updated))?;
            if settings == old && appearance.is_none() {
                return Ok(());
            }
            for (value, previous, label) in [
                (
                    &settings.icons_directory,
                    &old.icons_directory,
                    "Icons directory",
                ),
                (
                    &settings.image.move_directory,
                    &old.image.move_directory,
                    "AppImage directory",
                ),
            ] {
                if value != previous && !value.as_os_str().is_empty() {
                    let path = self.expand(value)?;
                    ensure!(!path.exists() || path.is_dir(), "{label} must be a folder");
                }
            }
            let working = &settings.executable.working_directory;
            if working != &old.executable.working_directory && !working.as_os_str().is_empty() {
                ensure!(
                    self.expand(working)?.is_dir(),
                    "Working directory does not exist"
                );
            }
            if settings.web.browser_path != old.web.browser_path {
                executable(&self.expand(&settings.web.browser_path)?)
                    .context("Choose an executable browser")?;
            }
        }
        if let Some(appearance) = &appearance {
            let mut updated = config::local_appearance(&self.config_path())?;
            config::merge(&mut updated, appearance.clone());
            crate::appearance::Appearance::from_table(updated)?;
        }
        self.edit_config(|document| {
            if let Some(apps) = apps {
                config::update_table(&mut document["apps"], &apps)?;
            }
            if let Some(appearance) = appearance {
                config::update_table(&mut document["appearance"], &appearance)?;
            }
            Ok(())
        })
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let catalog: Catalog = read_toml(&self.config.join("apps.toml"))?.unwrap_or_default();
        let mut ids = std::collections::HashSet::new();
        let mut commands = std::collections::HashSet::new();
        for entry in &catalog.apps {
            ensure!(
                valid_command(&entry.id) && ids.insert(&entry.id),
                "Invalid or duplicate app ID in apps.toml"
            );
            if let Some(command) = &entry.terminal_command {
                ensure!(
                    valid_command(command) && commands.insert(command),
                    "Invalid or duplicate terminal command in apps.toml"
                );
            }
        }
        let mut desktop_ids = std::collections::HashSet::new();
        for entry in &catalog.apps {
            if let Some(launcher) = &entry.launcher {
                ensure!(
                    valid_command(&launcher.desktop_id)
                        && !launcher.desktop_id.starts_with(desktop::ID)
                        && desktop_ids.insert(&launcher.desktop_id)
                        && entry.target.kind() == Kind::AppImage,
                    "Invalid or duplicate adopted launcher ID in apps.toml"
                );
            }
        }
        Ok(catalog.apps)
    }

    pub fn save(&self, draft: Draft) -> Result<Entry> {
        let _lock = self.lock()?;
        let mut entries = self.entries()?;
        let mut transaction = Transaction::default();
        let entry = self.apply_draft(&mut entries, draft, None, &mut transaction)?;
        self.save_entries(&mut transaction, entries)?;
        transaction.commit();
        self.refresh_desktop_database();
        Ok(entry)
    }

    fn apply_draft(
        &self,
        entries: &mut Vec<Entry>,
        draft: Draft,
        adopted: Option<desktop::AdoptedLauncher>,
        transaction: &mut Transaction,
    ) -> Result<Entry> {
        let previous = draft
            .id
            .as_ref()
            .map(|id| {
                entries
                    .iter()
                    .find(|entry| &entry.id == id)
                    .cloned()
                    .context("App no longer exists")
            })
            .transpose()?;
        if let Some(previous) = &previous {
            ensure!(
                previous.target.kind() == draft.kind,
                "App type cannot be changed"
            );
        }
        let mut launcher =
            adopted.or_else(|| previous.as_ref().and_then(|entry| entry.launcher.clone()));
        if launcher
            .as_ref()
            .is_some_and(|launcher| launcher.executable.is_some())
        {
            ensure!(
                !draft.move_appimage,
                "This app uses a custom launcher tied to its current location"
            );
            if let Some(previous) = &previous {
                ensure!(
                    previous.target.display() == draft.source,
                    "The custom launcher requires the original AppImage path"
                );
            }
        }
        let mut detected_icon = None;
        let mut appimage_version = previous
            .as_ref()
            .and_then(|entry| entry.appimage_version.clone());
        let mut fallback_name;
        let mut pending_move = None;
        let target = match draft.kind {
            Kind::AppImage | Kind::Native => {
                let selected = self.expand(Path::new(&draft.source))?;
                if draft.move_appimage {
                    ensure!(
                        !selected.symlink_metadata()?.is_symlink(),
                        "Select the AppImage file itself to move it"
                    );
                }
                let path = selected
                    .canonicalize()
                    .with_context(|| format!("Could not find {}", draft.source))?;
                let actual_kind = appimage::detect_executable(&path)?;
                ensure!(
                    actual_kind == draft.kind,
                    "Detected file type: {}. Select its category to add it.",
                    actual_kind.label()
                );
                let is_appimage = actual_kind == Kind::AppImage;
                ensure!(
                    !entries.iter().any(|app| Some(&app.id) != draft.id.as_ref()
                        && app.target.path() == Some(path.as_path())),
                    "This file is already in the library"
                );
                fallback_name = file_name(&path);
                if is_appimage {
                    if previous
                        .as_ref()
                        .is_none_or(|entry| entry.target.path() != Some(path.as_path()))
                    {
                        let metadata = appimage::metadata(&path);
                        fallback_name = metadata.name.unwrap_or(fallback_name);
                        detected_icon = metadata.icon;
                        appimage_version = metadata.version;
                    }
                    let managed = previous.as_ref().is_some_and(|entry| {
                        matches!(&entry.target, Target::AppImage { path: old, managed: true } if old == &path)
                    });
                    if draft.move_appimage {
                        let folder = self.expand(&self.settings()?.image.move_directory)?;
                        fs::create_dir_all(&folder)?;
                        let folder = folder.canonicalize()?;
                        let destination =
                            folder.join(path.file_name().context("AppImage has no filename")?);
                        if destination == path {
                            Target::AppImage { path, managed }
                        } else {
                            ensure!(
                                destination
                                    .symlink_metadata()
                                    .is_err_and(|error| error.kind() == ErrorKind::NotFound),
                                "{} is already in use",
                                destination.display()
                            );
                            pending_move = Some((path, destination.clone()));
                            Target::AppImage {
                                path: destination,
                                managed: true,
                            }
                        }
                    } else {
                        Target::AppImage { path, managed }
                    }
                } else {
                    Target::Native { path }
                }
            }
            Kind::Web => {
                let url = parse_url(&draft.source)?;
                fallback_name = url.host_str().unwrap_or_default().to_owned();
                if draft.icon.is_none()
                    && draft.fetch_favicon
                    && !draft.use_default_icon
                    && previous
                        .as_ref()
                        .is_none_or(|entry| entry.target.display() != url.as_str())
                {
                    detected_icon = icons::favicon(&url);
                }
                Target::Web { url: url.into() }
            }
        };

        let name = if draft.name.trim().is_empty() {
            fallback_name
        } else {
            draft.name.trim().into()
        };
        ensure!(
            !name.trim().is_empty() && !name.chars().any(char::is_control),
            "Enter an app name without control characters"
        );
        let id = previous
            .as_ref()
            .map(|entry| entry.id.clone())
            .unwrap_or_else(|| {
                let slug = slug(&name);
                available_id(&slug, entries, &self.applications)
            });
        let terminal_command = draft.terminal_command.filter(|command| !command.is_empty());
        if let Some(command) = &terminal_command {
            ensure!(
                valid_command(command),
                "Use letters, digits, dots, underscores, or hyphens for the terminal command"
            );
            ensure!(
                !entries.iter().any(|entry| entry.id != id && entry.terminal_command.as_ref() == Some(command)),
                "Terminal command '{command}' belongs to another app"
            );
        }

        let working_directory = draft
            .working_directory
            .map(|path| self.expand(&path))
            .transpose()?;
        if let Some(path) = &working_directory {
            ensure!(path.is_dir(), "Working directory does not exist");
        }

        let supplied_icon = draft
            .icon
            .as_deref()
            .map(|path| self.expand(path))
            .transpose()?;
        let selected_icon = supplied_icon
            .as_ref()
            .filter(|_| !draft.use_default_icon)
            .filter(|path| {
                previous
                    .as_ref()
                    .and_then(|entry| entry.icon_override.as_ref())
                    != Some(path)
            })
            .map(|path| Icon::open(path))
            .transpose()?;
        let old_icon = previous.as_ref().and_then(|entry| entry.icon.as_ref());
        let (icon, icon_directory) = if draft.use_default_icon {
            (None, None)
        } else if let Some(icon) = selected_icon.or(detected_icon) {
            let configured = self.settings()?.icons_directory;
            let directory = if configured.as_os_str().is_empty() {
                self.data.join("icons")
            } else {
                self.expand(&configured)?
            };
            let path = directory.join(format!("{id}.{}", icon.extension));
            transaction.write(&path, &icon.bytes, 0o644, old_icon == Some(&path))?;
            (Some(path), Some(directory))
        } else {
            (
                old_icon.cloned(),
                previous
                    .as_ref()
                    .and_then(|entry| entry.icon_directory.clone()),
            )
        };

        if let Some(launcher) = &mut launcher {
            launcher.comment = None;
        }
        let description =
            (!draft.description.trim().is_empty()).then(|| draft.description.trim().to_owned());
        let icon_override = if draft.use_default_icon {
            None
        } else {
            supplied_icon.or_else(|| {
                previous
                    .as_ref()
                    .and_then(|entry| entry.icon_override.clone())
            })
        };
        let entry = Entry {
            id,
            name,
            description,
            appimage_version,
            icon_override,
            target,
            icon,
            icon_directory,
            terminal_command,
            arguments: draft.arguments,
            working_directory,
            launcher,
        };

        self.write_launcher(transaction, &entry)?;
        self.update_command(transaction, previous.as_ref(), &entry)?;
        if let Some(previous) = &previous
            && previous.icon != entry.icon
        {
            self.remove_icon(transaction, previous)?;
        }
        if let Some((source, destination)) = pending_move {
            let mode = source.metadata()?.permissions().mode();
            transaction
                .copy(&source, &destination, (mode & 0o777) | 0o100)
                .with_context(|| format!("Could not move AppImage to {}", destination.display()))?;
            transaction.remove(&source)?;
        } else if let Some(path) = entry.target.path() {
            let permissions = path.metadata()?.permissions();
            if permissions.mode() & 0o100 == 0 {
                transaction
                    .set_mode(path, permissions.mode() | 0o100)
                    .with_context(|| format!("Could not make {} executable", path.display()))?;
            }
        }
        if let Some(old) = entries.iter_mut().find(|old| old.id == entry.id) {
            *old = entry.clone();
        } else {
            entries.push(entry.clone());
        }
        Ok(entry)
    }

    pub fn remove(&self, id: &str, delete_appimage: bool) -> Result<String> {
        let _lock = self.lock()?;
        let mut entries = self.entries()?;
        let index = entries
            .iter()
            .position(|entry| entry.id == id)
            .context("App no longer exists")?;
        let entry = entries.remove(index);
        let mut transaction = Transaction::default();
        if delete_appimage {
            let Target::AppImage {
                path,
                managed: true,
            } = &entry.target
            else {
                bail!("Only an AppImage moved by the manager can be deleted here");
            };
            transaction.remove(path)?;
        }
        remove_owned(
            &mut transaction,
            &self.launcher_path(&entry),
            &format!("X-TwAppManager-Id={}", entry.id),
        )?;
        if let Some(command) = &entry.terminal_command {
            remove_owned(
                &mut transaction,
                &self.bin.join(command),
                &command_marker(&entry.id),
            )?;
        }
        self.remove_icon(&mut transaction, &entry)?;
        self.save_entries(&mut transaction, entries)?;
        transaction.commit();
        self.refresh_desktop_database();
        Ok(entry.name)
    }

    pub fn install_desktop(&self) -> Result<PathBuf> {
        let _lock = self.lock()?;
        let defaults = desktop::current_appimage_defaults();
        let mut transaction = Transaction::default();
        let path = self.applications.join(format!("{}.desktop", desktop::ID));
        write_owned(
            &mut transaction,
            &path,
            &desktop::manager_entry(&self.executable)?,
            &format!("X-TwAppManager-Id={}", desktop::ID),
            0o644,
        )?;
        let icon = self
            .applications
            .with_file_name("icons")
            .join("hicolor/scalable/apps")
            .join(format!("{}.svg", desktop::ID));
        transaction.write(&icon, desktop::ICON, 0o644, icon.try_exists()?)?;
        // Reconnect managed launchers if the manager executable has moved.
        for entry in self.entries()? {
            self.write_launcher(&mut transaction, &entry)?;
            self.update_command(&mut transaction, Some(&entry), &entry)?;
        }
        transaction.commit();
        self.refresh_desktop_database();
        desktop::restore_appimage_defaults(defaults);
        Ok(path)
    }

    pub fn adopt_gear_lever(&self, desktop_ids: &[String]) -> Result<Vec<Entry>> {
        let _lock = self.lock()?;
        let mut entries = self.entries()?;
        let mut transaction = Transaction::default();
        let mut adopted = Vec::new();
        for filename in desktop_ids {
            let id = filename.strip_suffix(".desktop").unwrap_or(filename);
            ensure!(
                valid_command(id) && !id.starts_with(desktop::ID),
                "Invalid desktop ID: {filename}"
            );
            let original = self.applications.join(format!("{id}.desktop"));
            ensure!(
                original.symlink_metadata()?.is_file(),
                "Expected a regular desktop file"
            );
            let content = fs::read_to_string(&original)?;
            let old = desktop::GearLeverEntry::parse(id, &content)
                .with_context(|| format!("Could not import {filename}"))?;
            if let Some(wrapper) = &old.launcher.executable {
                executable(wrapper)?;
            }
            let backup = self
                .data
                .join("gear-lever-launchers")
                .join(format!("{id}.desktop"));
            transaction.write(&backup, content.as_bytes(), 0o644, false)?;
            transaction.remove(&original)?;
            let mut draft = Draft::new(
                Kind::AppImage,
                old.source
                    .to_str()
                    .context("AppImage path must be UTF-8")?
                    .to_owned(),
            );
            draft.name = old.name;
            draft.description = old.launcher.comment.clone().unwrap_or_default();
            draft.icon = old.icon;
            draft.arguments = old.arguments;
            draft.working_directory = old.working_directory;
            adopted.push(self.apply_draft(
                &mut entries,
                draft,
                Some(old.launcher),
                &mut transaction,
            )?);
        }
        self.save_entries(&mut transaction, entries)?;
        transaction.commit();
        self.refresh_desktop_database();
        Ok(adopted)
    }

    pub fn launch(
        &self,
        id: &str,
        extra: &[std::ffi::OsString],
        replace_process: bool,
    ) -> Result<()> {
        let lock = self.lock()?;
        let entry = self
            .entries()?
            .into_iter()
            .find(|entry| entry.id == id)
            .with_context(|| format!("No app with ID '{id}'"))?;
        let mut command = match &entry.target {
            Target::Web { url, .. } => {
                let browser = self.expand(&self.settings()?.web.browser_path)?;
                executable(&browser).context("Set an executable web app browser in Settings")?;
                let url = parse_url(url)?;
                let mut command = Command::new(browser);
                command
                    .arg(format!("--app={url}"))
                    .arg(format!("--class={}", entry.id));
                command
            }
            Target::Native { path } | Target::AppImage { path, .. } => {
                executable(path)?;
                if let Some(launcher) = &entry.launcher {
                    let program = launcher.executable.as_deref().unwrap_or(path);
                    executable(program)?;
                    let mut command = Command::new(program);
                    command.envs(&launcher.environment);
                    command
                } else {
                    Command::new(path)
                }
            }
        };
        let native_default =
            if entry.working_directory.is_none() && entry.target.kind() == Kind::Native {
                let path = self.settings()?.executable.working_directory;
                (!path.as_os_str().is_empty())
                    .then(|| self.expand(&path))
                    .transpose()?
            } else {
                None
            };
        if let Some(directory) = entry
            .working_directory
            .as_deref()
            .or(native_default.as_deref())
            .or_else(|| entry.target.path().and_then(Path::parent))
        {
            command.current_dir(directory);
        }
        command.args(&entry.arguments).args(extra);
        drop(lock);
        if replace_process {
            return Err(command.exec()).with_context(|| format!("Could not launch {}", entry.name));
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Could not launch {}", entry.name))?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    pub fn expand(&self, path: &Path) -> Result<PathBuf> {
        ensure!(!path.as_os_str().is_empty(), "Choose a path");
        if let Ok(relative) = path.strip_prefix("~") {
            return Ok(self.home.join(relative));
        }
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        Ok(env::current_dir()?.join(path))
    }

    fn lock(&self) -> Result<File> {
        fs::create_dir_all(&self.config)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.config.join("library.lock"))?;
        file.lock()?;
        Ok(file)
    }

    fn launcher_path(&self, app: &Entry) -> PathBuf {
        let id = app
            .launcher
            .as_ref()
            .map(|launcher| launcher.desktop_id.clone())
            .unwrap_or_else(|| format!("{}.{}", desktop::ID, app.id));
        self.applications.join(format!("{id}.desktop"))
    }

    fn write_launcher(&self, transaction: &mut Transaction, app: &Entry) -> Result<()> {
        let mut exec = desktop::exec(&self.executable, &["launch", &app.id])?;
        if let Some(code) = app
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.field_code.as_deref())
        {
            ensure!(
                matches!(code, "%f" | "%F" | "%u" | "%U"),
                "Invalid desktop field code"
            );
            exec.push_str(&format!(" -- {code}"));
        }
        let (category, fallback) = match app.target.kind() {
            Kind::Web => ("Network;WebBrowser;", "web-browser"),
            _ => ("Utility;", "application-x-executable"),
        };
        let icon = match &app.icon {
            Some(path) => path.to_str().context("Icon path must be UTF-8")?,
            None => fallback,
        };
        let marker = format!("X-TwAppManager-Id={}", app.id);
        let mut content = format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec={exec}\nIcon={}\n\
             Terminal=false\n{marker}\n",
            desktop::text(&app.name),
            desktop::text(icon)
        );
        if let Some(description) = app.description() {
            content.push_str(&format!(
                "Comment={}\n",
                desktop::text(
                    &description
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            ));
        }
        let kind = app.target.kind();
        let tag = kind.tag();
        content.push_str(&format!("Keywords={tag};\nX-TwAppManager-Type={tag}\n"));
        if kind == Kind::AppImage {
            let version = app.launcher.as_ref().and_then(|launcher| {
                let path = self
                    .data
                    .join("gear-lever-launchers")
                    .join(format!("{}.desktop", launcher.desktop_id));
                fs::read_to_string(path).ok()?.lines().find_map(|line| {
                    line.strip_prefix("X-AppImage-Version=")
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                })
            });
            content.push_str(&format!(
                "X-AppImage-Version={}\n",
                desktop::text(
                    app.appimage_version
                        .as_deref()
                        .or(version.as_deref())
                        .unwrap_or("unversioned")
                )
            ));
        }
        if let Some(launcher) = &app.launcher {
            for (key, value) in [
                (
                    "Categories",
                    launcher.categories.as_deref().or(Some(category)),
                ),
                ("MimeType", launcher.mime_types.as_deref()),
                ("StartupWMClass", launcher.window_class.as_deref()),
            ] {
                if let Some(value) = value {
                    content.push_str(&format!("{key}={}\n", desktop::text(value)));
                }
            }
        } else {
            content.push_str(&format!("Categories={category}\n"));
        }
        if app.target.kind() == Kind::Web {
            content.push_str(&format!("StartupWMClass={}\n", app.id));
        }
        write_owned(
            transaction,
            &self.launcher_path(app),
            &content,
            &marker,
            0o644,
        )
    }

    fn update_command(
        &self,
        transaction: &mut Transaction,
        old: Option<&Entry>,
        entry: &Entry,
    ) -> Result<()> {
        let marker = command_marker(&entry.id);
        let previous = old.and_then(|entry| entry.terminal_command.as_ref());
        if let Some(command) = &entry.terminal_command {
            let path = self.bin.join(command);
            let replacing = previous == Some(command) && owned(&path, &marker)?;
            if !replacing {
                let in_path = env::var_os("PATH")
                    .into_iter()
                    .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
                    .any(|dir| executable(&dir.join(command)).is_ok());
                ensure!(
                    !in_path,
                    "Terminal command '{command}' already exists on PATH"
                );
            }
            let executable = self
                .executable
                .to_str()
                .context("Manager executable path must be UTF-8")?;
            let script = format!(
                "#!/bin/sh\n{marker}\nexec {} launch {} -- \"$@\"\n",
                shell_words::quote(executable),
                shell_words::quote(&entry.id)
            );
            write_owned(transaction, &path, &script, &marker, 0o755)?;
        }
        if let Some(command) = previous.filter(|old| entry.terminal_command.as_ref() != Some(*old))
        {
            remove_owned(transaction, &self.bin.join(command), &marker)?;
        }
        Ok(())
    }

    fn remove_icon(&self, transaction: &mut Transaction, entry: &Entry) -> Result<()> {
        let Some(icon) = &entry.icon else {
            return Ok(());
        };
        let directory = entry
            .icon_directory
            .clone()
            .unwrap_or_else(|| self.data.join("icons"));
        if ["png", "svg"]
            .into_iter()
            .any(|ext| directory.join(format!("{}.{ext}", entry.id)) == *icon)
        {
            transaction.remove(icon)?;
        }
        Ok(())
    }

    fn save_entries(&self, transaction: &mut Transaction, mut entries: Vec<Entry>) -> Result<()> {
        entries.sort_by_cached_key(|app| app.name.to_lowercase());
        write_toml(
            transaction,
            &self.config.join("apps.toml"),
            &Catalog { apps: entries },
        )
    }

    fn refresh_desktop_database(&self) {
        if let Ok(output) = Command::new("update-desktop-database")
            .arg(&self.applications)
            .output()
            && !output.status.success()
        {
            eprintln!(
                "Could not refresh the desktop database: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
}

fn parse_url(value: &str) -> Result<Url> {
    let trimmed = value.trim();
    ensure!(!trimmed.is_empty(), "Enter a website URL");
    let url = if trimmed.contains("://") {
        Url::parse(trimmed)?
    } else {
        Url::parse(&format!("https://{trimmed}"))?
    };
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.has_host(),
        "Enter an HTTP or HTTPS website URL"
    );
    Ok(url)
}

fn command_marker(id: &str) -> String {
    // Keep recognizing scripts generated before the rename.
    format!("# TW App Manager: {id}")
}

fn owned(path: &Path, marker: &str) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && metadata.len() <= 128 * 1024,
                "{} is already in use",
                path.display()
            );
            let contents = fs::read_to_string(path)
                .with_context(|| format!("Could not read {}", path.display()))?;
            ensure!(
                contents.lines().any(|line| line == marker),
                "{} is already in use",
                path.display()
            );
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn write_owned(
    transaction: &mut Transaction,
    path: &Path,
    content: &str,
    marker: &str,
    mode: u32,
) -> Result<()> {
    let replace = owned(path, marker)?;
    transaction
        .write(path, content.as_bytes(), mode, replace)
        .with_context(|| format!("Could not write {}", path.display()))
}

fn remove_owned(transaction: &mut Transaction, path: &Path, marker: &str) -> Result<()> {
    if owned(path, marker)? {
        transaction.remove(path)?;
    }
    Ok(())
}

fn executable(path: &Path) -> Result<()> {
    let metadata = path
        .metadata()
        .with_context(|| format!("Could not find {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        "{} is not executable",
        path.display()
    );
    Ok(())
}

fn valid_command(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .take(72)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "app".to_owned()
    } else {
        slug.to_owned()
    }
}

fn available_id(slug: &str, entries: &[Entry], applications: &Path) -> String {
    let in_use = |id: &str| {
        entries.iter().any(|entry| entry.id == id)
            || applications
                .join(format!("{}.{id}.desktop", desktop::ID))
                .symlink_metadata()
                .is_ok()
    };
    if !in_use(slug) {
        return slug.to_owned();
    }
    (2..)
        .map(|number| format!("{slug}-{number}"))
        .find(|id| !in_use(id))
        .expect("the sequence contains a free app ID")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default()
        .into_owned()
}

fn xdg(key: &str, default: PathBuf) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or(default)
}

fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content)
            .map(Some)
            .with_context(|| format!("Could not parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

fn write_toml<T: Serialize>(transaction: &mut Transaction, path: &Path, data: &T) -> Result<()> {
    transaction
        .write(path, toml::to_string_pretty(data)?.as_bytes(), 0o600, true)
        .with_context(|| format!("Could not save {}", path.display()))
}
