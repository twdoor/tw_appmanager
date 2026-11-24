mod appearance;
mod appimage;
mod desktop;
mod icons;
mod library;
mod transaction;
mod ui;

use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand};

use crate::library::{Draft, Kind, Store};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Open the GUI and review one or more AppImages or x86-64 executables for import.
    #[arg(long, value_name = "FILE", num_args = 0..)]
    open: Vec<PathBuf>,

    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Register the app and AppImage Open With handler for the current user.
    InstallDesktop,

    /// Add an AppImage, x86-64 executable, or web app.
    Add(AddArgs),

    /// Import Gear Lever entries by desktop ID and take over their existing launchers.
    AdoptGearLever {
        #[arg(required = true, value_name = "DESKTOP_ID")]
        desktop_ids: Vec<String>,
    },

    /// List managed apps and their launch IDs.
    List,

    /// Launch an app from its ID and forward any extra arguments.
    Launch {
        id: String,
        #[arg(last = true, value_name = "ARG")]
        arguments: Vec<OsString>,
    },

    /// Remove an entry and its generated launchers.
    Remove {
        id: String,
        /// Also delete an AppImage previously moved by the manager.
        #[arg(long)]
        delete_appimage: bool,
    },

    /// Create, list, remove, or import web apps.
    Webapp(WebArgs),
}

#[derive(Args)]
struct AddArgs {
    #[arg(value_enum)]
    kind: Kind,
    /// Executable path or website URL.
    source: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    icon: Option<PathBuf>,
    #[arg(long)]
    terminal_command: Option<String>,
    #[arg(long = "move", conflicts_with = "keep_in_place")]
    move_appimage: bool,
    #[arg(long)]
    keep_in_place: bool,
    #[arg(long)]
    no_favicon: bool,
    #[arg(long)]
    working_directory: Option<PathBuf>,
    #[arg(last = true, value_name = "ARG")]
    arguments: Vec<String>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("operation").required(true).args(["install", "remove", "list", "import"])))]
struct WebArgs {
    #[arg(short = 'i', long, value_name = "NAME")]
    install: Option<String>,
    #[arg(short = 'r', long, value_name = "ID_OR_NAME")]
    remove: Option<String>,
    #[arg(short = 'l', long)]
    list: bool,
    #[arg(long)]
    import: bool,
    #[arg(long, requires = "install")]
    url: Option<String>,
    #[arg(long, requires = "install")]
    no_favicon: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("TW App Manager: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let store = Store::open()?;
    match cli.command {
        None => ui::run(store, cli.open)?,
        Some(Action::InstallDesktop) => println!("{}", store.install_desktop()?.display()),
        Some(Action::Add(args)) => {
            anyhow::ensure!(
                !args.move_appimage || args.kind == Kind::AppImage,
                "--move is only available for AppImages"
            );
            let mut draft = Draft::new(args.kind, args.source);
            draft.name = args.name.unwrap_or_default();
            draft.icon = args.icon;
            draft.terminal_command = args.terminal_command;
            draft.working_directory = args.working_directory;
            draft.arguments = args.arguments;
            draft.move_appimage = args.move_appimage
                || (args.kind == Kind::AppImage
                    && !args.keep_in_place
                    && store.settings()?.move_appimages_by_default);
            draft.fetch_favicon = !args.no_favicon;
            println!("Added {}", store.save(draft)?.id);
        }
        Some(Action::AdoptGearLever { desktop_ids }) => {
            for entry in store.adopt_gear_lever(&desktop_ids)? {
                println!("Imported {}", entry.name);
            }
        }
        Some(Action::List) => print_entries(&store, None)?,
        Some(Action::Launch { id, arguments }) => store.launch(&id, &arguments, true)?,
        Some(Action::Remove {
            id,
            delete_appimage,
        }) => println!("Removed {}", store.remove(&id, delete_appimage)?),
        Some(Action::Webapp(args)) => {
            if let Some(name) = args.install {
                let url = match args.url {
                    Some(url) => url,
                    None => {
                        print!("Enter URL for '{name}': ");
                        io::stdout().flush()?;
                        let mut url = String::new();
                        io::stdin().read_line(&mut url)?;
                        url
                    }
                };
                let mut draft = Draft::new(Kind::Web, url);
                draft.name = name;
                draft.fetch_favicon = !args.no_favicon;
                println!("Added {}", store.save(draft)?.id);
            } else if let Some(name) = args.remove {
                let entries = store.entries()?;
                let mut matches = entries.iter().filter(|entry| {
                    entry.target.kind() == Kind::Web && entry.name.eq_ignore_ascii_case(&name)
                });
                let entry = entries
                    .iter()
                    .find(|entry| entry.target.kind() == Kind::Web && entry.id == name)
                    .or_else(|| matches.next())
                    .context("Web app not found")?;
                anyhow::ensure!(
                    entry.id == name || matches.next().is_none(),
                    "More than one web app has this name. Use its ID from the web app list."
                );
                println!("Removed {}", store.remove(&entry.id, false)?);
            } else if args.import {
                println!("Imported {} web app(s)", store.import_legacy_webapps()?);
            } else {
                print_entries(&store, Some(Kind::Web))?;
            }
        }
    }
    Ok(())
}

fn print_entries(store: &Store, kind: Option<Kind>) -> Result<()> {
    for app in store.entries()? {
        if kind.is_none_or(|kind| app.target.kind() == kind) {
            println!(
                "{}\t{}\t{}\t{}",
                app.id,
                app.target.kind().label(),
                app.name,
                app.target.display()
            );
        }
    }
    Ok(())
}
