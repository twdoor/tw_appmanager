# Twck

Twck is a Linux desktop app, built with Rust and [Ely GPUI Components](https://github.com/ELY-Open-Source/Ely-GPUI-Components), for organizing AppImages, standalone x86-64 executables, and websites that open in a browser’s app mode. It creates a desktop launcher for each entry and can give each app a short command you can run from the terminal.

- [Install](#install)
- [Manage apps](#manage-apps)
- [Settings](#settings)
- [Theme sync](#theme-sync)
- [Command-line usage](#command-line-usage)
- [Import from Gear Lever](#import-from-gear-lever)
- [Files and backups](#files-and-backups)

## Install

Build on Linux with Rust 1.95 or later, a C compiler, and the GPUI development libraries. The graphical app needs a Vulkan-capable graphics stack and a desktop portal with a file chooser backend. Install `desktop-file-utils` to update the desktop launcher database.

On Fedora, the build dependencies include `libxkbcommon-devel`, `libxkbcommon-x11-devel`, `libxcb-devel`, `fontconfig-devel`, `freetype-devel`, and `wayland-devel`.

```sh
git clone https://github.com/twdoor/twck.git
cd twck
cargo build --locked
install -Dm755 target/debug/twck "$HOME/.local/bin/twck"
"$HOME/.local/bin/twck" install-desktop
```

Open **Twck** from your application menu. Add `~/.local/bin` to your `PATH` if your shell does not already include it. This makes both `twck` and any per-app terminal commands available.

When upgrading from TW App Manager, the `install-desktop` step above updates the app menu entry, desktop launchers, and terminal shortcuts to use `twck`. Twck reads your existing library and settings from the same directories.

## Manage apps

Choose **Add app**, select an app type, and choose a file or enter a URL. When your file manager offers **Open With → Twck**, you can use it to review a file for import. The manager reads the file header to distinguish an AppImage from an x86-64 Linux executable. You can also use `twck --open` to review several files in sequence.

| App type | Import and launch behavior |
| --- | --- |
| AppImage | Leave the file where it is or move it to your configured AppImages directory. The manager can detect the name and icon in Type 2 AppImages. You can also enter those values yourself. |
| Linux program | Select an x86-64 Linux executable. You can give it an icon, set arguments, and choose its working directory. By default it starts from its own directory. |
| Web app | Enter an HTTP or HTTPS URL. The manager opens it in the configured browser with `--app`. You can provide an icon or allow favicon lookup through Google’s favicon service. |

The **Terminal command** field creates an optional command for the entry, such as `notes`. Arguments passed to it are forwarded to the app. The **Launch arguments** field sets arguments to use every time a local app starts. For a multi-word argument, use quotes, for example `--profile "Work notes"`.

Use the library toolbar to search by name, description, or terminal command and switch between list and grid. Click an entry to edit it, or right-click to launch it. **Remove** in the editor deletes the entry, its menu launcher, and its terminal command. If Twck moved an AppImage into its managed directory, you can also choose to delete the file.

Drag the sidebar edge to change its width. Drag it inward past the labels to collapse it to icons, or use the toggle at the top.

| Shortcut | Action |
| --- | --- |
| `Ctrl+N` | Add an app from the library |
| `Ctrl+F` | Focus library search |
| `Ctrl+B` | Toggle the sidebar |
| `Ctrl+Enter` | Save an app or keep the color preview |
| `Enter` | Leave a single-line editor field, save when the page itself has focus, or confirm a font selection |
| `Enter` or `Shift+Enter` in the description | Insert a line break |
| `Escape` | Close a picker or go back from the editor or settings |

The editor stores the full multiline description. Menu launchers show it as a single-line desktop comment.

## Settings

Open **Settings** from the sidebar. Choices save immediately, and text fields save after a short pause, when you leave the field, or when you press Enter. The full TOML example is in [config/config.toml](config/config.toml).

| Section | What you can set |
| --- | --- |
| General | Destination for newly managed icons with `apps.icons_directory`. An empty path uses the manager’s data directory. |
| AppImages | Destination and default import action with `apps.image.move_directory` and `apps.image.move_to_directory`. You can choose whether to move each AppImage during import. |
| Web apps | Browser executable with `apps.web.browser_path`. The initial path is `/usr/bin/helium`. Choose a browser that supports `--app`. It applies to all managed web apps. |
| Executables | Default working directory with `apps.executable.working_directory`. Leave it empty to start each program in the directory containing its executable. |
| Appearance | Palette, font family, font size, and corner radius. Open the font field and type to filter the available families. **System font** restores the default. Changing **Corner radius** updates panels and controls together. Use `0` for square corners. |

Open a palette swatch to pick saturation and brightness, adjust hue and opacity, or reuse another palette color. Use the format menu to enter HEX, RGB, or HSV values. The copy button copies the current color as hex. The app previews color changes as you pick them. Choose **Use color** to save, or **Cancel** to restore the previous appearance.

The TOML file also lets you set heading size under `[appearance.typography]`, content width and spacing under `[appearance.layout]`, and separate control and app card radii under `[appearance.borders]`. Ely controls and settings panels use filled backgrounds. `appearance.borders.width` sets the keyboard focus outline width for app cards. Set `appearance.borders.window_width` to add an inset window outline. The app reloads appearance edits while it is open and keeps the last working appearance if an edit cannot be loaded.

## Theme sync

`config.toml` supports required files in `[include]` and optional files in `[include.optional]`. Relative paths resolve from the file that declares them. Included settings override the base values in order, with optional includes applied last. Missing optional files are skipped.

By default, the manager looks for an optional `noctalia.toml` beside `config.toml`. To use it with Noctalia, copy the provided [template](config/noctalia.toml.in) and add this entry to Noctalia’s theme template configuration.

```sh
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
install -Dm644 config/noctalia.toml.in "$config_home/noctalia/templates/tw-appmanager.toml"
```

```toml
[theme.templates.user.tw_appmanager]
input_path = "$XDG_CONFIG_HOME/noctalia/templates/tw-appmanager.toml"
output_path = "$XDG_CONFIG_HOME/tw-appmanager/noctalia.toml"
```

The generated file supplies the palette and the manager picks up changes automatically. In **Settings → Appearance**, choose **Follow theme** to use included colors or **Local colors** to use your own palette. Keeping a color from the picker switches to **Local colors**. Other settings supplied by included files still apply.

## Command-line usage

Run `twck --help` for all commands or `twck add --help` for file import options. Create web apps through **Add app** in the GUI. Use `twck list` to find an entry’s ID for `launch` or `remove`.

```sh
twck --open ./Example.AppImage ./tool.x86_64
twck add appimage ./Example.AppImage --keep-in-place --terminal-command example
twck add native ./tool.x86_64 --name "Example Tool" --icon ./tool.svg -- --profile work
twck list
twck launch example -- --help
twck remove example
```

Use `--move` to move an AppImage during import. Pass `--delete-appimage` to `remove` only when you want to delete an AppImage the manager previously moved. Arguments after `--` are saved with `add` and are supplied for that invocation only with `launch`.

## Import from Gear Lever

List the installed entries and find the desktop IDs you want to take over.

```sh
flatpak run it.mijorus.gearlever --list-installed --json
twck adopt-gear-lever example.desktop another.desktop
```

Replace the example IDs with yours. Adoption replaces the existing desktop launcher for each selected entry and preserves its desktop ID, AppImage path, and supported launch settings. Original desktop files are backed up under `tw-appmanager/gear-lever-launchers` in your XDG data directory. Twck takes over the launchers. Gear Lever’s own library metadata is managed separately by Gear Lever.

## Files and backups

| File or directory | Purpose |
| --- | --- |
| `${XDG_CONFIG_HOME:-~/.config}/tw-appmanager/config.toml` | App defaults, appearance, includes, and library view preferences |
| `${XDG_CONFIG_HOME:-~/.config}/tw-appmanager/apps.toml` | Managed app entries |
| `${XDG_DATA_HOME:-~/.local/share}/applications/` | Generated desktop launchers and the manager’s Open With entry |
| `${XDG_DATA_HOME:-~/.local/share}/tw-appmanager/` | Managed icons and Gear Lever launcher backups |
| `~/.local/bin/` | Manager executable and optional terminal launchers |

Back up the config and app catalog along with any moved AppImages and managed icons you want to keep. If you upgrade from a version with `settings.toml` and `appearance.toml`, the manager migrates them to `config.toml` on the first run and saves the old files with a `.bak` suffix.
