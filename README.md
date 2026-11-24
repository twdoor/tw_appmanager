# TW App Manager

A Rust and GPUI desktop app for AppImages, standalone Linux x86-64 executables, and browser-based web apps. Each app gets a menu launcher and can have its own terminal command, icon, and launch settings.

## Build and install on Linux

Install a recent stable Rust toolchain, a C compiler, and the GPUI Linux build dependencies. On Fedora, the relevant development packages include `libxkbcommon-devel`, `libxkbcommon-x11-devel`, `libxcb-devel`, `fontconfig-devel`, `freetype-devel`, and `wayland-devel`. Install `desktop-file-utils` for desktop database updates. The GUI uses Vulkan and the desktop portal for file pickers.

```sh
cargo build --locked
install -Dm755 target/debug/tw-appmanager ~/.local/bin/tw-appmanager
~/.local/bin/tw-appmanager install-desktop
```

Launch **TW App Manager** from the app menu, or choose it under **Open With** to import a file. TW App Manager detects AppImages and Linux x86-64 executables by inspecting the file header. To use the terminal commands assigned in the GUI, have `~/.local/bin` on your `PATH`. Re-run `install-desktop` if you move the manager executable.

## Managing apps

- **AppImages** can stay in place or move into the folder in **Settings**. The manager reads Type 2 AppImage names and icons directly when available. Type 1 AppImages can be added with a manually chosen name and icon.
- **Linux programs** are standalone x86-64 executables. Pick the executable from its existing folder, optionally provide an icon, and set launch arguments or a working directory if needed.
- **Web apps** open in a browser that supports `--app`. Helium is the initial choice. The browser selected in **Settings** applies to every managed web app. Favicon lookup uses Google's favicon service and can be turned off in the add form or with `--no-favicon`.
- **Existing web apps** can be brought in via **Settings → Import web apps**. The importer reads the original CLI's config and icons. It backs up matching launchers under the manager's data directory and replaces their menu entries with managed launchers.
- **Terminal commands** are optional per app. The launcher forwards extra arguments to the app. Removing an entry removes its menu launcher and terminal command. For AppImages the manager moved, the removal screen also offers to delete the file.

## Terminal examples

```sh
tw-appmanager add appimage ./Example.AppImage --move --terminal-command example
tw-appmanager add native /path/to/tool --name Tool --icon ./tool.png -- --profile work
tw-appmanager webapp -i Calendar --url https://calendar.example.com
tw-appmanager webapp -l
tw-appmanager webapp -r Calendar
tw-appmanager webapp --import
tw-appmanager list
tw-appmanager launch example -- --help
```

## Storage

Settings and the library are stored in `settings.toml` and `apps.toml` under `${XDG_CONFIG_HOME:-~/.config}/tw-appmanager`. Launchers go under `${XDG_DATA_HOME:-~/.local/share}/applications`. Managed icons and legacy launcher backups live under `${XDG_DATA_HOME:-~/.local/share}/tw-appmanager`.

## Appearance and live theme sync

TW App Manager reads `${XDG_CONFIG_HOME:-~/.config}/tw-appmanager/appearance.toml`. The default file is installed by `install-desktop`. Configure the palette, window and control borders, corner radii, typography, spacing, and icon sizes there. Colors accept `#RRGGBB` or `#RRGGBBAA`. Changes apply to open windows, and an invalid edit keeps the last working appearance until corrected.

Like Umbriel, TW App Manager supports `[include]` and `[include.optional]` tables with a `files` array. Relative paths are resolved from the file that lists them. Includes are applied after the local values, with optional includes applied last. By default the app picks up an optional `noctalia.toml` in the same directory. A generated file can override just the palette so local typography and layout settings stay in place.

For Noctalia, copy `config/noctalia.toml.in` to `${XDG_CONFIG_HOME:-~/.config}/noctalia/templates/tw-appmanager.toml` and register it in Noctalia's template configuration.

```toml
[theme.templates.user.tw_appmanager]
input_path = "$XDG_CONFIG_HOME/noctalia/templates/tw-appmanager.toml"
output_path = "$XDG_CONFIG_HOME/tw-appmanager/noctalia.toml"
```

No reload hook is needed. Umbriel controls any compositor-drawn window decorations. The `borders.window_width` setting draws a border inside the app.

## Importing from Gear Lever

Run `flatpak run it.mijorus.gearlever --list-installed` to see Gear Lever’s library. Pass the desktop IDs from `--list-installed --json` to `tw-appmanager adopt-gear-lever`. The import keeps each AppImage in place, retains its desktop ID and supported launch options, and backs up the original launchers under `${XDG_DATA_HOME:-~/.local/share}/tw-appmanager/gear-lever-launchers`.
